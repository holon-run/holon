use super::*;

#[test]
fn oversized_malformed_intent_rejects_at_scalar_budget_before_any_json_parse() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "bad-oversized-intent",
            TaskStopSignal::Cancel,
            true,
        )
    })?;
    let error=db.transaction(|tx|{
        tx.execute("UPDATE task_stop_effects SET intent_json=?1 WHERE command_nonce='bad-oversized-intent'",[format!("{{{}","x".repeat(65536))])?;
        load_pending_task_stops_tx(tx,&scope(),100,4096,&ledger)
    }).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ResourceExhausted before decode"),
        "{error:#}"
    );
    assert!(!error.to_string().contains("malformed JSON"));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn callback_material_guard_precedes_small_malformed_intent_phase_address_parse() -> Result<()> {
    let (_dir, db, ledger) = fixture()?;
    db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "bad-small-intent",
            TaskStopSignal::Cancel,
            true,
        )
    })?;
    let charged = std::cell::Cell::new(false);
    let error=db.transaction(|tx|{
        tx.execute("UPDATE task_stop_effects SET intent_json='bad-json' WHERE command_nonce='bad-small-intent'",[])?;
        observe_task_stops_tx(tx,&scope(),None,true,|bytes,records|{
            charged.set(true);
            assert!(bytes>0 && records==1);
            ledger.register_owned_bytes(&scope(),InputMemoryCategory::Candidate,
                OwnedBytesEstimate{payload_bytes:u64::try_from(bytes)?*8,metadata_estimate_bytes:512}).map_err(Into::into)
        },None)
    }).unwrap_err();
    assert!(
        charged.get(),
        "the actual guard must precede semantic JSON parsing"
    );
    assert!(format!("{error:#}").contains("malformed JSON"));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn frozen_phase_receipt_material_is_incrementally_budgeted_before_its_corrupt_decode() -> Result<()>
{
    let (_dir, db, ledger) = fixture()?;
    let original = db.transaction(|tx| {
        stop(
            tx,
            &ledger,
            "phase-material-original",
            TaskStopSignal::Cancel,
            true,
        )
    })?;
    let error=db.transaction(|tx|{
        // Deliberately corrupt restore. No authentic Result phase is claimed:
        // the purpose is to reject its oversized receipt before JSON validation.
        tx.execute("UPDATE task_stop_effects SET intent_json=json_set(intent_json,'$.result',json_object('phase_nonce','phase-material-corrupt')) WHERE effect_seq=?1",[original.effect_seq])?;
        let phase:String=tx.query_row("SELECT json_extract(intent_json,'$.result.phase_nonce') FROM task_stop_effects WHERE effect_seq=?1",[original.effect_seq],|r|r.get(0))?;
        assert_eq!(phase,"phase-material-corrupt");
        tx.execute("INSERT INTO input_mutation_receipts(command_nonce,family,phase_version,command_digest,scope_digest,scopes_json,prepared_at,prepared_json,outcome_json,committed_at,result_digest) SELECT 'phase-material-corrupt',family,phase_version,command_digest,scope_digest,scopes_json,prepared_at,?1,outcome_json,committed_at,result_digest FROM input_mutation_receipts WHERE command_nonce='phase-material-original'",["bad-json".repeat(65536)])?;
        load_task_stop_by_sequence_tx(tx,&scope(),original.effect_seq,128*1024,&ledger)
    }).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ResourceExhausted before Result receipt decode"),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

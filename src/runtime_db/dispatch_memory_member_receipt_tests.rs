use super::*;
use crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger;

fn policy() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {capture:WorkCaptureRequest::current(),byte_limit:64*1024*1024,record_limit:65536}
}
fn deferred(f:&Fixture) -> DispatchMemoryIntent {
    DispatchMemoryIntent::Deferred {binding:DispatchMemoryBinding::from_start(&f.start),frozen:f.frozen("member-material")}
}

#[test]
fn actual_memory_member_receipt_normalized_raw_precedes_bad_json_and_rollback_keeps_the_original_whole_receipt()->Result<()> {
    let f=Fixture::new(true)?;
    let member=f.update("member-material-raw",1,false)?;
    assert_eq!(member.obligation().context("actual Work member absent")?.members.len(),1);
    let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let before=f.db.connection()?.query_row("SELECT family,outcome_json,result_digest FROM input_mutation_receipts WHERE command_nonce=?1",
        [f.original_start.command_nonce()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?;
    let result:Result<()> = f.db.transaction(|tx| {
        tx.execute("UPDATE input_mutation_receipts SET family=?2,outcome_json='!' WHERE command_nonce=?1",
            params![f.original_start.command_nonce(),"f".repeat(256*1024)])?;
        let mut small=policy();small.byte_limit=512*1024;
        let material=DispatchMemoryTransactionBudget::prepare_tx(tx,&deferred(&f),&small,&ledger)?;
        drop(material);
        anyhow::bail!("unbounded normalized receipt reached the old decoder")
    });
    let error=result.err().context("normalized raw receipt must be refused before JSON")?;
    assert!(matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted)),"{error:#}");
    assert_eq!(ledger.report()?.observed_bytes,0);
    let after=f.db.connection()?.query_row("SELECT family,outcome_json,result_digest FROM input_mutation_receipts WHERE command_nonce=?1",
        [f.original_start.command_nonce()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?;
    assert_eq!(after,before);
    f.db.transaction(|tx|validate_dispatch_memory_members_tx(tx,&member))?;
    Ok(())
}

#[test]
fn actual_memory_member_receipt_complete_native_keys_reject_before_the_original_typed_outcome_and_release_all_guards()->Result<()> {
    let f=Fixture::new(true)?;
    let member=f.update("member-material-native",1,false)?;
    let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    let measured=f.db.transaction(|tx| {
        let material=DispatchMemoryTransactionBudget::prepare_tx(tx,&deferred(&f),&policy(),&ledger)?;
        let measured=material.material_usage_for_test();drop(material);Ok(measured)
    })?;
    assert_eq!(ledger.report()?.observed_bytes,0);
    let dense=serde_json::to_string(&(0..2048).map(|index|(format!("key-{index}"),index)).collect::<BTreeMap<_,_>>())?;
    let result:Result<()> = f.db.transaction(|tx| {
        let old_nodes:usize=tx.query_row("SELECT 20+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.scopes_json))+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.prepared_json))+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.outcome_json)) FROM input_mutation_receipts r WHERE command_nonce=?1",
            [f.original_start.command_nonce()],|row|row.get(0))?;
        tx.execute("UPDATE input_mutation_receipts SET outcome_json=?2 WHERE command_nonce=?1",params![f.original_start.command_nonce(),dense])?;
        let (values,keys):(usize,usize)=tx.query_row("SELECT COUNT(*),COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(?1)",[&dense],|row|Ok((row.get(0)?,row.get(1)?)))?;
        assert_eq!((values,keys),(2049,2048));
        let values_only:usize=tx.query_row("SELECT 20+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.scopes_json))+(SELECT COUNT(*)+COUNT(CASE WHEN typeof(key)='text' THEN 1 END) FROM json_tree(r.prepared_json))+(SELECT COUNT(*) FROM json_tree(r.outcome_json)) FROM input_mutation_receipts r WHERE command_nonce=?1",
            [f.original_start.command_nonce()],|row|row.get(0))?;
        let mut tight=policy();tight.record_limit=measured.1.checked_add(values_only.checked_sub(old_nodes).context("dense values fixture must grow")?
            .checked_mul(6).context("native fixture count overflow")?).context("native fixture limit overflow")?;
        assert!(tight.record_limit<65536);
        let error=DispatchMemoryTransactionBudget::prepare_tx(tx,&deferred(&f),&tight,&ledger)
            .err().context("receipt object keys were not admitted before its typed decoder")?;
        assert!(matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted)),"{error:#}");
        assert_eq!(ledger.report()?.observed_bytes,0);
        let material=DispatchMemoryTransactionBudget::prepare_tx(tx,&deferred(&f),&policy(),&ledger)?;
        assert!(material.material_usage_for_test().1>tight.record_limit);
        let error=validate_dispatch_memory_members_tx(tx,&member).err().context("adequate material must preserve original corrupt outcome rejection")?;
        assert!(!matches!(error.downcast_ref::<DispatchMemoryError>(),Some(DispatchMemoryError::ResourceExhausted)),"{error:#}");
        assert!(ledger.report()?.observed_bytes>0);
        drop(material);assert_eq!(ledger.report()?.observed_bytes,0);
        // Restore the original whole receipt through the transaction rollback.
        Err(error)
    });
    result.err().context("invalid member outcome cannot be committed")?;
    f.db.transaction(|tx|validate_dispatch_memory_members_tx(tx,&member))?;
    assert_eq!(ledger.report()?.observed_bytes,0);
    Ok(())
}

#[test]
fn actual_memory_member_receipts_keep_sequential_maximum_and_validate_legitimate_json_bytes_with_one_live_phase()->Result<()> {
    let f=Fixture::new(true)?;
    let member=f.update("member-material-valid",1,false)?;
    let ledger=InputEvidenceBudgetLedger::new(Default::default())?;
    f.db.transaction(|tx| {
        let count:usize=tx.query_row("SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1 OR command_nonce=?2",
            params![f.original_start.command_nonce(),"member-material-valid"],|row|row.get(0))?;
        assert_eq!(count,2);
        let first=DispatchMemoryTransactionBudget::prepare_tx(tx,&deferred(&f),&policy(),&ledger)?;
        let baseline=first.material_usage_for_test();drop(first);assert_eq!(ledger.report()?.observed_bytes,0);
        let padding=" ".repeat(32*1024);
        tx.execute("UPDATE input_mutation_receipts SET outcome_json=outcome_json||?3 WHERE command_nonce=?1 OR command_nonce=?2",
            params![f.original_start.command_nonce(),"member-material-valid",padding])?;
        let last=DispatchMemoryTransactionBudget::prepare_tx(tx,&deferred(&f),&policy(),&ledger)?;
        assert_eq!(last.material_usage_for_test(),(baseline.0+32*1024*6,baseline.1),
            "sequential member validation must not retain the sum of all receipt body copies");
        validate_dispatch_memory_members_tx(tx,&member)?;
        assert!(ledger.report()?.observed_bytes>0);
        drop(last);assert_eq!(ledger.report()?.observed_bytes,0);
        Ok(())
    })
}

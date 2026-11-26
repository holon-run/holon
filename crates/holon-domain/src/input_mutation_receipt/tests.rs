use super::*;

#[test]
fn canonical_command_digest_preserves_legacy_bytes_and_nested_golden() -> Result<()> {
    fn legacy_canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted = map
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>();
                serde_json::Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key, legacy_canonical(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(legacy_canonical).collect())
            }
            value => value,
        }
    }

    let canonical = r#"{"a":[{"c":[null,true,-7,1.25,18446744073709551615],"z":"雪\n\"\\"},[{"a":"é","b":9}]],"z":{"a":0,"β":"尾"}}"#;
    let reversed = r#"{"z":{"β":"尾","a":0},"a":[{"z":"雪\n\"\\","c":[null,true,-7,1.25,18446744073709551615]},[{"b":9,"a":"é"}]]}"#;
    let golden = "f1b55a38035b127fd878c2fd29b3050ea863d74ce7ce918eff29835c1dab4e55";
    assert_eq!(
        format!("{:x}", Sha256::digest(canonical.as_bytes())),
        golden
    );
    for source in [
        canonical,
        reversed,
        "null",
        "true",
        "[]",
        "{}",
        r#""é\n雪""#,
        "-7",
        "1.25",
        "18446744073709551615",
    ] {
        let value: serde_json::Value = serde_json::from_str(source)?;
        let legacy_bytes = serde_json::to_vec(&legacy_canonical(value.clone()))?;
        assert_eq!(
            canonical_command_digest(&value)?,
            format!("{:x}", Sha256::digest(&legacy_bytes))
        );
        if source == canonical || source == reversed {
            assert_eq!(legacy_bytes, canonical.as_bytes());
            assert_eq!(canonical_command_digest(&value)?, golden);
        }
    }
    let mut reordered: serde_json::Value = serde_json::from_str(reversed)?;
    reordered["a"].as_array_mut().unwrap().reverse();
    assert_ne!(canonical_command_digest(&reordered)?, golden);
    Ok(())
}

fn scope(agent: &str, inc: u64) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: agent.into(),
        identity_incarnation: inc,
    }
}

#[test]
fn descriptor_validates_sha_nonce_unique_nonempty_current_scopes() -> Result<()> {
    let time = "2026-10-13T00:00:00.123456789Z".parse()?;
    for (nonce, scopes, digest) in [
        (" ", vec![scope("agent-a", 1)], "a".repeat(64)),
        ("command", Vec::new(), "a".repeat(64)),
        ("command", vec![scope("agent-a", 0)], "a".repeat(64)),
        ("command", vec![scope("", 1)], "a".repeat(64)),
        (
            "command",
            vec![scope("agent-a", 1), scope("agent-a", 1)],
            "a".repeat(64),
        ),
        (
            "command",
            vec![scope("agent-a", 1)],
            "Debug(command)".into(),
        ),
        ("command", vec![scope("agent-a", 1)], "G".repeat(64)),
    ] {
        assert!(PreparedInputMutation::new(
            InputMutationFamily::Queue,
            nonce.into(),
            scopes,
            digest,
            time
        )
        .is_err());
    }
    let a = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        "a".into(),
        vec![scope("b", 2), scope("a", 1)],
        "a".repeat(64),
        time,
    )?;
    let b = PreparedInputMutation::new(
        InputMutationFamily::Queue,
        "b".into(),
        vec![scope("a", 1), scope("b", 2)],
        "a".repeat(64),
        time,
    )?;
    assert_eq!(a.scopes(), &[scope("a", 1), scope("b", 2)]);
    assert_eq!(
        a.scope_digest(),
        b.scope_digest(),
        "nonce is a separate unique address"
    );
    let changed = PreparedInputMutation::new(
        InputMutationFamily::Terminal,
        "a".into(),
        a.scopes().to_vec(),
        "a".repeat(64),
        time,
    )?;
    assert_ne!(a.scope_digest(), changed.scope_digest());
    Ok(())
}

use super::*;
use crate::schema::EPISODE_CONTINUITY_SCHEMA;
use chrono::{Datelike, NaiveDate};
use holon_domain::{AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility};
use rusqlite::types::{ToSql, ToSqlOutput, ValueRef};

const REASONS: [&str; 3] = [
    "explicit_live_ref",
    "explicit_upgrade_recent_candidate",
    "live_owner",
];

fn identity() -> AgentIdentityRecord {
    AgentIdentityRecord::new(
        "fresh-continuity-timestamp",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
}

fn connection() -> Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(EPISODE_CONTINUITY_SCHEMA)?;
    Ok(connection)
}

fn bound_at(connection: &Connection, incarnation: u64) -> Result<String> {
    Ok(connection.query_row(
        "SELECT bound_at FROM agent_episode_continuity WHERE identity_incarnation = ?1",
        [incarnation],
        |row| row.get(0),
    )?)
}

#[test]
fn fresh_timestamp_sql_bytes_match_original_for_wide_years_fractions_and_leap_seconds() -> Result<()>
{
    let mut connection = connection()?;
    let mut identity = identity();
    let mut values = vec![DateTime::<Utc>::MIN_UTC, DateTime::<Utc>::MAX_UTC];
    for year in [
        DateTime::<Utc>::MIN_UTC.year(),
        -10000,
        -1,
        0,
        9999,
        10000,
        DateTime::<Utc>::MAX_UTC.year(),
    ] {
        for nanos in [
            0,
            1_000_000,
            1_000,
            1,
            123_000_000,
            123_456_000,
            123_456_789,
            1_000_000_000,
            1_123_456_789,
        ] {
            values.push(
                NaiveDate::from_ymd_opt(year, 10, 17)
                    .unwrap()
                    .and_hms_nano_opt(23, 59, 59, nanos)
                    .unwrap()
                    .and_utc(),
            );
        }
    }
    for (index, value) in values.into_iter().enumerate() {
        identity.incarnation = (index + 1) as u64;
        let tx = connection.transaction()?;
        ensure_fresh_episode_continuity_tx(&tx, &identity, value)?;
        tx.commit()?;
        assert_eq!(
            bound_at(&connection, identity.incarnation)?,
            value.to_rfc3339()
        );
    }
    let mut buffer = ContinuityTimestamp {
        bytes: [0; 40],
        len: 0,
    };
    use std::fmt::Write;
    assert!(buffer.write_str(&"x".repeat(41)).is_err());
    assert_eq!(buffer.len, 0);
    buffer.write_str("valid")?;
    assert_eq!(&buffer.bytes[..buffer.len], b"valid");
    Ok(())
}

#[test]
fn member_digest_fixed_hex_matches_original_bytes_and_borrows_sql_text() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    assert_eq!(
        &member_content_digest(b""),
        b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        &member_content_digest(b"abc"),
        b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let binary: Vec<u8> = (0..=255).collect();
    let long = "  {\n \"escaped\": \"\\u754c\", \"x\": 1 }\0\\\"\n".repeat(2048);
    for payload in [
        b"".as_slice(),
        b"abc",
        b"\0\"\\\r\n",
        "界é🦀".as_bytes(),
        binary.as_slice(),
        long.as_bytes(),
    ] {
        let hex = member_content_digest(payload);
        let text = ToSqlOutput::Borrowed(ValueRef::Text(&hex));
        let ToSqlOutput::Borrowed(ValueRef::Text(bound)) = text.to_sql()? else {
            panic!("member digest must bind borrowed SQL TEXT");
        };
        assert_eq!(bound.as_ptr(), hex.as_ptr());
        assert_eq!(bound.len(), 64);
        let expected = format!("{:x}", Sha256::digest(payload));
        assert_eq!(bound, expected.as_bytes());
        let result: (String, String) =
            connection.query_row("SELECT typeof(?1), ?1", [text], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!(result, ("text".to_owned(), expected));
    }
    Ok(())
}

#[test]
fn member_qualification_fixed_json_matches_all_combinations_and_borrows_text() -> Result<()> {
    let connection = Connection::open_in_memory()?;
    for mask in 0..8 {
        let reasons: Vec<_> = REASONS
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, reason)| *reason)
            .collect();
        let expected = serde_json::to_string(&reasons)?;
        let json = member_qualification_json(&reasons)?;
        let ToSqlOutput::Borrowed(ValueRef::Text(bound)) = json.to_sql()? else {
            panic!("qualification JSON must bind borrowed SQL TEXT");
        };
        assert_eq!(bound.as_ptr(), json.bytes.as_ptr());
        assert_eq!(bound, expected.as_bytes());
        assert_eq!(json.len, expected.len());
        if mask == 7 {
            assert_eq!(json.len, MEMBER_QUALIFICATION_JSON_MAX_BYTES);
        }
        let result: (String, String) =
            connection.query_row("SELECT typeof(?1), ?1", [&json], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!(result, ("text".to_owned(), expected));
    }
    // A future longer reason must fail rather than truncate or grow the buffer.
    let oversized = "x".repeat(MEMBER_QUALIFICATION_JSON_MAX_BYTES);
    let error = member_qualification_json(&[&oversized])
        .err()
        .expect("bounded qualification encoder unexpectedly grew");
    assert!(error.downcast_ref::<serde_json::Error>().unwrap().is_io());
    Ok(())
}

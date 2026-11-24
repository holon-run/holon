use super::*;
use crate::tests::{migrated_connection, write_identity};
use holon_domain::{
    AgentIdentityRecord, AgentKind, AgentOwnership, AgentProfilePreset, AgentVisibility, BriefKind,
};

#[test]
fn retry_keeps_the_original_cause_and_the_highest_priority() -> Result<()> {
    let mut connection = migrated_connection()?;
    let agent_id = "inbox-agent";
    let first = BriefRecord::new(agent_id, BriefKind::Result, "first", None, None);
    let retry = BriefRecord::new(agent_id, BriefKind::Result, "retry", None, None);
    let transaction = connection.transaction()?;
    write_identity(
        &transaction,
        &AgentIdentityRecord::new(
            agent_id,
            AgentKind::Named,
            AgentVisibility::Public,
            AgentOwnership::SelfOwned,
            AgentProfilePreset::PublicNamed,
            None,
            None,
        ),
    )?;
    for brief in [&first, &retry] {
        transaction.execute(
            "INSERT INTO briefs (evidence_id, agent_id, created_at, kind, payload_json)
             VALUES (?1, ?2, ?3, 'result', '{}')",
            params![brief.id, brief.agent_id, brief.created_at.to_rfc3339()],
        )?;
    }
    let recipient = "user";
    record_cause(
        &transaction,
        &first,
        recipient,
        NotificationKind::OperatorFirstBrief,
        "input",
        None,
    )?;
    record_cause(
        &transaction,
        &first,
        recipient,
        NotificationKind::OperatorInputRequired,
        "wait",
        None,
    )?;
    record_cause(
        &transaction,
        &retry,
        recipient,
        NotificationKind::OperatorFirstBrief,
        "input",
        None,
    )?;
    record_cause(
        &transaction,
        &retry,
        recipient,
        NotificationKind::OperatorInputRequired,
        "wait",
        None,
    )?;
    transaction.commit()?;

    let recent = recent(&connection, recipient, false, Utc::now())?;
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].id, first.id);
    assert_eq!(recent[0].kind, NotificationKind::OperatorInputRequired);
    Ok(())
}

//! One-time legacy storage import orchestration.

use std::fs;

use anyhow::{anyhow, Context, Result};

use crate::{runtime_db::RuntimeDb, types::AgentState};

use super::AppStorage;

#[derive(Debug, Clone, Copy)]
pub(crate) struct LegacyImporter<'a> {
    storage: &'a AppStorage,
}

impl<'a> LegacyImporter<'a> {
    pub(super) fn new(storage: &'a AppStorage) -> Self {
        Self { storage }
    }

    pub(crate) fn workspace_entries_complete(&self) -> Result<bool> {
        self.storage_domain_complete("workspace_entries")
    }

    pub(crate) fn import_host_domains(&self) -> Result<()> {
        let runtime_db = &self.storage.runtime_db;
        if !self.storage_domain_complete("workspace_entries")? {
            runtime_db
                .workspace_entries()
                .import_legacy(self.storage.read_recent_workspace_entries(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("workspace_occupancies")? {
            runtime_db
                .workspace_occupancies()
                .import_legacy(self.storage.read_recent_workspace_occupancies(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("agent_identities")? {
            runtime_db
                .agent_identities()
                .import_legacy(self.storage.read_recent_agent_identities(usize::MAX)?)?;
        }
        Ok(())
    }

    pub(crate) fn import_runtime_domains(&self) -> Result<()> {
        let runtime_db = &self.storage.runtime_db;
        let agent_states_complete = self.storage_domain_complete("agent_states")?;
        let recovered_agent_for_import = if agent_states_complete {
            None
        } else {
            self.read_agent_file()?
        };
        // Re-entrant with host registry bootstrap; completeness guards keep imports at most once.
        self.import_host_domains()?;
        if !self.storage_domain_complete("work_items")? {
            let mut legacy_work_items = self.storage.read_recent_work_items(usize::MAX)?;
            for record in &mut legacy_work_items {
                crate::work_item_plan::refresh_plan_artifact_metadata(
                    self.storage.data_dir(),
                    record,
                )?;
            }
            runtime_db.work_items().import_legacy(legacy_work_items)?;
        }
        if !agent_states_complete {
            runtime_db
                .agent_states()
                .import_legacy(recovered_agent_for_import)?;
        }
        if !self.storage_domain_complete("tasks")? {
            runtime_db
                .tasks()
                .import_legacy(self.storage.read_recent_tasks(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("external_triggers")? {
            runtime_db
                .external_triggers()
                .import_legacy(self.storage.read_recent_external_triggers(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("wait_conditions")? {
            runtime_db
                .wait_conditions()
                .import_legacy(self.storage.read_recent_wait_conditions(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("queue_entries")? {
            runtime_db
                .queue_entries()
                .import_legacy(self.storage.read_recent_queue_entries(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("timers")? {
            runtime_db
                .timers()
                .import_legacy(self.storage.read_recent_timers(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("messages")? {
            runtime_db
                .messages()
                .import_legacy(self.storage.read_all_message_values()?)?;
        }
        if !self.storage_domain_complete("transcript_entries")? {
            runtime_db
                .transcript_entries()
                .import_legacy(self.storage.read_all_transcript()?)?;
        }
        if !self.storage_domain_complete("work_item_delegations")? {
            runtime_db
                .work_item_delegations()
                .import_legacy(self.storage.read_recent_work_item_delegations(usize::MAX)?)?;
        }
        if !self.storage_domain_complete("work_item_continuations")? {
            runtime_db.work_item_continuations().import_empty()?;
        }
        if !self.storage_domain_complete("context_episode_anchors")? {
            runtime_db
                .context_episodes()
                .import_legacy(self.storage.read_recent_context_episodes(usize::MAX)?)?;
        }
        Ok(())
    }

    pub(crate) fn import_derived_domains(&self, agent_id: &str) -> Result<()> {
        let runtime_db = &self.storage.runtime_db;
        let turn_records_complete = self.storage_domain_complete("turn_records")?;
        let evidence_complete = self.storage_domain_complete("evidence")?;
        let legacy_messages = (!turn_records_complete || !evidence_complete)
            .then(|| self.storage.read_all_message_values())
            .transpose()?;
        if !turn_records_complete {
            runtime_db.turn_records().import_legacy(
                legacy_messages.clone().unwrap_or_default(),
                self.storage.read_recent_tool_executions(usize::MAX)?,
                self.storage.read_recent_briefs(usize::MAX)?,
                self.storage.read_recent_delivery_summaries(usize::MAX)?,
                self.storage.read_recent_wait_conditions(usize::MAX)?,
            )?;
        }
        if !evidence_complete {
            runtime_db.evidence().import_legacy(
                legacy_messages.unwrap_or_default(),
                self.storage.read_all_transcript()?,
                self.storage.read_recent_tool_executions(usize::MAX)?,
                self.storage.read_recent_briefs(usize::MAX)?,
                self.storage.read_recent_delivery_summaries(usize::MAX)?,
            )?;
        }
        if !self.storage_domain_complete("audit_events")? {
            runtime_db
                .audit_events()
                .import_legacy(Some(agent_id), Vec::new())?;
        }
        runtime_db.validate_expected_storage_domains(RuntimeDb::expected_storage_domains())
    }

    fn read_agent_file(&self) -> Result<Option<AgentState>> {
        let path = self.storage.state_dir().join("agent.json");
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let agent: AgentState = serde_json::from_str(&content)?;
        if let Some(storage_agent_id) = self.storage.agent_id.as_deref() {
            anyhow::ensure!(
                agent.id == storage_agent_id,
                "agent-scoped legacy import for `{}` cannot import agent state for `{}`",
                storage_agent_id,
                agent.id
            );
        }
        Ok(Some(agent))
    }

    fn storage_domain_complete(&self, domain: &str) -> Result<bool> {
        let expected = RuntimeDb::expected_storage_domains()
            .iter()
            .find(|expected| expected.domain == domain)
            .ok_or_else(|| anyhow!("unknown runtime storage domain {domain}"))?;
        self.storage
            .runtime_db
            .storage_domain_is_complete(expected.domain, expected.canonical_source)
    }
}

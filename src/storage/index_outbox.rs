//! Runtime index outbox effects and compatibility recovery behavior.

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::Notify;

use crate::{
    memory::index::enqueue_memory_index_upsert,
    runtime_db::{
        transitions::{PostCommitEffects, PostCommitWarning},
        RuntimeIndexChange, RuntimeIndexOperation,
    },
    tool::helpers::{command_output_source_ref, command_receipt_source_ref},
    types::{
        BriefKind, BriefRecord, ContextEpisodeRecord, MessageEnvelope, TaskRecord,
        ToolExecutionRecord, WorkItemRecord, WorkspaceEntry,
    },
};

use super::memory::memory_index_agent_key;

#[derive(Debug, Clone)]
pub struct RuntimeIndexOutbox {
    agent_id: Option<String>,
    read_only: bool,
    shared_indexes_dir: PathBuf,
    append_mutex: Arc<Mutex<()>>,
    memory_index_notify: Arc<Mutex<Option<Arc<Notify>>>>,
}

impl RuntimeIndexOutbox {
    pub(crate) fn new(
        agent_id: Option<String>,
        read_only: bool,
        shared_indexes_dir: PathBuf,
        append_mutex: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            agent_id,
            read_only,
            shared_indexes_dir,
            append_mutex,
            memory_index_notify: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn enable_notify(&self, notify: Arc<Notify>) -> Result<()> {
        let mut guard = self
            .memory_index_notify
            .lock()
            .map_err(|_| anyhow::anyhow!("memory index notify mutex poisoned"))?;
        *guard = Some(notify);
        Ok(())
    }

    pub(crate) fn notify_transition(&self, effects: &PostCommitEffects) -> Vec<PostCommitWarning> {
        let mut warnings = Vec::new();
        if effects.notify_memory_index {
            match self.memory_index_notify.lock() {
                Ok(guard) => {
                    if let Some(notify) = guard.as_ref() {
                        notify.notify_one();
                    }
                }
                Err(_) => {
                    let message = "memory index notify mutex poisoned after transition commit";
                    tracing::warn!(
                        agent_id = self.agent_id.as_deref().unwrap_or("<global>"),
                        message
                    );
                    warnings.push(PostCommitWarning {
                        effect: "memory_index_notification",
                        message: message.into(),
                    });
                }
            }
        }
        warnings
    }

    pub(crate) fn mark_dirty(&self) -> Result<()> {
        anyhow::ensure!(
            !self.read_only,
            "cannot write through read-only runtime storage"
        );
        let agent_id = self.storage_agent_id()?;
        let dirty_path = self.shared_indexes_dir.join(format!(
            "memory.{}.dirty",
            memory_index_agent_key(&agent_id)
        ));
        if dirty_path.exists() {
            return Ok(());
        }
        fs::create_dir_all(&self.shared_indexes_dir)?;
        fs::write(&dirty_path, b"dirty").with_context(|| "failed to mark memory index dirty")
    }

    fn upsert(
        &self,
        agent_id: impl Into<String>,
        source_kind: impl Into<String>,
        source_id: impl Into<String>,
        source_ref: impl Into<String>,
        source_updated_at: Option<DateTime<Utc>>,
        reason: &'static str,
    ) -> RuntimeIndexChange {
        RuntimeIndexChange {
            agent_id: agent_id.into(),
            source_kind: source_kind.into(),
            source_id: source_id.into(),
            source_ref: source_ref.into(),
            operation: RuntimeIndexOperation::Upsert,
            source_updated_at,
            reason: reason.into(),
        }
    }

    pub(crate) fn changes_for_brief(&self, brief: &BriefRecord) -> Vec<RuntimeIndexChange> {
        if brief.kind == BriefKind::Ack || brief.text.trim().is_empty() {
            return Vec::new();
        }
        vec![self.upsert(
            brief.agent_id.clone(),
            "brief",
            brief.id.clone(),
            format!("brief:{}", brief.id),
            Some(brief.created_at),
            "brief_written",
        )]
    }

    pub(crate) fn changes_for_message(&self, message: &MessageEnvelope) -> Vec<RuntimeIndexChange> {
        vec![self.upsert(
            message.agent_id.clone(),
            "message",
            message.id.clone(),
            format!("message:{}", message.id),
            Some(message.created_at),
            "message_written",
        )]
    }

    pub(crate) fn changes_for_task(&self, task: &TaskRecord) -> Vec<RuntimeIndexChange> {
        vec![self.upsert(
            task.agent_id.clone(),
            "task",
            task.id.clone(),
            format!("task:{}", task.id),
            Some(task.updated_at),
            "task_written",
        )]
    }

    pub(crate) fn changes_for_work_item(&self, record: &WorkItemRecord) -> Vec<RuntimeIndexChange> {
        vec![self.upsert(
            record.agent_id.clone(),
            "work_item",
            record.id.clone(),
            format!("work_item:{}", record.id),
            Some(record.updated_at),
            "work_item_written",
        )]
    }

    pub(crate) fn changes_for_context_episode(
        &self,
        record: &ContextEpisodeRecord,
    ) -> Vec<RuntimeIndexChange> {
        vec![self.upsert(
            record.agent_id.clone(),
            "context_episode",
            record.id.clone(),
            format!("episode:{}", record.id),
            Some(record.finalized_at),
            "context_episode_written",
        )]
    }

    pub(crate) fn changes_for_workspace_entry(
        &self,
        entry: &WorkspaceEntry,
    ) -> Vec<RuntimeIndexChange> {
        let Some(agent_id) = entry
            .owner_agent_id
            .clone()
            .or_else(|| self.storage_agent_id().ok())
        else {
            return Vec::new();
        };
        vec![self.upsert(
            agent_id,
            "workspace_profile",
            entry.workspace_id.clone(),
            format!("workspace_profile:{}", entry.workspace_id),
            Some(entry.updated_at),
            "workspace_profile_written",
        )]
    }

    pub(crate) fn changes_for_tool_execution(
        &self,
        record: &ToolExecutionRecord,
    ) -> Vec<RuntimeIndexChange> {
        let mut changes = Vec::new();
        match record.tool_name.as_str() {
            crate::tool::names::EXEC_COMMAND => {
                if record.input.get("cmd").and_then(Value::as_str).is_some() {
                    let source_ref = command_receipt_source_ref(&record.id, None);
                    changes.push(self.upsert(
                        record.agent_id.clone(),
                        "tool_command_receipt",
                        source_ref.clone(),
                        source_ref,
                        record.completed_at.or(Some(record.created_at)),
                        "tool_command_receipt_written",
                    ));
                }
            }
            crate::tool::names::EXEC_COMMAND_BATCH => {
                if let Some(items) = record.input.get("items").and_then(Value::as_array) {
                    for (offset, item) in items.iter().enumerate() {
                        if item.get("cmd").and_then(Value::as_str).is_some() {
                            let source_ref =
                                command_receipt_source_ref(&record.id, Some(offset + 1));
                            changes.push(self.upsert(
                                record.agent_id.clone(),
                                "tool_command_receipt",
                                source_ref.clone(),
                                source_ref,
                                record.completed_at.or(Some(record.created_at)),
                                "tool_command_receipt_written",
                            ));
                        }
                    }
                }
            }
            _ => {
                let source_ref = command_output_source_ref(&record.id, None, "output");
                changes.push(self.upsert(
                    record.agent_id.clone(),
                    "tool_execution_output_preview",
                    source_ref.clone(),
                    source_ref,
                    record.completed_at.or(Some(record.created_at)),
                    "tool_execution_output_preview_written",
                ));
            }
        }
        changes
    }

    pub(crate) fn enqueue_brief_best_effort(&self, brief: &BriefRecord) -> Result<()> {
        let source_ref = format!("brief:{}", brief.id);
        self.enqueue_best_effort("brief", &brief.id, &source_ref)
    }

    pub(crate) fn enqueue_message_best_effort(&self, message: &MessageEnvelope) -> Result<()> {
        let source_ref = format!("message:{}", message.id);
        self.enqueue_best_effort("message", &message.id, &source_ref)
    }

    pub(crate) fn enqueue_task_best_effort(&self, task: &TaskRecord) -> Result<()> {
        let source_ref = format!("task:{}", task.id);
        self.enqueue_best_effort("task", &task.id, &source_ref)
    }

    pub(crate) fn enqueue_work_item_best_effort(&self, record: &WorkItemRecord) -> Result<()> {
        let source_ref = format!("work_item:{}", record.id);
        self.enqueue_best_effort("work_item", &record.id, &source_ref)
    }

    pub(crate) fn enqueue_context_episode_best_effort(
        &self,
        record: &ContextEpisodeRecord,
    ) -> Result<()> {
        let source_ref = format!("episode:{}", record.id);
        self.enqueue_best_effort("context_episode", &record.id, &source_ref)
    }

    pub(crate) fn enqueue_workspace_entry_best_effort(&self, entry: &WorkspaceEntry) -> Result<()> {
        let source_ref = format!("workspace_profile:{}", entry.workspace_id);
        self.enqueue_best_effort("workspace_profile", &entry.workspace_id, &source_ref)
    }

    pub(crate) fn enqueue_tool_execution_best_effort(
        &self,
        record: &ToolExecutionRecord,
    ) -> Result<()> {
        let result = (|| -> Result<()> {
            match record.tool_name.as_str() {
                crate::tool::names::EXEC_COMMAND => {
                    if record.input.get("cmd").and_then(Value::as_str).is_some() {
                        let source_ref = command_receipt_source_ref(&record.id, None);
                        self.enqueue_source("tool_command_receipt", &source_ref, &source_ref)?;
                    }
                    Ok(())
                }
                crate::tool::names::EXEC_COMMAND_BATCH => {
                    if let Some(items) = record.input.get("items").and_then(Value::as_array) {
                        for (offset, item) in items.iter().enumerate() {
                            if item.get("cmd").and_then(Value::as_str).is_some() {
                                let source_ref =
                                    command_receipt_source_ref(&record.id, Some(offset + 1));
                                self.enqueue_source(
                                    "tool_command_receipt",
                                    &source_ref,
                                    &source_ref,
                                )?;
                            }
                        }
                    }
                    Ok(())
                }
                _ => {
                    let source_ref = command_output_source_ref(&record.id, None, "output");
                    self.enqueue_source("tool_execution_output", &source_ref, &source_ref)
                }
            }
        })();
        self.finish_enqueue(result, &record.tool_name, &record.id, &record.id)
    }

    fn enqueue_best_effort(
        &self,
        source_kind: &str,
        source_id: &str,
        source_ref: &str,
    ) -> Result<()> {
        let result = self.enqueue_source(source_kind, source_id, source_ref);
        self.finish_enqueue(result, source_kind, source_id, source_ref)
    }

    fn enqueue_source(&self, source_kind: &str, source_id: &str, source_ref: &str) -> Result<()> {
        let _guard = self
            .append_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("storage append mutex poisoned"))?;
        enqueue_memory_index_upsert(
            &self.shared_indexes_dir,
            &self.storage_agent_id()?,
            source_kind,
            source_id,
            source_ref,
        )
    }

    fn finish_enqueue(
        &self,
        result: Result<()>,
        source_kind: &str,
        source_id: &str,
        source_ref: &str,
    ) -> Result<()> {
        if let Err(error) = result {
            tracing::warn!(
                error = %error,
                agent_id = self.agent_id.as_deref().unwrap_or("<global>"),
                source_kind,
                source_id,
                source_ref,
                "memory index enqueue failed after canonical storage write"
            );
            if let Err(dirty_error) = self.mark_dirty() {
                tracing::warn!(
                    error = %dirty_error,
                    agent_id = self.agent_id.as_deref().unwrap_or("<global>"),
                    source_kind,
                    source_id,
                    source_ref,
                    "failed to mark memory index dirty after enqueue failure"
                );
            }
        }

        if let Ok(guard) = self.memory_index_notify.lock() {
            if let Some(notify) = guard.as_ref() {
                notify.notify_one();
            }
        }
        Ok(())
    }

    fn storage_agent_id(&self) -> Result<String> {
        self.agent_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("operation requires agent-scoped storage"))
    }
}

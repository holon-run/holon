use crate::domain::agent::AgentMessageRoutingContext;
use crate::ids;
use crate::observability::TraceContext;
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use holon_domain::{ContinuationTriggerKind, MessageKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

impl<'de> Deserialize<'de> for MessageEnvelope {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct MessageEnvelopeCompat {
            id: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            message_seq: Option<u64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            turn_id: Option<String>,
            #[serde(alias = "session_id")]
            agent_id: String,
            created_at: DateTime<Utc>,
            kind: MessageKind,
            origin: MessageOrigin,
            #[serde(default)]
            authority_class: Option<AuthorityClass>,
            #[serde(default, rename = "trust")]
            legacy_trust: Option<AuthorityClass>,
            priority: Priority,
            #[serde(default)]
            trigger_kind: Option<ContinuationTriggerKind>,
            #[serde(default)]
            work_item_id: Option<String>,
            #[serde(default)]
            task_id: Option<String>,
            #[serde(default)]
            source_refs: BTreeMap<String, String>,
            #[serde(default)]
            trace_context: Option<TraceContext>,
            body: MessageBody,
            #[serde(default)]
            delivery_surface: Option<MessageDeliverySurface>,
            #[serde(default)]
            admission_context: Option<AdmissionContext>,
            metadata: Option<Value>,
            correlation_id: Option<String>,
            causation_id: Option<String>,
            #[serde(default)]
            routing_context: Option<AgentMessageRoutingContext>,
        }

        let compat = MessageEnvelopeCompat::deserialize(deserializer)?;
        let authority_class = compat
            .authority_class
            .or(compat.legacy_trust)
            .ok_or_else(|| serde::de::Error::missing_field("authority_class"))?;
        Ok(Self {
            id: compat.id,
            message_seq: compat.message_seq,
            turn_id: compat.turn_id,
            agent_id: compat.agent_id,
            created_at: compat.created_at,
            kind: compat.kind,
            origin: compat.origin,
            authority_class,
            priority: compat.priority,
            trigger_kind: compat.trigger_kind,
            work_item_id: compat.work_item_id,
            task_id: compat.task_id,
            source_refs: compat.source_refs,
            trace_context: compat.trace_context,
            body: compat.body,
            delivery_surface: compat.delivery_surface,
            admission_context: compat.admission_context,
            metadata: compat.metadata,
            correlation_id: compat.correlation_id,
            causation_id: compat.causation_id,
            routing_context: compat.routing_context,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    // TODO: remove `interrupt` alias after older ledgers and request clients have migrated.
    #[serde(alias = "interrupt")]
    Interject,
    Next,
    Normal,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageOrigin {
    Operator {
        actor_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor_display_name: Option<String>,
    },
    Channel {
        channel_id: String,
        sender_id: Option<String>,
    },
    Webhook {
        source: String,
        event_type: Option<String>,
    },
    Callback {
        descriptor_id: String,
        source: Option<String>,
    },
    Timer {
        timer_id: String,
    },
    System {
        subsystem: String,
    },
    Task {
        task_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageBody {
    Text {
        text: String,
    },
    Json {
        value: Value,
    },
    Brief {
        title: Option<String>,
        text: String,
        attachments: Option<Vec<BriefAttachment>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct BriefAttachment {
    pub kind: String,
    pub name: String,
    pub uri: Option<String>,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityClass {
    #[serde(alias = "trusted_operator")]
    #[value(alias = "trusted-operator")]
    OperatorInstruction,
    #[serde(alias = "trusted_system")]
    #[value(alias = "trusted-system")]
    RuntimeInstruction,
    #[serde(alias = "trusted_integration")]
    #[value(alias = "trusted-integration")]
    IntegrationSignal,
    #[serde(alias = "untrusted_external")]
    #[value(alias = "untrusted-external")]
    ExternalEvidence,
}

pub const HOLON_CALLER_AGENT_ID_ENV: &str = "HOLON_CALLER_AGENT_ID";
pub const HOLON_CALLER_SOURCE_TASK_ID_ENV: &str = "HOLON_CALLER_SOURCE_TASK_ID";
pub const HOLON_CALLER_SOURCE_TURN_ID_ENV: &str = "HOLON_CALLER_SOURCE_TURN_ID";
pub const HOLON_CALLER_SOURCE_WORK_ITEM_ID_ENV: &str = "HOLON_CALLER_SOURCE_WORK_ITEM_ID";
pub const HOLON_CALLER_SOURCE_ACTIVATION_ID_ENV: &str = "HOLON_CALLER_SOURCE_ACTIVATION_ID";
pub const HOLON_CALLER_AUTHORITY_CLASS_ENV: &str = "HOLON_CALLER_AUTHORITY_CLASS";

/// Declaration-based provenance supplied to a CLI launched by an agent.
///
/// These values describe the caller and its source activation, but are not
/// authentication material. A local process can forge them.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AgentInvocationContext {
    pub caller_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_activation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_authority_class: Option<AuthorityClass>,
}

impl AgentInvocationContext {
    pub fn from_env() -> Result<Option<Self>, String> {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let caller_agent_id = read(HOLON_CALLER_AGENT_ID_ENV);
        let any_context_field = [
            HOLON_CALLER_SOURCE_TASK_ID_ENV,
            HOLON_CALLER_SOURCE_TURN_ID_ENV,
            HOLON_CALLER_SOURCE_WORK_ITEM_ID_ENV,
            HOLON_CALLER_SOURCE_ACTIVATION_ID_ENV,
            HOLON_CALLER_AUTHORITY_CLASS_ENV,
        ]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some());

        Self::from_fields(
            caller_agent_id,
            read(HOLON_CALLER_SOURCE_TASK_ID_ENV),
            read(HOLON_CALLER_SOURCE_TURN_ID_ENV),
            read(HOLON_CALLER_SOURCE_WORK_ITEM_ID_ENV),
            read(HOLON_CALLER_SOURCE_ACTIVATION_ID_ENV),
            read(HOLON_CALLER_AUTHORITY_CLASS_ENV),
            any_context_field,
        )
    }

    pub(super) fn from_fields(
        caller_agent_id: Option<String>,
        source_task_id: Option<String>,
        source_turn_id: Option<String>,
        source_work_item_id: Option<String>,
        source_activation_id: Option<String>,
        inherited_authority_class: Option<String>,
        any_context_field: bool,
    ) -> Result<Option<Self>, String> {
        let Some(caller_agent_id) = caller_agent_id else {
            if any_context_field {
                return Err(format!(
                    "{HOLON_CALLER_AGENT_ID_ENV} is required when caller context is present"
                ));
            }
            return Ok(None);
        };

        let inherited_authority_class = inherited_authority_class
            .map(|value| parse_agent_invocation_authority(&value))
            .transpose()?;

        Ok(Some(Self {
            caller_agent_id,
            source_task_id,
            source_turn_id,
            source_work_item_id,
            source_activation_id,
            inherited_authority_class,
        }))
    }
}

fn parse_agent_invocation_authority(value: &str) -> Result<AuthorityClass, String> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "operator_instruction" | "trusted_operator" => Ok(AuthorityClass::OperatorInstruction),
        "runtime_instruction" | "trusted_system" => Ok(AuthorityClass::RuntimeInstruction),
        "integration_signal" | "trusted_integration" => Ok(AuthorityClass::IntegrationSignal),
        "external_evidence" | "untrusted_external" => Ok(AuthorityClass::ExternalEvidence),
        _ => Err(format!(
            "invalid {HOLON_CALLER_AUTHORITY_CLASS_ENV}: {value}"
        )),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MessageEnvelope {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub kind: MessageKind,
    pub origin: MessageOrigin,
    pub authority_class: AuthorityClass,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_kind: Option<ContinuationTriggerKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_refs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_context: Option<TraceContext>,
    pub body: MessageBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_surface: Option<MessageDeliverySurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_context: Option<AdmissionContext>,
    pub metadata: Option<Value>,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_context: Option<AgentMessageRoutingContext>,
}

impl MessageEnvelope {
    pub(crate) fn is_runtime_owned_internal_followup(&self) -> bool {
        self.kind == MessageKind::InternalFollowup
            && self.delivery_surface == Some(MessageDeliverySurface::RuntimeSystem)
            && self.admission_context == Some(AdmissionContext::RuntimeOwned)
            && matches!(
                self.origin,
                MessageOrigin::System { .. } | MessageOrigin::Task { .. }
            )
    }

    pub fn new(
        agent_id: impl Into<String>,
        kind: MessageKind,
        origin: MessageOrigin,
        authority_class: AuthorityClass,
        priority: Priority,
        body: MessageBody,
    ) -> Self {
        Self {
            id: ids::message_id(),
            message_seq: None,
            turn_id: None,
            agent_id: agent_id.into(),
            created_at: Utc::now(),
            kind,
            origin,
            authority_class,
            priority,
            trigger_kind: None,
            work_item_id: None,
            task_id: None,
            source_refs: BTreeMap::new(),
            trace_context: None,
            body,
            delivery_surface: None,
            admission_context: None,
            metadata: None,
            correlation_id: None,
            causation_id: None,
            routing_context: None,
        }
    }

    pub fn with_admission(
        mut self,
        delivery_surface: MessageDeliverySurface,
        admission_context: AdmissionContext,
    ) -> Self {
        self.delivery_surface = Some(delivery_surface);
        self.admission_context = Some(admission_context);
        self
    }

    pub fn normalize_admission_fields(&mut self) {
        if self.trigger_kind.is_none() {
            self.trigger_kind = Some(admission_trigger_kind_for_message_kind(&self.kind));
        }

        if self.task_id.is_none() {
            if let MessageOrigin::Task { task_id } = &self.origin {
                self.task_id = Some(task_id.clone());
            }
        }

        let metadata_binding_trusted = self.metadata_binding_fields_are_trusted();
        if let Some(metadata) = self.metadata.as_ref() {
            if metadata_binding_trusted && self.work_item_id.is_none() {
                self.work_item_id = metadata_string(metadata, "work_item_id")
                    .or_else(|| metadata_string(metadata, "child_work_item_id"));
            }
            if metadata_binding_trusted && self.task_id.is_none() {
                self.task_id = metadata_string(metadata, "task_id");
            }
            collect_source_ref(metadata, &mut self.source_refs, "queued_event_id");
            collect_source_ref(metadata, &mut self.source_refs, "task_result_id");
            collect_source_ref(metadata, &mut self.source_refs, "external_trigger_id");
            collect_source_ref(metadata, &mut self.source_refs, "callback_delivery_id");
            collect_source_ref(metadata, &mut self.source_refs, "timer_id");
            collect_source_ref(metadata, &mut self.source_refs, "wait_id");
            collect_source_ref(metadata, &mut self.source_refs, "wait_generation");
            if let Some(wake_hint) = metadata.get("wake_hint") {
                collect_source_ref(wake_hint, &mut self.source_refs, "external_trigger_id");
                collect_source_ref(wake_hint, &mut self.source_refs, "resource");
                collect_source_ref(wake_hint, &mut self.source_refs, "wait_id");
                collect_source_ref(wake_hint, &mut self.source_refs, "wait_generation");
            }
        }

        match &self.origin {
            MessageOrigin::Callback { descriptor_id, .. } => {
                self.source_refs
                    .entry("external_trigger_id".into())
                    .or_insert_with(|| descriptor_id.clone());
            }
            MessageOrigin::Timer { timer_id } => {
                self.source_refs
                    .entry("timer_id".into())
                    .or_insert_with(|| timer_id.clone());
            }
            MessageOrigin::Task { task_id } => {
                self.source_refs
                    .entry("task_id".into())
                    .or_insert_with(|| task_id.clone());
            }
            _ => {}
        }

        if !self.source_refs.contains_key("interaction_id") {
            if let Some(interaction_id) = self.trusted_interaction_id() {
                self.source_refs
                    .insert("interaction_id".into(), interaction_id);
            }
        }
    }

    pub fn trusted_interaction_id(&self) -> Option<String> {
        if self.kind != MessageKind::OperatorPrompt
            || self.authority_class != AuthorityClass::OperatorInstruction
            || !matches!(self.origin, MessageOrigin::Operator { .. })
        {
            return None;
        }

        let scope = match (self.delivery_surface, self.admission_context) {
            (Some(MessageDeliverySurface::CliPrompt), Some(AdmissionContext::LocalProcess)) => {
                "cli_prompt"
            }
            (Some(MessageDeliverySurface::RunOnce), Some(AdmissionContext::LocalProcess)) => {
                "run_once"
            }
            (
                Some(MessageDeliverySurface::HttpControlPrompt),
                Some(AdmissionContext::ControlAuthenticated),
            ) => "http_control_prompt",
            (
                Some(MessageDeliverySurface::RemoteOperatorTransport),
                Some(AdmissionContext::OperatorTransportAuthenticated),
            ) => "remote_operator_transport",
            _ => return None,
        };

        self.source_refs
            .get("interaction_id")
            .map(String::as_str)
            .map(str::trim)
            .filter(|interaction_id| !interaction_id.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                let actor_id = match &self.origin {
                    MessageOrigin::Operator { actor_id, .. } => {
                        actor_id.as_deref().unwrap_or("operator")
                    }
                    _ => unreachable!("operator origin checked above"),
                };
                Some(crate::ids::interaction_id(&[
                    &self.agent_id,
                    scope,
                    actor_id,
                ]))
            })
    }

    fn metadata_binding_fields_are_trusted(&self) -> bool {
        self.authority_class == AuthorityClass::RuntimeInstruction
            && matches!(self.admission_context, Some(AdmissionContext::RuntimeOwned))
            && matches!(
                self.delivery_surface,
                Some(MessageDeliverySurface::RuntimeSystem | MessageDeliverySurface::TaskRejoin)
            )
    }
}

pub fn admission_trigger_kind_for_message_kind(kind: &MessageKind) -> ContinuationTriggerKind {
    match kind {
        MessageKind::OperatorPrompt => ContinuationTriggerKind::OperatorInput,
        MessageKind::ChannelEvent | MessageKind::WebhookEvent | MessageKind::CallbackEvent => {
            ContinuationTriggerKind::ExternalEvent
        }
        MessageKind::TimerTick => ContinuationTriggerKind::TimerFire,
        MessageKind::SystemTick => ContinuationTriggerKind::SystemTick,
        MessageKind::TaskResult | MessageKind::TaskStatus => ContinuationTriggerKind::TaskResult,
        MessageKind::InternalFollowup => ContinuationTriggerKind::InternalFollowup,
        MessageKind::Control | MessageKind::BriefAck | MessageKind::BriefResult => {
            ContinuationTriggerKind::SystemTick
        }
    }
}

fn metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn collect_source_ref(metadata: &Value, refs: &mut BTreeMap<String, String>, key: &str) {
    let Some(value) = metadata_string(metadata, key) else {
        return;
    };
    refs.entry(key.to_string()).or_insert(value);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDeliverySurface {
    CliPrompt,
    RunOnce,
    HttpPublicEnqueue,
    HttpWebhook,
    HttpCallbackEnqueue,
    HttpCallbackWake,
    HttpControlPrompt,
    RemoteOperatorTransport,
    TimerScheduler,
    RuntimeSystem,
    TaskRejoin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionContext {
    PublicUnauthenticated,
    ControlAuthenticated,
    OperatorTransportAuthenticated,
    ExternalTriggerCapability,
    LocalProcess,
    RuntimeOwned,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct EnqueueResult {
    pub enqueued: bool,
    pub priority: Priority,
    pub follow_up_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueEntryStatus {
    Queued,
    Dequeued,
    Processed,
    Interjected,
    /// Daemon shutdown interrupted processing; replay on recovery.
    Interrupted,
    Aborted,
    Dropped,
    Quarantined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatorMessageStatus {
    Sending,
    Queued,
    WaitingForSafePoint,
    Processing,
    Processed,
    Failed,
    Dropped,
    Quarantined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperatorMessageRecord {
    pub message_id: String,
    pub agent_id: String,
    pub status: OperatorMessageStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub body: MessageBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct QueueEntryRecord {
    pub message_id: String,
    #[serde(alias = "session_id")]
    pub agent_id: String,
    pub priority: Priority,
    pub status: QueueEntryStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

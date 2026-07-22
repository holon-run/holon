//! Semantic scheduler proposal contract.
//!
//! Providers may classify operator intent and propose a binding, but they never
//! mutate runtime state. The deterministic resolver below validates every
//! proposal against a canonical snapshot and degrades rejected proposals to
//! `Unresolved`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::scheduler_protocol::{
    activation_provenance_has_valid_authority, ActivationProvenance, SchedulerScenarioClass,
};
use crate::types::{
    AdmissionContext, AuthorityClass, MessageDeliverySurface, MessageEnvelope, MessageOrigin,
};

pub const MAX_CONFIDENCE_BPS: u16 = 10_000;
pub const SEMANTIC_CONTRACT_REVISION: u64 = 2;
pub const SEMANTIC_OPERATOR_BINDING_SCENARIO: SchedulerScenarioClass =
    SchedulerScenarioClass::OrdinarySemanticOperatorBinding;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticDecisionInput {
    pub contract_revision: u64,
    pub id: String,
    pub target_agent_id: String,
    pub ingress_route: SemanticIngressRoute,
    pub provenance: ActivationProvenance,
    pub snapshot_revision: u64,
    pub operator_input: String,
    #[serde(default)]
    pub waits: Vec<SemanticWaitCandidate>,
    #[serde(default)]
    pub work_items: Vec<SemanticWorkItemCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticIngressRoute {
    pub agent_id: String,
    pub message_seq: u64,
    pub delivery_surface: MessageDeliverySurface,
    pub admission_context: AdmissionContext,
    pub authority_class: AuthorityClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticWaitCandidate {
    pub wait_id: String,
    pub agent_id: String,
    pub generation: u64,
    pub state: SemanticWaitCandidateState,
    pub owner_work_item_id: String,
    pub summary: String,
    #[serde(default)]
    pub routing_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticWaitCandidateState {
    Active,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticWorkItemCandidate {
    pub work_item_id: String,
    pub agent_id: String,
    pub revision: u64,
    pub state: SemanticWorkItemCandidateState,
    pub summary: String,
    #[serde(default)]
    pub routing_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticWorkItemCandidateState {
    Runnable,
    Waiting,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticProposal {
    BindWait {
        input_id: String,
        snapshot_revision: u64,
        wait_id: String,
        generation: u64,
    },
    BindWorkItem {
        input_id: String,
        snapshot_revision: u64,
        work_item_id: String,
        revision: u64,
    },
    NewInteraction {
        input_id: String,
        snapshot_revision: u64,
    },
    Unresolved {
        input_id: String,
        snapshot_revision: u64,
    },
}

impl SemanticProposal {
    pub fn input_id(&self) -> &str {
        match self {
            Self::BindWait { input_id, .. }
            | Self::BindWorkItem { input_id, .. }
            | Self::NewInteraction { input_id, .. }
            | Self::Unresolved { input_id, .. } => input_id,
        }
    }

    pub fn snapshot_revision(&self) -> u64 {
        match self {
            Self::BindWait {
                snapshot_revision, ..
            }
            | Self::BindWorkItem {
                snapshot_revision, ..
            }
            | Self::NewInteraction {
                snapshot_revision, ..
            }
            | Self::Unresolved {
                snapshot_revision, ..
            } => *snapshot_revision,
        }
    }

    pub fn is_target_binding(&self) -> bool {
        matches!(self, Self::BindWait { .. } | Self::BindWorkItem { .. })
    }

    pub fn is_unresolved(&self) -> bool {
        matches!(self, Self::Unresolved { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProposalProviderIdentity {
    pub provider_id: String,
    pub model_ref: String,
    pub contract_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProposalProviderConfig {
    pub identity: SemanticProposalProviderIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProposalResponse {
    pub proposal: SemanticProposal,
    pub confidence_bps: u16,
    #[serde(default)]
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticProposalProviderError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[async_trait]
pub trait SemanticProposalProvider: Send + Sync {
    async fn propose(
        &self,
        input: &SemanticDecisionInput,
    ) -> Result<SemanticProposalResponse, SemanticProposalProviderError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticValidationPolicy {
    pub minimum_confidence_bps: u16,
}

impl SemanticValidationPolicy {
    pub fn validate(self) -> Result<Self, SemanticProposalValidationError> {
        if self.minimum_confidence_bps > MAX_CONFIDENCE_BPS {
            return Err(SemanticProposalValidationError::InvalidConfidence);
        }
        Ok(self)
    }
}

impl Default for SemanticValidationPolicy {
    fn default() -> Self {
        Self {
            minimum_confidence_bps: 9_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticProposalValidationError {
    InvalidInputId,
    UnsupportedInputContractRevision,
    InputSourceMismatch,
    DuplicateInputSource,
    InvalidTargetAgent,
    InvalidIngressRoute,
    IngressRouteMismatch,
    InvalidProvenance,
    InvalidWaitCandidate,
    CandidateAgentMismatch,
    DuplicateWait,
    InvalidWorkItemCandidate,
    DuplicateWorkItem,
    InvalidRoutingKey,
    InvalidProviderIdentity,
    UnsupportedContractRevision,
    InputIdMismatch,
    StaleSnapshotRevision,
    InvalidConfidence,
    LowConfidence,
    UnknownWait,
    InactiveWait,
    StaleWaitGeneration,
    UnknownWorkItem,
    TerminalWorkItem,
    StaleWorkItemRevision,
    AmbiguousBinding,
}

#[derive(Debug)]
pub(crate) struct TrustedSemanticIngress<'a> {
    message: &'a MessageEnvelope,
}

impl<'a> TrustedSemanticIngress<'a> {
    pub(crate) fn from_persisted_message(
        message: &'a MessageEnvelope,
    ) -> Result<Self, SemanticProposalValidationError> {
        let Some(message_seq) = message.message_seq else {
            return Err(SemanticProposalValidationError::InvalidIngressRoute);
        };
        if message_seq == 0
            || !is_canonical_identity(&message.id)
            || !is_canonical_identity(&message.agent_id)
            || !trusted_ingress_authority(message)
        {
            return Err(SemanticProposalValidationError::InvalidIngressRoute);
        }
        Ok(Self { message })
    }

    pub(crate) fn decision_input(
        &self,
        waits: Vec<SemanticWaitCandidate>,
        work_items: Vec<SemanticWorkItemCandidate>,
    ) -> SemanticDecisionInput {
        let message = self.message;
        SemanticDecisionInput {
            contract_revision: SEMANTIC_CONTRACT_REVISION,
            id: message.id.clone(),
            target_agent_id: message.agent_id.clone(),
            ingress_route: SemanticIngressRoute {
                agent_id: message.agent_id.clone(),
                message_seq: message
                    .message_seq
                    .expect("trusted persisted semantic ingress has a message sequence"),
                delivery_surface: message
                    .delivery_surface
                    .expect("trusted semantic ingress has a delivery surface"),
                admission_context: message
                    .admission_context
                    .expect("trusted semantic ingress has an admission context"),
                authority_class: message.authority_class,
            },
            provenance: ActivationProvenance {
                origin: semantic_activation_origin(&message.origin),
                trust: semantic_activation_trust(message.authority_class),
                source_id: message.id.clone(),
                correlation_id: message.correlation_id.clone(),
                causation_id: message.causation_id.clone(),
            },
            snapshot_revision: message
                .message_seq
                .expect("trusted persisted semantic ingress has a message sequence"),
            operator_input: semantic_message_text(message),
            waits,
            work_items,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SemanticProposalResolution {
    Accepted {
        provenance: ActivationProvenance,
        provider: SemanticProposalProviderIdentity,
        proposal: SemanticProposal,
        confidence_bps: u16,
        #[serde(default)]
        latency_ms: Option<u64>,
    },
    Unresolved {
        provenance: ActivationProvenance,
        provider: SemanticProposalProviderIdentity,
        proposed: SemanticProposal,
        effective: SemanticProposal,
        confidence_bps: u16,
        reason: SemanticProposalValidationError,
        #[serde(default)]
        latency_ms: Option<u64>,
    },
}

impl SemanticProposalResolution {
    pub fn provenance(&self) -> &ActivationProvenance {
        match self {
            Self::Accepted { provenance, .. } | Self::Unresolved { provenance, .. } => provenance,
        }
    }

    pub fn effective_proposal(&self) -> &SemanticProposal {
        match self {
            Self::Accepted { proposal, .. } => proposal,
            Self::Unresolved { effective, .. } => effective,
        }
    }

    pub fn accepted(&self) -> bool {
        matches!(self, Self::Accepted { .. })
    }
}

pub fn validate_semantic_decision_input(
    input: &SemanticDecisionInput,
) -> Result<(), SemanticProposalValidationError> {
    if input.contract_revision != SEMANTIC_CONTRACT_REVISION {
        return Err(SemanticProposalValidationError::UnsupportedInputContractRevision);
    }
    if !is_canonical_identity(&input.id) {
        return Err(SemanticProposalValidationError::InvalidInputId);
    }
    if !is_canonical_identity(&input.target_agent_id) {
        return Err(SemanticProposalValidationError::InvalidTargetAgent);
    }
    if !is_canonical_identity(&input.ingress_route.agent_id) {
        return Err(SemanticProposalValidationError::InvalidIngressRoute);
    }
    if !is_canonical_identity(&input.provenance.source_id)
        || input
            .provenance
            .correlation_id
            .as_deref()
            .is_some_and(|id| !is_canonical_identity(id))
        || input
            .provenance
            .causation_id
            .as_deref()
            .is_some_and(|id| !is_canonical_identity(id))
        || !activation_provenance_has_valid_authority(&input.provenance)
    {
        return Err(SemanticProposalValidationError::InvalidProvenance);
    }
    if input.ingress_route.message_seq == 0
        || input.ingress_route.message_seq != input.snapshot_revision
        || input.ingress_route.authority_class != semantic_authority_class(input.provenance.trust)
        || !trusted_semantic_route(&input.ingress_route, input.provenance.origin)
    {
        return Err(SemanticProposalValidationError::InvalidIngressRoute);
    }
    if input.ingress_route.agent_id != input.target_agent_id {
        return Err(SemanticProposalValidationError::IngressRouteMismatch);
    }
    if input.id != input.provenance.source_id {
        return Err(SemanticProposalValidationError::InputSourceMismatch);
    }

    let mut wait_ids = BTreeSet::new();
    for wait in &input.waits {
        if !is_canonical_identity(&wait.wait_id)
            || !is_canonical_identity(&wait.agent_id)
            || !is_canonical_identity(&wait.owner_work_item_id)
            || wait.summary.trim().is_empty()
        {
            return Err(SemanticProposalValidationError::InvalidWaitCandidate);
        }
        if wait.agent_id != input.target_agent_id {
            return Err(SemanticProposalValidationError::CandidateAgentMismatch);
        }
        if !wait_ids.insert(wait.wait_id.as_str()) {
            return Err(SemanticProposalValidationError::DuplicateWait);
        }
        validate_routing_keys(&wait.routing_keys)?;
    }

    let mut work_item_ids = BTreeSet::new();
    for work_item in &input.work_items {
        if !is_canonical_identity(&work_item.work_item_id)
            || !is_canonical_identity(&work_item.agent_id)
            || work_item.summary.trim().is_empty()
        {
            return Err(SemanticProposalValidationError::InvalidWorkItemCandidate);
        }
        if work_item.agent_id != input.target_agent_id {
            return Err(SemanticProposalValidationError::CandidateAgentMismatch);
        }
        if !work_item_ids.insert(work_item.work_item_id.as_str()) {
            return Err(SemanticProposalValidationError::DuplicateWorkItem);
        }
        validate_routing_keys(&work_item.routing_keys)?;
    }
    Ok(())
}

pub fn validate_semantic_decision_inputs(
    inputs: &[SemanticDecisionInput],
) -> Result<(), SemanticProposalValidationError> {
    let mut source_ids = BTreeSet::new();
    for input in inputs {
        validate_semantic_decision_input(input)?;
        if !source_ids.insert(input.provenance.source_id.as_str()) {
            return Err(SemanticProposalValidationError::DuplicateInputSource);
        }
    }
    Ok(())
}

pub fn validate_semantic_provider_identity(
    provider: &SemanticProposalProviderIdentity,
) -> Result<(), SemanticProposalValidationError> {
    if !is_canonical_identity(&provider.provider_id) || !is_canonical_identity(&provider.model_ref)
    {
        return Err(SemanticProposalValidationError::InvalidProviderIdentity);
    }
    if provider.contract_revision != SEMANTIC_CONTRACT_REVISION {
        return Err(SemanticProposalValidationError::UnsupportedContractRevision);
    }
    Ok(())
}

pub fn validate_semantic_provider_config(
    provider: &SemanticProposalProviderConfig,
) -> Result<(), SemanticProposalValidationError> {
    validate_semantic_provider_identity(&provider.identity)
}

pub fn validate_semantic_proposal(
    input: &SemanticDecisionInput,
    proposal: &SemanticProposal,
) -> Result<(), SemanticProposalValidationError> {
    validate_semantic_decision_input(input)?;
    if proposal.input_id() != input.id {
        return Err(SemanticProposalValidationError::InputIdMismatch);
    }
    if proposal.snapshot_revision() != input.snapshot_revision {
        return Err(SemanticProposalValidationError::StaleSnapshotRevision);
    }
    match proposal {
        SemanticProposal::BindWait {
            wait_id,
            generation,
            ..
        } => {
            let wait = input
                .waits
                .iter()
                .find(|candidate| candidate.wait_id == *wait_id)
                .ok_or(SemanticProposalValidationError::UnknownWait)?;
            if wait.state != SemanticWaitCandidateState::Active {
                return Err(SemanticProposalValidationError::InactiveWait);
            }
            if wait.generation != *generation {
                return Err(SemanticProposalValidationError::StaleWaitGeneration);
            }
            let active_waits: Vec<_> = input
                .waits
                .iter()
                .filter(|candidate| candidate.state == SemanticWaitCandidateState::Active)
                .collect();
            if active_waits.len() > 1
                && !uniquely_matches(
                    &input.operator_input,
                    wait_id,
                    active_waits
                        .iter()
                        .map(|candidate| (candidate.wait_id.as_str(), &candidate.routing_keys)),
                )
            {
                return Err(SemanticProposalValidationError::AmbiguousBinding);
            }
        }
        SemanticProposal::BindWorkItem {
            work_item_id,
            revision,
            ..
        } => {
            let work_item = input
                .work_items
                .iter()
                .find(|candidate| candidate.work_item_id == *work_item_id)
                .ok_or(SemanticProposalValidationError::UnknownWorkItem)?;
            if work_item.state == SemanticWorkItemCandidateState::Terminal {
                return Err(SemanticProposalValidationError::TerminalWorkItem);
            }
            if work_item.revision != *revision {
                return Err(SemanticProposalValidationError::StaleWorkItemRevision);
            }
            let eligible_work_items: Vec<_> = input
                .work_items
                .iter()
                .filter(|candidate| candidate.state != SemanticWorkItemCandidateState::Terminal)
                .collect();
            if eligible_work_items.len() > 1
                && !uniquely_matches(
                    &input.operator_input,
                    work_item_id,
                    eligible_work_items.iter().map(|candidate| {
                        (candidate.work_item_id.as_str(), &candidate.routing_keys)
                    }),
                )
            {
                return Err(SemanticProposalValidationError::AmbiguousBinding);
            }
        }
        SemanticProposal::NewInteraction { .. } | SemanticProposal::Unresolved { .. } => {}
    }
    Ok(())
}

pub fn resolve_semantic_proposal(
    input: &SemanticDecisionInput,
    provider: SemanticProposalProviderConfig,
    response: SemanticProposalResponse,
    policy: SemanticValidationPolicy,
) -> SemanticProposalResolution {
    let validation = validate_semantic_decision_input(input)
        .and_then(|()| validate_semantic_provider_config(&provider))
        .and_then(|()| policy.validate())
        .and_then(|policy| {
            if response.confidence_bps > MAX_CONFIDENCE_BPS {
                return Err(SemanticProposalValidationError::InvalidConfidence);
            }
            if response.confidence_bps < policy.minimum_confidence_bps {
                return Err(SemanticProposalValidationError::LowConfidence);
            }
            validate_semantic_proposal(input, &response.proposal)
        });

    match validation {
        Ok(()) => SemanticProposalResolution::Accepted {
            provenance: input.provenance.clone(),
            provider: provider.identity,
            proposal: response.proposal,
            confidence_bps: response.confidence_bps,
            latency_ms: response.latency_ms,
        },
        Err(reason) => SemanticProposalResolution::Unresolved {
            provenance: input.provenance.clone(),
            provider: provider.identity,
            proposed: response.proposal,
            effective: unresolved(input),
            confidence_bps: response.confidence_bps,
            reason,
            latency_ms: response.latency_ms,
        },
    }
}

pub fn structural_semantic_proposal(input: &SemanticDecisionInput) -> SemanticProposal {
    if validate_semantic_decision_input(input).is_err() {
        return unresolved(input);
    }

    let matching_waits: Vec<_> = input
        .waits
        .iter()
        .filter(|wait| input.operator_input.contains(&wait.wait_id))
        .collect();
    if let [wait] = matching_waits.as_slice() {
        return SemanticProposal::BindWait {
            input_id: input.id.clone(),
            snapshot_revision: input.snapshot_revision,
            wait_id: wait.wait_id.clone(),
            generation: wait.generation,
        };
    }

    let matching_work_items: Vec<_> = input
        .work_items
        .iter()
        .filter(|work_item| input.operator_input.contains(&work_item.work_item_id))
        .collect();
    if let [work_item] = matching_work_items.as_slice() {
        return SemanticProposal::BindWorkItem {
            input_id: input.id.clone(),
            snapshot_revision: input.snapshot_revision,
            work_item_id: work_item.work_item_id.clone(),
            revision: work_item.revision,
        };
    }
    unresolved(input)
}

fn unresolved(input: &SemanticDecisionInput) -> SemanticProposal {
    SemanticProposal::Unresolved {
        input_id: input.id.clone(),
        snapshot_revision: input.snapshot_revision,
    }
}

fn uniquely_matches<'a>(
    operator_input: &str,
    selected_id: &str,
    candidates: impl Iterator<Item = (&'a str, &'a Vec<String>)>,
) -> bool {
    let matched: Vec<_> = candidates
        .filter(|(candidate_id, routing_keys)| {
            operator_input.contains(candidate_id)
                || routing_keys
                    .iter()
                    .any(|routing_key| operator_input.contains(routing_key))
        })
        .map(|(candidate_id, _)| candidate_id)
        .collect();
    matched == [selected_id]
}

fn validate_routing_keys(routing_keys: &[String]) -> Result<(), SemanticProposalValidationError> {
    let mut unique = BTreeSet::new();
    for routing_key in routing_keys {
        if !is_canonical_identity(routing_key) || !unique.insert(routing_key.as_str()) {
            return Err(SemanticProposalValidationError::InvalidRoutingKey);
        }
    }
    Ok(())
}

fn is_canonical_identity(value: &str) -> bool {
    !value.is_empty() && value == value.trim()
}

fn semantic_message_text(message: &MessageEnvelope) -> String {
    match &message.body {
        crate::types::MessageBody::Text { text } => text.clone(),
        crate::types::MessageBody::Brief { text, .. } => text.clone(),
        crate::types::MessageBody::Json { value } => value.to_string(),
    }
}

fn semantic_activation_origin(
    origin: &MessageOrigin,
) -> super::scheduler_protocol::ActivationOrigin {
    use super::scheduler_protocol::ActivationOrigin;
    match origin {
        MessageOrigin::Operator { .. } => ActivationOrigin::Operator,
        MessageOrigin::Channel { .. } => ActivationOrigin::Channel,
        MessageOrigin::Webhook { .. } => ActivationOrigin::Webhook,
        MessageOrigin::Callback { .. } => ActivationOrigin::Callback,
        MessageOrigin::Timer { .. } => ActivationOrigin::Timer,
        MessageOrigin::System { .. } => ActivationOrigin::System,
        MessageOrigin::Task { .. } => ActivationOrigin::Task,
    }
}

fn semantic_activation_trust(
    authority: AuthorityClass,
) -> super::scheduler_protocol::ActivationTrust {
    use super::scheduler_protocol::ActivationTrust;
    match authority {
        AuthorityClass::OperatorInstruction => ActivationTrust::OperatorInstruction,
        AuthorityClass::RuntimeInstruction => ActivationTrust::RuntimeInstruction,
        AuthorityClass::IntegrationSignal => ActivationTrust::IntegrationSignal,
        AuthorityClass::ExternalEvidence => ActivationTrust::ExternalEvidence,
    }
}

fn semantic_authority_class(trust: super::scheduler_protocol::ActivationTrust) -> AuthorityClass {
    use super::scheduler_protocol::ActivationTrust;
    match trust {
        ActivationTrust::OperatorInstruction => AuthorityClass::OperatorInstruction,
        ActivationTrust::RuntimeInstruction => AuthorityClass::RuntimeInstruction,
        ActivationTrust::IntegrationSignal => AuthorityClass::IntegrationSignal,
        ActivationTrust::ExternalEvidence => AuthorityClass::ExternalEvidence,
    }
}

fn trusted_ingress_authority(message: &MessageEnvelope) -> bool {
    let (Some(delivery_surface), Some(admission_context)) =
        (message.delivery_surface, message.admission_context)
    else {
        return false;
    };
    trusted_semantic_route(
        &SemanticIngressRoute {
            agent_id: message.agent_id.clone(),
            message_seq: message.message_seq.unwrap_or_default(),
            delivery_surface,
            admission_context,
            authority_class: message.authority_class,
        },
        semantic_activation_origin(&message.origin),
    ) && message.authority_class
        == semantic_authority_class(semantic_activation_trust(message.authority_class))
}

fn trusted_semantic_route(
    route: &SemanticIngressRoute,
    origin: super::scheduler_protocol::ActivationOrigin,
) -> bool {
    use super::scheduler_protocol::ActivationOrigin;
    match origin {
        ActivationOrigin::Operator => {
            route.authority_class == AuthorityClass::OperatorInstruction
                && matches!(
                    (route.delivery_surface, route.admission_context),
                    (
                        MessageDeliverySurface::CliPrompt | MessageDeliverySurface::RunOnce,
                        AdmissionContext::LocalProcess
                    ) | (
                        MessageDeliverySurface::HttpControlPrompt,
                        AdmissionContext::ControlAuthenticated
                    ) | (
                        MessageDeliverySurface::RemoteOperatorTransport,
                        AdmissionContext::OperatorTransportAuthenticated
                    )
                )
        }
        ActivationOrigin::Channel | ActivationOrigin::Webhook => matches!(
            route.authority_class,
            AuthorityClass::IntegrationSignal | AuthorityClass::ExternalEvidence
        ),
        ActivationOrigin::Callback => matches!(
            route.authority_class,
            AuthorityClass::RuntimeInstruction
                | AuthorityClass::IntegrationSignal
                | AuthorityClass::ExternalEvidence
        ),
        ActivationOrigin::Timer | ActivationOrigin::System => matches!(
            route.authority_class,
            AuthorityClass::RuntimeInstruction | AuthorityClass::IntegrationSignal
        ),
        ActivationOrigin::Task | ActivationOrigin::RuntimeRecovery => {
            route.authority_class == AuthorityClass::RuntimeInstruction
        }
    }
}

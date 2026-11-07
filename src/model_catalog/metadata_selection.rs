//! Borrow first; only the requested output owns selected metadata.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct SelectedConstraint {
    pub field: ModelMetadataField,
    pub source: ModelMetadataConstraintSource,
    pub clamp: Option<(usize, usize)>,
}

impl SelectedConstraint {
    fn into_owned(self) -> ModelMetadataConstraint {
        let (kind, requested, effective) = match self.clamp {
            Some((requested, effective)) => (
                ModelMetadataConstraintKind::Clamped,
                requested.to_string(),
                effective.to_string(),
            ),
            None => (
                ModelMetadataConstraintKind::Disabled,
                "true".to_owned(),
                "false".to_owned(),
            ),
        };
        ModelMetadataConstraint {
            field: self.field,
            kind,
            source: self.source,
            requested,
            effective,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct SelectedEvidence {
    // One origin per field; six endpoint disables and four token-budget clamps.
    fields: [Option<(ModelMetadataField, ModelMetadataOrigin)>; 19],
    constraints: [Option<SelectedConstraint>; 10],
}

impl SelectedEvidence {
    pub fn source_for(&self, field: ModelMetadataField) -> Option<ModelMetadataOrigin> {
        self.fields
            .iter()
            .flatten()
            .find_map(|(key, origin)| (*key == field).then_some(*origin))
    }

    pub fn record(&mut self, field: ModelMetadataField, origin: ModelMetadataOrigin) {
        if self.fields.iter().flatten().any(|(key, _)| *key == field) {
            return;
        }
        *self
            .fields
            .iter_mut()
            .find(|slot| slot.is_none())
            .expect("metadata field bound") = Some((field, origin));
    }

    pub fn constrain(&mut self, constraint: SelectedConstraint) {
        *self
            .constraints
            .iter_mut()
            .find(|slot| slot.is_none())
            .expect("metadata constraint bound") = Some(constraint);
    }

    fn footprint(&self) -> (usize, usize) {
        let fields = self.fields.iter().flatten().count();
        let constraints = self.constraints.iter().flatten().count();
        // BTree nodes and Vec capacity, including both formatted constraint scalars.
        let scalar_bytes = 2 * (usize::MAX.ilog10() as usize + 1);
        let bytes = fields
            * (std::mem::size_of::<(ModelMetadataField, ModelMetadataOrigin)>()
                + 4 * std::mem::size_of::<usize>())
            + constraints * (std::mem::size_of::<ModelMetadataConstraint>() + scalar_bytes);
        (bytes, fields + 3 * constraints)
    }

    fn into_owned(self) -> ModelMetadataEvidence {
        let mut constraints = Vec::with_capacity(self.constraints.iter().flatten().count());
        constraints.extend(
            self.constraints
                .into_iter()
                .flatten()
                .map(SelectedConstraint::into_owned),
        );
        ModelMetadataEvidence {
            fields: self.fields.into_iter().flatten().collect(),
            constraints,
        }
    }
}

#[derive(Clone)]
pub(super) enum SelectedReasoningOptions<'a> {
    Configured(&'a [String]),
    Derived(&'static [&'static str]),
}

impl SelectedReasoningOptions<'_> {
    fn footprint(&self) -> (usize, usize) {
        let (bytes, count) = match self {
            Self::Configured(options) => (
                options.iter().map(String::len).sum::<usize>(),
                options.len(),
            ),
            Self::Derived(options) => (
                options.iter().map(|value| value.len()).sum::<usize>(),
                options.len(),
            ),
        };
        (bytes + count * std::mem::size_of::<String>(), count)
    }

    fn into_owned(self) -> Vec<String> {
        match self {
            Self::Configured(options) => options.to_vec(),
            Self::Derived(options) => options.iter().map(|value| (*value).to_owned()).collect(),
        }
    }
}

#[derive(Clone)]
pub(super) struct ModelMetadataSelection<'a> {
    pub model_ref: &'a ModelRef,
    pub display_name: Option<&'a String>,
    pub description: &'a str,
    pub context_window_tokens: Option<usize>,
    pub effective_context_window_percent: u8,
    pub prompt_budget_estimated_tokens: usize,
    pub compaction_trigger_estimated_tokens: usize,
    pub compaction_keep_recent_estimated_tokens: usize,
    pub runtime_max_output_tokens: u32,
    pub verbosity: Option<ModelVerbosity>,
    pub tool_output_truncation_estimated_tokens: usize,
    pub max_output_tokens_upper_limit: Option<u32>,
    pub capabilities: ModelCapabilityFlags,
    pub decision_capable: bool,
    pub decision_protocol: Option<DecisionProtocol>,
    pub reasoning_effort_options: SelectedReasoningOptions<'a>,
    pub source: ModelMetadataSource,
    pub evidence: SelectedEvidence,
    pub auto_compact_token_limit: Option<usize>,
    pub effective_default_max_output_tokens: Option<u32>,
    pub configured_tool_output_truncation_estimated_tokens: Option<usize>,
    pub endpoint: Option<&'a ProviderEndpointId>,
}

impl ModelMetadataSelection<'_> {
    pub fn try_into_policy(self) -> anyhow::Result<ResolvedRuntimeModelPolicy> {
        let (options_bytes, options) = self.reasoning_effort_options.footprint();
        let (evidence_bytes, evidence_nodes) = self.evidence.footprint();
        let model_bytes = self.model_ref.provider.as_str().len() + self.model_ref.model.len();
        let name_bytes = self.display_name.map_or(model_bytes + 1, String::len);
        let bytes = std::mem::size_of::<ResolvedRuntimeModelPolicy>()
            .saturating_add(model_bytes)
            .saturating_add(name_bytes)
            .saturating_add(self.description.len())
            .saturating_add(options_bytes)
            .saturating_add(evidence_bytes);
        crate::diagnostics::resource_io::charge_owned_metadata(
            u64::try_from(bytes)?,
            5usize
                .saturating_add(options)
                .saturating_add(evidence_nodes),
        )?;
        Ok(self.into_policy())
    }

    pub fn into_policy(self) -> ResolvedRuntimeModelPolicy {
        ResolvedRuntimeModelPolicy {
            model_ref: self.model_ref.clone(),
            display_name: self
                .display_name
                .cloned()
                .unwrap_or_else(|| self.model_ref.as_string()),
            description: self.description.to_owned(),
            context_window_tokens: self.context_window_tokens,
            effective_context_window_percent: self.effective_context_window_percent,
            prompt_budget_estimated_tokens: self.prompt_budget_estimated_tokens,
            compaction_trigger_estimated_tokens: self.compaction_trigger_estimated_tokens,
            compaction_keep_recent_estimated_tokens: self.compaction_keep_recent_estimated_tokens,
            runtime_max_output_tokens: self.runtime_max_output_tokens,
            verbosity: self.verbosity,
            tool_output_truncation_estimated_tokens: self.tool_output_truncation_estimated_tokens,
            max_output_tokens_upper_limit: self.max_output_tokens_upper_limit,
            agent_turn: self.capabilities.agent_turn,
            capabilities: self.capabilities,
            decision_capable: self.decision_capable,
            decision_protocol: self.decision_protocol,
            reasoning_effort_options: self.reasoning_effort_options.into_owned(),
            source: self.source,
            evidence: self.evidence.into_owned(),
        }
    }

    pub fn into_catalog_entry(self) -> BuiltInModelMetadata {
        BuiltInModelMetadata {
            model_ref: self.model_ref.clone(),
            display_name: self
                .display_name
                .cloned()
                .unwrap_or_else(|| self.model_ref.as_string()),
            description: self.description.to_owned(),
            context_window_tokens: self.context_window_tokens,
            effective_context_window_percent: self.effective_context_window_percent,
            auto_compact_token_limit: self.auto_compact_token_limit,
            default_max_output_tokens: self.effective_default_max_output_tokens,
            max_output_tokens_upper_limit: self.max_output_tokens_upper_limit,
            default_verbosity: self.verbosity,
            tool_output_truncation_estimated_tokens: self
                .configured_tool_output_truncation_estimated_tokens,
            capabilities: self.capabilities,
            reasoning_effort_options: self.reasoning_effort_options.into_owned(),
            source: self.source,
            endpoint: self.endpoint.cloned(),
        }
    }
}

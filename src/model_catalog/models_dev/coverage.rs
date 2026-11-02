//! Offline route coverage. Upstream values are review candidates, never writes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::{
    config::{ModelRouteRef, ProviderEndpointId},
    context::ContextConfig,
    model_catalog::{BuiltInModelCatalog, ModelCapabilityFlags, ModelMetadataEvidence},
};

use super::{ModelsDevSnapshot, ProviderMapping};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EndpointAssociation {
    pub models_dev_id: String,
    pub route_registration: String,
    /// Exact reviewed compatibility routes, including those without pinned metadata.
    #[serde(default)]
    pub supported_models: Vec<String>,
    pub region: String,
    pub owner: String,
    pub reviewed_at: String,
    pub evidence: Vec<String>,
}

/// A separate, review-only mapping: plan metadata must not enter the generic
/// projector and leak into another endpoint's canonical model.
pub fn endpoint_associations() -> Result<Vec<EndpointAssociation>, String> {
    serde_json::from_str(include_str!("../../../models.dev/endpoint-mappings.json"))
        .map_err(|e| format!("invalid endpoint associations: {e}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFacts {
    pub context_window_tokens: Option<usize>,
    pub default_max_output_tokens: Option<u32>,
    pub max_output_tokens_upper_limit: Option<u32>,
    pub prompt_budget_estimated_tokens: usize,
    pub runtime_max_output_tokens: u32,
    pub capabilities: ModelCapabilityFlags,
    pub agent_turn: bool,
    pub decision_capable: bool,
    pub reasoning_effort_options: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamCandidate {
    pub models_dev_ref: String,
    pub context_window_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub differences: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageRow {
    pub route_ref: ModelRouteRef,
    pub aliases: Vec<String>,
    pub raw_model: serde_json::Value,
    pub raw_route: serde_json::Value,
    pub effective: RuntimeFacts,
    pub evidence: ModelMetadataEvidence,
    pub upstream_candidates: Vec<UpstreamCandidate>,
    pub context_status: LimitStatus,
    pub output_status: LimitStatus,
    pub review: Option<UnknownReview>,
    pub missing_reasons: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    pub schema_version: u32,
    pub scope: String,
    pub required_routes: Vec<ModelRouteRef>,
    pub routes: Vec<CoverageRow>,
    pub unassociated_plan_providers: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LimitStatus {
    Known,
    Unknown,
    Dynamic,
    NotApplicable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnknownReview {
    pub owner: String,
    pub reviewed_at: String,
    pub reason: String,
    pub recheck: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedRoute {
    pub route_ref: ModelRouteRef,
    pub facts: RuntimeFacts,
    pub inputs: ReviewedInputs,
    pub context_status: LimitStatus,
    pub output_status: LimitStatus,
    pub review: Option<UnknownReview>,
}

/// Only runtime-relevant raw fields participate, not descriptions or unrelated
/// registration details. A missing route field differs from an explicit null.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewedInputs {
    pub model: BTreeMap<String, serde_json::Value>,
    pub route: BTreeMap<String, serde_json::Value>,
    pub evidence: ModelMetadataEvidence,
}

impl CoverageRow {
    pub fn reviewed_inputs(&self) -> ReviewedInputs {
        fn protected_fields(raw: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
            [
                "context_window_tokens",
                "effective_context_window_percent",
                "auto_compact_token_limit",
                "default_max_output_tokens",
                "max_output_tokens_upper_limit",
                "default_verbosity",
                "tool_output_truncation_estimated_tokens",
                "capabilities",
                "decision_protocol",
                "reasoning_effort_options",
            ]
            .into_iter()
            .filter_map(|key| raw.get(key).map(|value| (key.to_string(), value.clone())))
            .collect()
        }
        ReviewedInputs {
            model: protected_fields(&self.raw_model),
            route: protected_fields(&self.raw_route),
            evidence: self.evidence.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageBaseline {
    pub schema_version: u32,
    pub routes: Vec<ReviewedRoute>,
}

pub fn generate_coverage(
    snapshot: &ModelsDevSnapshot,
    catalog: &BuiltInModelCatalog,
    associations: &[EndpointAssociation],
) -> Result<CoverageReport, String> {
    let mut routes = catalog.list_routes();
    for model in catalog.list() {
        if !routes.iter().any(|r| r.model_ref() == model.model_ref) {
            routes.push(
                catalog
                    .preferred_route_for_model(&model.model_ref)
                    .unwrap_or_else(|| {
                        ModelRouteRef::new(
                            model.model_ref.provider.clone(),
                            model
                                .endpoint
                                .unwrap_or_else(ProviderEndpointId::default_endpoint),
                            model.model_ref.model.clone(),
                        )
                    }),
            );
        }
    }
    let registered = super::validation::ValidationEngine::new();
    let mut seen_associations = BTreeSet::new();
    for association in associations {
        if !seen_associations.insert((&association.models_dev_id, &association.route_registration))
        {
            return Err(format!(
                "duplicate endpoint association: {}",
                association.models_dev_id
            ));
        }
        if !registered.contains_route(&association.route_registration) {
            return Err(format!(
                "unregistered endpoint association: {}",
                association.route_registration
            ));
        }
        if association.owner.trim().is_empty()
            || association.region.trim().is_empty()
            || association.evidence.is_empty()
            || association.evidence.iter().any(|e| e.trim().is_empty())
            || chrono::NaiveDate::parse_from_str(&association.reviewed_at, "%Y-%m-%d").is_err()
        {
            return Err(format!(
                "incomplete endpoint association: {}",
                association.models_dev_id
            ));
        }
        let mut seen_models = BTreeSet::new();
        for model in &association.supported_models {
            if !seen_models.insert(model) {
                return Err(format!(
                    "duplicate supported model: {}/{model}",
                    association.route_registration
                ));
            }
            let route =
                ModelRouteRef::parse(&format!("{}/{model}", association.route_registration))
                    .map_err(|e| e.to_string())?;
            if route.model != *model
                || catalog.get(&route.model_ref()).is_none()
                || catalog.canonicalize_model_ref(&route.model_ref()) != route.model_ref()
            {
                return Err(format!(
                    "unsupported association model: {}",
                    route.as_string()
                ));
            }
            routes.push(route);
        }
    }
    routes.sort_by_key(ModelRouteRef::as_string);
    routes.dedup();
    let required_routes = routes.clone();
    let mappings = ProviderMapping::default_mappings();
    let overrides = HashMap::new();
    let discovered = HashMap::new();
    let mut rows = Vec::new();
    for route in routes {
        let model_ref = route.model_ref();
        let model = catalog
            .get(&model_ref)
            .ok_or_else(|| format!("missing model: {model_ref:?}"))?;
        // Keep the route's actual winning metadata separate from runtime defaults.
        let resolved = catalog.resolve_metadata(
            &model_ref,
            catalog.get_route(&route),
            Some(model),
            &overrides,
            &discovered,
            None,
            &ContextConfig::default(),
            8_192,
            Some(&route.endpoint),
        );
        let default_max_output_tokens = resolved.effective_default_max_output_tokens;
        let policy = resolved.into_policy();
        let effective = RuntimeFacts {
            context_window_tokens: policy.context_window_tokens,
            default_max_output_tokens,
            max_output_tokens_upper_limit: policy.max_output_tokens_upper_limit,
            prompt_budget_estimated_tokens: policy.prompt_budget_estimated_tokens,
            runtime_max_output_tokens: policy.runtime_max_output_tokens,
            capabilities: policy.capabilities.clone(),
            agent_turn: policy.agent_turn,
            decision_capable: policy.decision_capable,
            reasoning_effort_options: policy.reasoning_effort_options.clone(),
        };
        let sources = mappings
            .iter()
            .filter(|m| {
                m.holon_provider_id == route.provider.as_str()
                    && route.endpoint == ProviderEndpointId::default_endpoint()
            })
            .map(|m| m.models_dev_id.as_str())
            .chain(
                associations
                    .iter()
                    .filter(|a| {
                        a.route_registration
                            == format!("{}@{}", route.provider.as_str(), route.endpoint.as_str())
                            && (a.supported_models.is_empty()
                                || a.supported_models.contains(&route.model))
                    })
                    .map(|a| a.models_dev_id.as_str()),
            )
            .collect::<BTreeSet<_>>();
        let mut upstream_candidates = Vec::new();
        let mut missing_reasons = BTreeMap::new();
        for source in sources {
            let Some(upstream) = snapshot
                .providers
                .get(source)
                .and_then(|p| p.models.get(&route.model))
            else {
                if associations.iter().any(|a| {
                    a.models_dev_id == source
                        && a.route_registration
                            == format!("{}@{}", route.provider.as_str(), route.endpoint.as_str())
                        && a.supported_models.contains(&route.model)
                }) {
                    missing_reasons.insert(
                        format!("upstream:{source}"),
                        "reviewed_endpoint_upstream_model_missing".to_string(),
                    );
                }
                continue;
            };
            let context = upstream.limit.as_ref().and_then(|l| l.context);
            let output = upstream.limit.as_ref().and_then(|l| l.output);
            let mut differences = Vec::new();
            if context.filter(|n| *n > 0) != effective.context_window_tokens.map(|n| n as u64) {
                differences.push("context_window_tokens".to_string());
            }
            if output.filter(|n| *n > 0) != effective.max_output_tokens_upper_limit.map(u64::from) {
                differences.push("max_output_tokens_upper_limit".to_string());
            }
            upstream_candidates.push(UpstreamCandidate {
                models_dev_ref: format!("{source}/{}", upstream.id),
                context_window_tokens: context,
                max_output_tokens: output,
                differences,
            });
        }
        let aliases = catalog
            .aliases
            .iter()
            .filter_map(|(alias, _)| {
                (catalog.canonicalize_model_ref(alias) == model_ref).then(|| alias.as_string())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for (field, missing, upstream_present) in [
            (
                "context_window_tokens",
                effective.context_window_tokens.is_none(),
                upstream_candidates
                    .iter()
                    .any(|c| c.context_window_tokens.is_some_and(|n| n > 0)),
            ),
            (
                "max_output_tokens_upper_limit",
                effective.max_output_tokens_upper_limit.is_none(),
                upstream_candidates
                    .iter()
                    .any(|c| c.max_output_tokens.is_some_and(|n| n > 0)),
            ),
        ] {
            if missing {
                missing_reasons.insert(
                    field.to_string(),
                    if upstream_present {
                        "upstream_has_value_runtime_missing_review_required"
                    } else if upstream_candidates.is_empty() {
                        "no_exact_endpoint_upstream_evidence"
                    } else {
                        "upstream_also_unknown"
                    }
                    .to_string(),
                );
            }
        }
        let raw_route =
            serde_json::to_value(catalog.get_route(&route)).map_err(|e| e.to_string())?;
        rows.push(CoverageRow {
            route_ref: route,
            aliases,
            raw_model: serde_json::to_value(model).map_err(|e| e.to_string())?,
            raw_route,
            context_status: if effective.context_window_tokens.is_some_and(|n| n > 0) {
                LimitStatus::Known
            } else {
                LimitStatus::Unknown
            },
            output_status: if effective
                .max_output_tokens_upper_limit
                .is_some_and(|n| n > 0)
            {
                LimitStatus::Known
            } else {
                LimitStatus::Unknown
            },
            review: None,
            missing_reasons,
            effective,
            evidence: policy.evidence.clone(),
            upstream_candidates,
        });
    }
    let associated = associations
        .iter()
        .map(|a| a.models_dev_id.as_str())
        .collect::<BTreeSet<_>>();
    let unassociated_plan_providers = snapshot
        .providers
        .keys()
        .filter(|id| {
            (id.contains("plan") || id.contains("codex")) && !associated.contains(id.as_str())
        })
        .cloned()
        .collect();
    Ok(CoverageReport {
        schema_version: 1,
        scope: "checked_in_catalog_without_local_config_or_discovery".to_string(),
        required_routes,
        routes: rows,
        unassociated_plan_providers,
    })
}

/// A stale review cannot hide changed runtime facts or inherited input fields.
pub fn annotate_reviews(report: &mut CoverageReport, baseline: &CoverageBaseline) {
    let reviews = baseline
        .routes
        .iter()
        .map(|r| (r.route_ref.as_string(), r))
        .collect::<BTreeMap<_, _>>();
    for row in &mut report.routes {
        if let Some(review) = reviews
            .get(&row.route_ref.as_string())
            .filter(|r| r.facts == row.effective && r.inputs == row.reviewed_inputs())
        {
            row.context_status = review.context_status;
            row.output_status = review.output_status;
            row.review = review.review.clone();
        }
    }
}

pub fn check_coverage(report: &CoverageReport, baseline: &CoverageBaseline) -> Vec<String> {
    let mut errors = Vec::new();
    let mut present = BTreeSet::new();
    for row in &report.routes {
        if !present.insert(row.route_ref.as_string()) {
            errors.push(format!(
                "duplicate coverage route: {}",
                row.route_ref.as_string()
            ));
        }
    }
    for required in &report.required_routes {
        if !present.contains(&required.as_string()) {
            errors.push(format!(
                "{}: required route missing from coverage",
                required.as_string()
            ));
        }
    }
    if baseline.schema_version != 1 {
        errors.push("unsupported metadata review schema".to_string());
    }
    let mut reviewed = BTreeMap::new();
    for entry in &baseline.routes {
        if reviewed
            .insert(entry.route_ref.as_string(), entry)
            .is_some()
        {
            errors.push(format!("duplicate review: {}", entry.route_ref.as_string()));
        }
    }
    for row in &report.routes {
        let key = row.route_ref.as_string();
        let Some(entry) = reviewed.remove(&key) else {
            errors.push(format!("{key}: new route requires metadata review"));
            continue;
        };
        if row.effective != entry.facts {
            errors.push(format!("{key}: runtime facts changed; review values, empty-field regressions, and budget resolution"));
        }
        if row.reviewed_inputs() != entry.inputs {
            errors.push(format!("{key}: raw metadata or field sources changed; review inherited and empty-field regressions"));
        }
        for (field, value, status) in [
            (
                "context",
                row.effective.context_window_tokens.map(|n| n as u64),
                entry.context_status,
            ),
            (
                "output",
                row.effective.max_output_tokens_upper_limit.map(u64::from),
                entry.output_status,
            ),
        ] {
            if value == Some(0) || (value.is_some() != (status == LimitStatus::Known)) {
                errors.push(format!("{key}: invalid {field} status/value"));
            }
        }
        if entry.context_status != LimitStatus::Known || entry.output_status != LimitStatus::Known {
            let valid = entry.review.as_ref().is_some_and(|r| {
                !r.owner.trim().is_empty()
                    && !r.reason.trim().is_empty()
                    && !r.recheck.trim().is_empty()
                    && !r.evidence.is_empty()
                    && r.evidence.iter().all(|e| !e.trim().is_empty())
                    && chrono::NaiveDate::parse_from_str(&r.reviewed_at, "%Y-%m-%d").is_ok()
            });
            if !valid {
                errors.push(format!("{key}: unknown/dynamic/not_applicable capacity requires owner, date, evidence, reason, recheck"));
            }
        }
        if row.effective.agent_turn
            && (row.effective.prompt_budget_estimated_tokens == 0
                || row.effective.runtime_max_output_tokens == 0)
        {
            errors.push(format!(
                "{key}: agent-turn route lacks executable budget policy"
            ));
        }
    }
    errors.extend(
        reviewed
            .keys()
            .map(|key| format!("{key}: stale metadata review")),
    );
    errors
}

pub fn render_coverage_summary(report: &CoverageReport) -> String {
    let context = report
        .routes
        .iter()
        .filter(|r| r.effective.context_window_tokens.is_none())
        .count();
    let output = report
        .routes
        .iter()
        .filter(|r| r.effective.max_output_tokens_upper_limit.is_none())
        .count();
    let mut summary = format!(
        "\n## Route metadata review (offline)\n\nRoutes: {}; unknown context: {context}; unknown output ceiling: {output}.\n\
         Runtime output policy is not output capacity. Existing builtin fields are not automatically overwritten.\n\n\
         | Route | Upstream source | Fields requiring review |\n|---|---|---|\n",
        report.routes.len(),
    );
    for row in &report.routes {
        for candidate in &row.upstream_candidates {
            if !candidate.differences.is_empty() {
                summary.push_str(&format!(
                    "| `{}` | `{}` | {} |\n",
                    row.route_ref.as_string(),
                    candidate.models_dev_ref,
                    candidate.differences.join(", ")
                ));
            }
        }
    }
    summary.push_str(
        "\n### Retained unknown capacities (not declarations of provider limits)\n\n\
        | Route | Context status | Output status | Review reason |\n|---|---|---|---|\n",
    );
    for row in &report.routes {
        if !row.missing_reasons.is_empty() {
            let reason = row
                .review
                .as_ref()
                .map(|r| r.reason.clone())
                .unwrap_or_else(|| {
                    row.missing_reasons
                        .values()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; ")
                });
            summary.push_str(&format!(
                "| `{}` | {:?} | {:?} | {} |\n",
                row.route_ref.as_string(),
                row.context_status,
                row.output_status,
                reason.replace('|', "\\|").replace('\n', " ")
            ));
        }
    }
    let mut endpoints = BTreeMap::<String, usize>::new();
    for row in &report.routes {
        if row.route_ref.endpoint != ProviderEndpointId::default_endpoint() {
            *endpoints
                .entry(format!(
                    "{}@{}",
                    row.route_ref.provider.as_str(),
                    row.route_ref.endpoint.as_str()
                ))
                .or_default() += 1;
        }
    }
    summary
        .push_str("\n### Special endpoints: independent eligibility/capacity review required\n\n");
    for (endpoint, count) in endpoints {
        summary.push_str(&format!(
            "- `{endpoint}`: {count} checked-in routes; not proof of live subscription support.\n"
        ));
    }
    summary.push_str("\nUnassociated plan/Codex provider nodes (not automatically supported):\n");
    for id in &report.unassociated_plan_providers {
        summary.push_str(&format!("- `{id}`\n"));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ModelRef, model_catalog::models_dev::parse_snapshot};

    #[test]
    fn models_dev_coverage_native_upstream_values_expose_missing_runtime_fields() {
        let mut catalog = BuiltInModelCatalog::new();
        let model = catalog
            .entries
            .get_mut(&ModelRef::parse("deepseek/deepseek-flash").unwrap())
            .unwrap();
        model.context_window_tokens = None;
        model.max_output_tokens_upper_limit = None;
        let snapshot = parse_snapshot(
            r#"{
            "deepseek":{"id":"deepseek","models":{
                "deepseek-flash":{"id":"deepseek-flash","limit":{"context":1048576,"output":393216}}
            }}
        }"#,
        )
        .unwrap();
        let report =
            generate_coverage(&snapshot, &catalog, &endpoint_associations().unwrap()).unwrap();
        for endpoint in ["default", "responses"] {
            let row = report
                .routes
                .iter()
                .find(|r| r.route_ref.as_string() == format!("deepseek@{endpoint}/deepseek-flash"))
                .unwrap();
            for field in ["context_window_tokens", "max_output_tokens_upper_limit"] {
                assert_eq!(
                    row.missing_reasons[field],
                    "upstream_has_value_runtime_missing_review_required"
                );
                assert!(row.upstream_candidates[0]
                    .differences
                    .iter()
                    .any(|d| d == field));
            }
        }
    }
}

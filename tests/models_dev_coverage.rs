use holon::model_catalog::{
    models_dev::{
        coverage::{
            annotate_reviews, check_coverage, endpoint_associations, generate_coverage,
            CoverageBaseline, CoverageReport, LimitStatus, ReviewedRoute, UnknownReview,
        },
        parse_snapshot,
    },
    BuiltInModelCatalog,
};

fn report() -> CoverageReport {
    let mut report = generate_coverage(
        &parse_snapshot("{}").unwrap(),
        &BuiltInModelCatalog::new(),
        &[],
    )
    .unwrap();
    report.routes.truncate(1);
    report
}

#[test]
fn models_dev_coverage_inherited_values_cannot_hide_raw_or_source_regressions() {
    use holon::model_catalog::{ModelMetadataField, ModelMetadataOrigin};

    let mut report = report();
    report.routes[0].raw_route = serde_json::json!({"context_window_tokens": 128000});
    report.routes[0].effective.context_window_tokens = None;
    report.routes[0].context_status = LimitStatus::Dynamic;
    let reviewed = baseline(&report);
    assert!(check_coverage(&report, &reviewed).is_empty());

    // Effective facts stay unchanged when a removed field inherits the same value.
    report.routes[0].raw_route = serde_json::json!({});
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("raw metadata")));
    report.routes[0].context_status = LimitStatus::Unknown;
    annotate_reviews(&mut report, &reviewed);
    assert_eq!(report.routes[0].context_status, LimitStatus::Unknown);

    report.routes[0].raw_route = serde_json::json!({"context_window_tokens": 128000});
    report.routes[0].evidence.fields.insert(
        ModelMetadataField::ContextWindowTokens,
        ModelMetadataOrigin::UnknownFallback,
    );
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("field sources")));

    report.routes[0].evidence = reviewed.routes[0].inputs.evidence.clone();
    report.routes[0].raw_model["description"] = serde_json::json!("Copy edit only.");
    assert!(check_coverage(&report, &reviewed).is_empty());
}

#[test]
fn models_dev_coverage_summary_keeps_current_review_status_but_not_stale_reviews() {
    use holon::model_catalog::models_dev::coverage::render_coverage_summary;

    let mut report = report();
    report.routes[0].effective.context_window_tokens = None;
    report.routes[0].effective.max_output_tokens_upper_limit = None;
    report.routes[0].context_status = LimitStatus::Unknown;
    report.routes[0].output_status = LimitStatus::Unknown;
    report.routes[0]
        .missing_reasons
        .insert("context".into(), "generic_unknown".into());
    let mut reviewed = baseline(&report);
    reviewed.routes[0].context_status = LimitStatus::Dynamic;
    reviewed.routes[0].output_status = LimitStatus::NotApplicable;
    reviewed.routes[0].review.as_mut().unwrap().reason = "Reviewed dynamic selector.".into();
    annotate_reviews(&mut report, &reviewed);
    let summary = render_coverage_summary(&report);
    assert!(summary.contains("Dynamic | NotApplicable | Reviewed dynamic selector."));

    report.routes[0].effective.prompt_budget_estimated_tokens += 1;
    report.routes[0].context_status = LimitStatus::Unknown;
    report.routes[0].output_status = LimitStatus::Unknown;
    report.routes[0].review = None;
    annotate_reviews(&mut report, &reviewed);
    assert!(report.routes[0].review.is_none());
    assert!(!render_coverage_summary(&report).contains("Reviewed dynamic selector."));
}

fn baseline(report: &CoverageReport) -> CoverageBaseline {
    CoverageBaseline {
        schema_version: 1,
        routes: report.routes.iter().map(|row| ReviewedRoute {
            route_ref: row.route_ref.clone(),
            facts: row.effective.clone(),
            inputs: row.reviewed_inputs(),
            context_status: row.context_status,
            output_status: row.output_status,
            review: Some(UnknownReview {
                owner: "catalog-maintainer".to_string(),
                reviewed_at: "2026-10-25".to_string(),
                reason: "Exact endpoint capacity remains unconfirmed; internal budgets are not capacity.".to_string(),
                recheck: "On the next endpoint documentation or catalog update.".to_string(),
                evidence: vec!["models.dev/endpoint-review.md".to_string()],
            }),
        }).collect(),
    }
}

#[test]
fn models_dev_coverage_changed_or_new_route_requires_review() {
    let mut report = report();
    let reviewed = baseline(&report);
    assert!(check_coverage(&report, &reviewed).is_empty());
    report.routes[0].effective.context_window_tokens = None;
    assert!(!check_coverage(&report, &reviewed).is_empty());
    report.routes[0].route_ref.model.push_str("-new-id");
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("new route")));
}

#[test]
fn models_dev_coverage_missing_limits_need_independent_documented_status_and_policy() {
    let mut report = report();
    report.routes[0].effective.context_window_tokens = None;
    report.routes[0].context_status = LimitStatus::Unknown;
    let mut reviewed = baseline(&report);
    assert!(check_coverage(&report, &reviewed).is_empty());
    reviewed.routes[0].context_status = LimitStatus::Dynamic;
    assert!(check_coverage(&report, &reviewed).is_empty());
    annotate_reviews(&mut report, &reviewed);
    assert_eq!(report.routes[0].context_status, LimitStatus::Dynamic);
    reviewed.routes[0].review = None;
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("requires owner")));
    report.routes[0].effective.context_window_tokens = Some(0);
    reviewed = baseline(&report);
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("status/value")));
    report.routes[0].effective.context_window_tokens = None;
    report.routes[0].effective.agent_turn = true;
    report.routes[0].effective.prompt_budget_estimated_tokens = 0;
    reviewed = baseline(&report);
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("executable budget")));
}

#[test]
fn models_dev_coverage_duplicate_stale_and_undocumented_reviews_are_rejected() {
    let report = report();
    let mut reviewed = baseline(&report);
    reviewed.routes.push(reviewed.routes[0].clone());
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("duplicate")));
    reviewed.routes.truncate(1);
    reviewed.routes[0].route_ref.model.push_str("-stale");
    assert!(check_coverage(&report, &reviewed)
        .iter()
        .any(|e| e.contains("stale")));
}

#[test]
fn models_dev_coverage_plan_association_never_borrows_generic_or_other_region_limits() {
    let snapshot = parse_snapshot(r#"{
        "alibaba":{"id":"alibaba","models":{
            "deepseek-v4.1-flash-1017":{"id":"deepseek-v4.1-flash-1017","limit":{"context":111,"output":11}}
        }},
        "alibaba-token-plan-cn":{"id":"alibaba-token-plan-cn","models":{
            "deepseek-v4.1-flash-1017":{"id":"deepseek-v4.1-flash-1017","limit":{"context":1000000,"output":384000}}
        }},
        "alibaba-token-plan":{"id":"alibaba-token-plan","models":{
            "deepseek-v4.1-flash-1017":{"id":"deepseek-v4.1-flash-1017","limit":{"context":222,"output":22}}
        }}
    }"#).unwrap();
    let catalog = BuiltInModelCatalog::new();
    let report = generate_coverage(&snapshot, &catalog, &endpoint_associations().unwrap()).unwrap();
    let row = report
        .routes
        .iter()
        .find(|r| r.route_ref.as_string() == "dashscope@token-plan/deepseek-v4.1-flash-1017")
        .unwrap();
    assert_eq!(row.upstream_candidates.len(), 1);
    assert_eq!(
        row.upstream_candidates[0].context_window_tokens,
        Some(1_000_000)
    );
    assert_eq!(
        row.upstream_candidates[0].models_dev_ref,
        "alibaba-token-plan-cn/deepseek-v4.1-flash-1017"
    );
    assert!(report
        .unassociated_plan_providers
        .contains(&"alibaba-token-plan".to_string()));
    assert_eq!(
        catalog
            .get(&row.route_ref.model_ref())
            .unwrap()
            .context_window_tokens,
        row.raw_model["context_window_tokens"]
            .as_u64()
            .map(|n| n as usize)
    );
}

#[test]
fn models_dev_coverage_associations_need_registered_reviewed_endpoints() {
    let mut associations = endpoint_associations().unwrap();
    associations[0].route_registration = "dashscope@invented-plan".to_string();
    assert!(generate_coverage(
        &parse_snapshot("{}").unwrap(),
        &BuiltInModelCatalog::new(),
        &associations
    )
    .unwrap_err()
    .contains("unregistered"));
}

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
    report.required_routes = vec![report.routes[0].route_ref.clone()];
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

#[test]
fn models_dev_coverage_reviewed_compatibility_routes_are_complete_and_endpoint_scoped() {
    use holon::config::ModelRouteRef;

    let snapshot = parse_snapshot(
        r#"{
        "deepseek":{"id":"deepseek","models":{
            "deepseek-flash":{"id":"deepseek-flash","limit":{"context":1048576,"output":393216}}
        }},
        "alibaba-token-plan-cn":{"id":"alibaba-token-plan-cn","models":{
            "deepseek-flash":{"id":"deepseek-flash","limit":{"context":111,"output":11}}
        }}
    }"#,
    )
    .unwrap();
    let catalog = BuiltInModelCatalog::new();
    let before = catalog.clone();
    let associations = endpoint_associations().unwrap();
    let report = generate_coverage(&snapshot, &catalog, &associations).unwrap();
    for association in &associations {
        for model in &association.supported_models {
            let expected = format!("{}/{model}", association.route_registration);
            assert_eq!(
                report
                    .routes
                    .iter()
                    .filter(|r| r.route_ref.as_string() == expected)
                    .count(),
                1,
                "{expected}"
            );
        }
    }
    for (endpoint, efforts) in [
        ("default", vec!["low", "high", "max"]),
        (
            "responses",
            vec!["none", "minimal", "low", "medium", "high", "xhigh", "max"],
        ),
    ] {
        let route = ModelRouteRef::parse(&format!("deepseek@{endpoint}/deepseek-flash")).unwrap();
        let row = report.routes.iter().find(|r| r.route_ref == route).unwrap();
        assert_eq!(row.upstream_candidates.len(), 1);
        let candidate = &row.upstream_candidates[0];
        assert_eq!(candidate.models_dev_ref, "deepseek/deepseek-flash");
        assert_eq!(candidate.context_window_tokens, Some(1_048_576));
        assert_eq!(candidate.max_output_tokens, Some(393_216));
        assert_eq!(
            candidate.differences,
            ["context_window_tokens", "max_output_tokens_upper_limit"]
        );
        assert_eq!(row.effective.context_window_tokens, Some(1_000_000));
        assert_eq!(row.effective.max_output_tokens_upper_limit, Some(384_000));
        assert_eq!(row.effective.reasoning_effort_options, efforts);
        // Both protocols now document image input; retain each endpoint's own
        // resolver result, not capabilities copied from an upstream candidate.
        assert!(row.effective.capabilities.image_input);
        if endpoint == "default" {
            assert!(row.raw_route.is_null());
            assert!(catalog.get_route(&route).is_none());
        }
    }
    assert_eq!(catalog, before);
    assert_eq!(
        catalog
            .preferred_route_for_model(
                &ModelRouteRef::parse("deepseek@default/deepseek-flash")
                    .unwrap()
                    .model_ref()
            )
            .unwrap()
            .as_string(),
        "deepseek@responses/deepseek-flash"
    );
    // A Responses registration is not evidence for arbitrary default routes.
    assert!(!report
        .routes
        .iter()
        .any(|r| r.route_ref.as_string() == "dashscope@default/deepseek-v4.1-flash-1017"));
}

#[test]
fn models_dev_coverage_missing_required_route_fails_even_with_matching_baseline() {
    let mut report = generate_coverage(
        &parse_snapshot("{}").unwrap(),
        &BuiltInModelCatalog::new(),
        &endpoint_associations().unwrap(),
    )
    .unwrap();
    let reviewed = baseline(&report);
    assert!(check_coverage(&report, &reviewed).is_empty());
    report
        .routes
        .retain(|r| r.route_ref.as_string() != "deepseek@default/deepseek-flash");
    let matching_but_incomplete = baseline(&report);
    assert!(check_coverage(&report, &matching_but_incomplete)
        .iter()
        .any(|e| e == "deepseek@default/deepseek-flash: required route missing from coverage"));
    report.routes.push(report.routes[0].clone());
    assert!(check_coverage(&report, &matching_but_incomplete)
        .iter()
        .any(|e| e.contains("duplicate coverage route")));
}

#[test]
fn models_dev_coverage_supported_models_require_exact_existing_models() {
    let snapshot = parse_snapshot("{}").unwrap();
    let catalog = BuiltInModelCatalog::new();
    let mut associations = endpoint_associations().unwrap();
    let native = associations
        .iter_mut()
        .find(|a| a.route_registration == "deepseek@default")
        .unwrap();
    native
        .supported_models
        .push("deepseek-flash-preview".into());
    assert!(generate_coverage(&snapshot, &catalog, &associations)
        .unwrap_err()
        .contains("unsupported association model"));
    associations
        .iter_mut()
        .find(|a| a.route_registration == "deepseek@default")
        .unwrap()
        .supported_models = vec!["deepseek-flash".into(), "deepseek-flash".into()];
    assert!(generate_coverage(&snapshot, &catalog, &associations)
        .unwrap_err()
        .contains("duplicate supported model"));
}

#[test]
fn models_dev_coverage_missing_reviewed_upstream_is_explicit() {
    let report = generate_coverage(
        &parse_snapshot("{}").unwrap(),
        &BuiltInModelCatalog::new(),
        &endpoint_associations().unwrap(),
    )
    .unwrap();
    let row = report
        .routes
        .iter()
        .find(|r| r.route_ref.as_string() == "deepseek@default/deepseek-flash")
        .unwrap();
    assert_eq!(
        row.missing_reasons["upstream:deepseek"],
        "reviewed_endpoint_upstream_model_missing"
    );
    assert!(row.upstream_candidates.is_empty());
}

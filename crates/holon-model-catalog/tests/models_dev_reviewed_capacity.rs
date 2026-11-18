use holon_domain::model_values::{ModelRef, ModelRouteRef};
use holon_model_catalog::{
    models_dev::{
        coverage::{endpoint_associations, generate_coverage},
        parse_snapshot,
    },
    BuiltInModelCatalog,
};

#[test]
fn seed_character_routes_keep_unconfirmed_capacity_unknown() {
    let report = generate_coverage(
        &parse_snapshot("{}").unwrap(),
        &BuiltInModelCatalog::new(),
        &endpoint_associations().unwrap(),
    )
    .unwrap();
    for route in [
        "volcengine@default/doubao-seed-character-260628",
        "volcengine@plan/doubao-seed-character-260628",
    ] {
        let row = report
            .routes
            .iter()
            .find(|row| row.route_ref.as_string() == route)
            .unwrap_or_else(|| panic!("missing route {route}"));
        for field in [
            "context_window_tokens",
            "auto_compact_token_limit",
            "default_max_output_tokens",
            "max_output_tokens_upper_limit",
        ] {
            assert!(row.raw_model[field].is_null(), "{route} {field}");
        }
        assert_eq!(row.effective.context_window_tokens, None, "{route}");
        assert_eq!(row.effective.default_max_output_tokens, None, "{route}");
        assert_eq!(row.effective.max_output_tokens_upper_limit, None, "{route}");
    }
}

#[test]
fn models_dev_reviewed_capacity_fills_exact_routes_without_changing_output_policy() {
    let catalog = BuiltInModelCatalog::new();
    let report = generate_coverage(
        &parse_snapshot("{}").unwrap(),
        &catalog,
        &endpoint_associations().unwrap(),
    )
    .unwrap();
    for (route, context, output) in [
        ("dashscope@default/qwen-plus-latest", 1_000_000, 32_768),
        ("dashscope@default/qwen-flash-2025-07-28", 1_000_000, 32_768),
        (
            "dashscope@default/qwen3.5-flash-2026-02-23",
            1_000_000,
            65_536,
        ),
        (
            "dashscope@default/qwen3.5-plus-2026-02-15",
            1_000_000,
            65_536,
        ),
        (
            "dashscope@default/qwen3.5-plus-2026-04-20",
            1_000_000,
            65_536,
        ),
        (
            "dashscope@default/qwen3.6-flash-2026-04-16",
            1_000_000,
            65_536,
        ),
        (
            "dashscope@default/qwen3.6-plus-2026-04-02",
            1_000_000,
            65_536,
        ),
        (
            "dashscope@default/qwen3.7-flash-2026-07-15",
            1_000_000,
            131_072,
        ),
        (
            "dashscope@token-plan/deepseek-v4.1-flash-1017",
            1_000_000,
            384_000,
        ),
        ("deepseek@responses/deepseek-flash", 1_000_000, 384_000),
        ("deepseek@default/deepseek-flash", 1_000_000, 384_000),
        ("openai@default/gpt-5.4", 272_000, 128_000),
        ("openai@default/gpt-5.4-mini", 128_000, 128_000),
    ] {
        let row = report
            .routes
            .iter()
            .find(|row| row.route_ref.as_string() == route)
            .unwrap_or_else(|| panic!("missing route {route}"));
        assert_eq!(
            row.effective.context_window_tokens,
            Some(context),
            "{route}"
        );
        assert_eq!(
            row.effective.max_output_tokens_upper_limit,
            Some(output),
            "{route}"
        );
        assert_eq!(row.effective.default_max_output_tokens, None, "{route}");
        assert!(row.effective.runtime_max_output_tokens < output, "{route}");
    }

    let dynamic = report
        .routes
        .iter()
        .find(|row| row.route_ref.as_string() == "dashscope@token-plan/auto")
        .unwrap();
    assert_eq!(dynamic.effective.context_window_tokens, None);
    assert_eq!(dynamic.effective.max_output_tokens_upper_limit, None);

    // Coverage includes the reviewed compatibility route without manufacturing
    // pinned runtime metadata or changing the preferred Responses endpoint.
    assert!(catalog
        .get_route(&ModelRouteRef::parse("deepseek@default/deepseek-flash").unwrap())
        .is_none());
    for endpoint in ["default", "coding-plan", "coding-plan-intl"] {
        assert!(catalog
            .get_route(
                &ModelRouteRef::parse(&format!("dashscope@{endpoint}/deepseek-v4.1-flash-1017"))
                    .unwrap()
            )
            .is_none());
    }
    let tencent = catalog
        .get(&ModelRef::parse("tencent-tokenhub/deepseek-v4.1-flash-1017").unwrap())
        .unwrap();
    assert_eq!(tencent.context_window_tokens, None);
    assert_eq!(tencent.max_output_tokens_upper_limit, None);
}

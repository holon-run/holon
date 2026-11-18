use super::*;
use crate::models_dev::parse_snapshot;
use holon_domain::model_values::ModelRef;

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
    let report = generate_coverage(&snapshot, &catalog, &endpoint_associations().unwrap()).unwrap();
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

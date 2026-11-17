use super::*;
use crate::tool::helpers::capture_tool_input_coercion;
use crate::tool::ToolError;

fn shapes(items: Value) -> Vec<Value> {
    shapes_with_defaults(json!({"items": items, "login": "false"}))
}

#[test]
fn malformed_defaults_do_not_hide_recovered_item_counts() {
    for (field, value) in [
        ("stop_on_error", json!("invalid")),
        ("workdir", json!([])),
        ("shell", json!({})),
        ("login", json!("invalid")),
        ("yield_time_ms", json!(-1)),
        ("max_output_tokens", json!("invalid")),
    ] {
        for count in [0, 12, 13] {
            let item = if count == 12 {
                json!({"cmd": "printf ok"})
            } else {
                json!({})
            };
            let mut raw = json!({"items": vec![item; count]});
            raw[field] = value.clone();
            for input in shapes_with_defaults(raw) {
                assert_eq!(batch_item_count(&input), Some(count), "{input}");
                let error = parse_batch_args(&input).unwrap_err();
                let error = error.downcast_ref::<ToolError>().unwrap();
                assert_eq!(error.kind, "invalid_tool_input");
                let details = error.details.as_ref().unwrap();
                match count {
                    0 => assert_eq!(details["validation_error"], "must not be empty"),
                    13 => {
                        assert_eq!(details["validation_error"], "too_many_items");
                        assert_eq!(details["actual_items"], 13);
                        assert!(details.get("parse_error").is_none());
                    }
                    _ => assert!(details.get("parse_error").is_some(), "{input}"),
                }
            }
        }
    }
}

fn shapes_with_defaults(raw: Value) -> Vec<Value> {
    let mut string_items = raw.clone();
    string_items["items"] = json!(raw["items"].to_string());
    let mut shapes = vec![raw.clone(), string_items.clone(), json!(raw.to_string())];
    for key in ["arguments", "parameters", "params", "input"] {
        shapes.push(json!({key: raw}));
        shapes.push(json!({key: raw.to_string()}));
        let mut mixed = string_items.clone();
        mixed[key] = json!({"items": vec![json!({}); 13], "login": true});
        shapes.push(mixed);
    }
    shapes
}

#[test]
fn recovered_shape_and_typed_parser_agree_at_twelve_items() {
    for input in shapes(json!(vec![json!({"cmd": "printf ok"}); 12])) {
        assert_eq!(batch_item_count(&input), Some(12), "{input}");
        let args = parse_batch_args(&input).unwrap();
        assert_eq!(args.items.len(), 12);
        assert_eq!(args.login, Some(false));
        assert!(args.items.iter().all(|item| item.cmd == "printf ok"));
    }
}

#[test]
fn recovered_thirteen_items_are_rejected_before_malformed_item_parsing() {
    for input in shapes(json!(vec![json!({}); 13])) {
        assert_eq!(batch_item_count(&input), Some(13), "{input}");
        let error = parse_batch_args(&input).unwrap_err();
        let error = error.downcast_ref::<ToolError>().unwrap();
        assert_eq!(error.kind, "invalid_tool_input");
        let details = error.details.as_ref().unwrap();
        assert_eq!(details["validation_error"], "too_many_items");
        assert_eq!(details["actual_items"], 13);
        assert!(details.get("parse_error").is_none());
    }
}

#[test]
fn recovered_empty_batches_keep_the_existing_shape_error() {
    for input in shapes(json!([])) {
        let error = parse_batch_args(&input).unwrap_err();
        let error = error.downcast_ref::<ToolError>().unwrap();
        assert_eq!(error.kind, "invalid_tool_input");
        assert_eq!(
            error.details.as_ref().unwrap()["validation_error"],
            "must not be empty"
        );
    }
}

#[test]
fn unrecoverable_and_invalid_inputs_keep_typed_error_priority_and_hints() {
    for input in [
        json!({}),
        json!({"items": null}),
        json!({"items": "[not json]"}),
        json!({"arguments": {"workdir": "."}}),
        json!({"arguments": {"items": []}, "params": {"items": []}}),
        json!({"arguments": {"items": [{"cmd": "printf ok"}]}, "unknown": true}),
        json!({"params": {"items": [{"cmd": "printf ok"}]}, "items": [{}]}),
    ] {
        let expected =
            parse_tool_args_with_recovery_hint::<ExecCommandBatchArgs, _>(NAME, &input, || {
                exec_command_batch_recovery_hint(&input)
            })
            .unwrap_err();
        let actual = parse_batch_args(&input).unwrap_err();
        let expected = expected.downcast_ref::<ToolError>().unwrap();
        let actual = actual.downcast_ref::<ToolError>().unwrap();
        assert_eq!(actual.kind, expected.kind, "{input}");
        assert_eq!(actual.details, expected.details, "{input}");
        assert_eq!(actual.recovery_hint, expected.recovery_hint, "{input}");
    }
}

#[tokio::test]
async fn shape_inspection_does_not_record_execution_coercion() {
    let input = json!({"arguments": {"items": "[{\"cmd\":\"printf ok\"}]"}});
    let (count, coercion) = capture_tool_input_coercion(async { batch_item_count(&input) }).await;
    assert_eq!(count, Some(1));
    assert_eq!(coercion, None);
    let (args, coercion) = capture_tool_input_coercion(async { parse_batch_args(&input) }).await;
    assert_eq!(args.unwrap().items.len(), 1);
    assert!(coercion.is_some());
}

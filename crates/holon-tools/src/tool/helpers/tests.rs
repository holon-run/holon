use super::*;

#[derive(Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EnvelopeArgs {
    cmd: String,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    workdir: Option<String>,
}

#[test]
fn parse_tool_args_recovers_known_object_and_string_envelopes() {
    for envelope_key in TOOL_INPUT_ENVELOPE_KEYS {
        let direct = json!({
            envelope_key: {"cmd": "printf ok"},
            "max_output_tokens": "800"
        });
        let parsed: EnvelopeArgs = parse_tool_args("TestTool", &direct).unwrap();
        assert_eq!(
            parsed,
            EnvelopeArgs {
                cmd: "printf ok".into(),
                max_output_tokens: Some(800),
                workdir: None,
            }
        );

        let string = json!({
            envelope_key: "{\"cmd\":\"printf ok\",\"max_output_tokens\":\"900\"}"
        });
        let parsed: EnvelopeArgs = parse_tool_args("TestTool", &string).unwrap();
        assert_eq!(
            parsed,
            EnvelopeArgs {
                cmd: "printf ok".into(),
                max_output_tokens: Some(900),
                workdir: None,
            }
        );
    }
}

#[test]
fn parse_tool_args_prefers_explicit_outer_fields_when_unwrapping() {
    let parsed: EnvelopeArgs = parse_tool_args(
        "TestTool",
        &json!({
            "arguments": {
                "cmd": "inner",
                "workdir": "inner-dir"
            },
            "cmd": "outer",
            "max_output_tokens": "800"
        }),
    )
    .unwrap();

    assert_eq!(parsed.cmd, "outer");
    assert_eq!(parsed.workdir.as_deref(), Some("inner-dir"));
    assert_eq!(parsed.max_output_tokens, Some(800));
}

#[test]
fn parse_tool_args_does_not_recover_multiple_or_non_object_envelopes() {
    for input in [
        json!({
            "arguments": {"cmd": "printf arguments"},
            "params": {"cmd": "printf params"}
        }),
        json!({"arguments": ["printf", "ok"]}),
        json!({"arguments": "printf ok"}),
    ] {
        let error = parse_tool_args::<EnvelopeArgs>("TestTool", &input).unwrap_err();
        let error = error.downcast_ref::<ToolError>().expect("tool error");
        assert_eq!(error.kind, "invalid_tool_input");
    }
}

#[test]
fn parse_tool_args_does_not_unwrap_a_recognized_object_field() {
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegitimateInputArgs {
        #[allow(dead_code)]
        input: Value,
        #[allow(dead_code)]
        required: String,
    }

    let error = parse_tool_args::<LegitimateInputArgs>(
        "TestTool",
        &json!({"input": {"required": "must stay nested"}}),
    )
    .unwrap_err();
    let error = error.downcast_ref::<ToolError>().expect("tool error");

    assert!(error
        .details
        .as_ref()
        .and_then(|details| details["parse_error"].as_str())
        .is_some_and(|message| message.contains("missing field `required`")));
}

#[tokio::test]
async fn failed_envelope_candidate_preserves_first_parse_error_and_hint() {
    let input = json!({"arguments": {"workdir": "."}});
    let (result, input_coercion) =
        capture_tool_input_coercion(async { parse_tool_args::<EnvelopeArgs>("TestTool", &input) })
            .await;
    let error = result.unwrap_err();
    let error = error.downcast_ref::<ToolError>().expect("tool error");
    let details = error.details.as_ref().expect("details");

    assert_eq!(input_coercion, None);
    assert!(details["parse_error"]
        .as_str()
        .expect("parse error")
        .contains("unknown field `arguments`"));
    let hint = error.recovery_hint.as_deref().expect("recovery hint");
    assert!(hint.contains("remove the top-level `arguments` envelope"));
    assert!(hint.contains("accepted top-level fields"));
}

#[test]
fn unknown_field_hint_names_unsupported_and_accepted_fields() {
    let error =
        parse_tool_args::<EnvelopeArgs>("TestTool", &json!({"cmd": "printf ok", "work_dir": "."}))
            .unwrap_err();
    let error = error.downcast_ref::<ToolError>().expect("tool error");
    let hint = error.recovery_hint.as_deref().expect("recovery hint");

    assert!(hint.contains("remove unsupported top-level field `work_dir`"));
    assert!(hint.contains("`cmd`"));
    assert!(hint.contains("`workdir`"));
}

#[test]
fn invalid_structured_json_reports_bounded_path_and_location_without_value() {
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StructuredArgs {
        #[allow(dead_code)]
        items: Vec<String>,
    }

    let error =
        parse_tool_args::<StructuredArgs>("TestTool", &json!({"items": "[\"SECRET_MARKER\",]"}))
            .unwrap_err();
    let error = error.downcast_ref::<ToolError>().expect("tool error");
    let details = error.details.as_ref().expect("details");
    let diagnostics = details["string_json_parse_errors"]
        .as_array()
        .expect("string JSON diagnostics");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["path"], "/items");
    assert!(diagnostics[0]["line"].as_u64().unwrap() >= 1);
    assert!(diagnostics[0]["column"].as_u64().unwrap() >= 1);
    assert!(!details.to_string().contains("SECRET_MARKER"));
}

#[tokio::test]
async fn input_coercion_capture_records_actual_recovery_without_values() {
    let input = json!({
        "arguments": "{\"cmd\":\"SECRET_MARKER\"}",
        "max_output_tokens": "800"
    });
    let (parsed, coercion) =
        capture_tool_input_coercion(async { parse_tool_args::<EnvelopeArgs>("TestTool", &input) })
            .await;
    assert_eq!(parsed.unwrap().cmd, "SECRET_MARKER");
    let coercion = coercion.expect("input coercion");
    let value = serde_json::to_value(coercion).unwrap();

    assert_eq!(value["kind"], "unwrap_tool_input_envelope");
    assert_eq!(value["envelope_key"], "arguments");
    assert_eq!(
        value["outer_keys"],
        json!(["arguments", "max_output_tokens"])
    );
    assert_eq!(value["inner_keys"], json!(["cmd"]));
    assert!(!value.to_string().contains("SECRET_MARKER"));
}

#[tokio::test]
async fn input_coercion_capture_does_not_infer_from_directly_accepted_input() {
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegitimateInputArgs {
        input: Value,
    }

    let input = json!({"input": {"value": "nested"}});
    let (parsed, input_coercion) = capture_tool_input_coercion(async {
        parse_tool_args::<LegitimateInputArgs>("TestTool", &input)
    })
    .await;

    assert_eq!(parsed.unwrap().input, json!({"value": "nested"}));
    assert_eq!(input_coercion, None);
}

#[test]
fn output_budget_defaults_to_command_tool_default() {
    assert_eq!(output_char_budget(None), 32_000);
}

#[test]
fn effective_tool_output_tokens_defaults_and_clamps() {
    assert_eq!(effective_tool_output_tokens(None, 8_000, 64_000), 8_000);
    assert_eq!(effective_tool_output_tokens(Some(0), 8_000, 64_000), 8_000);
    assert_eq!(
        effective_tool_output_tokens(Some(100_000), 8_000, 64_000),
        64_000
    );
}

#[test]
fn command_cost_diagnostics_reports_long_inline_commands_without_full_echo() {
    let cmd = format!(
        "python - <<'PY'\n{}FINAL_SECRET_MARKER\nPY",
        "print('secret')\n".repeat(400)
    );
    let diagnostics = command_cost_diagnostics(&cmd, 2_000);

    assert!(diagnostics.contains_heredoc);
    assert!(diagnostics.contains_inline_script);
    assert!(diagnostics.exceeds_soft_threshold);
    assert_eq!(diagnostics.effective_max_output_tokens, 2_000);
    assert_eq!(diagnostics.output_char_budget, 8_000);
    assert_eq!(
        diagnostics.cmd_preview,
        "[omitted: command contains heredoc or inline script]"
    );
    assert!(!diagnostics.cmd_preview.contains("FINAL_SECRET_MARKER"));
    assert_eq!(command_display(&cmd), "python - <<'PY'\nprint('secret')");
}

#[test]
fn command_preview_redacts_common_secret_shapes() {
    let preview =
        command_preview("TOKEN=abc123 curl --password hunter2 https://user:pass@example.com/path");

    assert!(preview.contains("TOKEN=[redacted]"));
    assert!(preview.contains("--password [redacted]"));
    assert!(preview.contains("https://[redacted]@example.com/path"));
    assert!(!preview.contains("abc123"));
    assert!(!preview.contains("hunter2"));
    assert!(!preview.contains("user:pass"));
}

#[test]
fn command_display_keeps_full_non_script_command_with_redaction() {
    let display = command_display(
        "TOKEN=abc123 cargo test --all-targets -- --exact some_really_long_test_name",
    );

    assert!(display.contains("TOKEN=[redacted]"));
    assert!(display.contains("cargo test --all-targets -- --exact some_really_long_test_name"));
    assert!(!display.contains("abc123"));
}

#[test]
fn coerce_string_to_number_integer() {
    assert_eq!(coerce_string("42"), Some(Value::Number(42i64.into())));
    assert_eq!(coerce_string("0"), Some(Value::Number(0i64.into())));
    assert_eq!(coerce_string("-7"), Some(Value::Number((-7i64).into())));
}

#[test]
fn coerce_string_to_number_float() {
    let result = coerce_string("4.567");
    assert_eq!(
        result,
        Some(Value::Number(serde_json::Number::from_f64(4.567).unwrap()))
    );
}

#[test]
fn coerce_integral_decimal_strings_to_integer() {
    assert_eq!(
        coerce_string("900000.0"),
        Some(Value::Number(900000i64.into()))
    );
    assert_eq!(coerce_string("-3.0"), Some(Value::Number((-3i64).into())));
    assert_eq!(coerce_string("1e3"), Some(Value::Number(1000i64.into())));
    assert_eq!(coerce_string("1.23e3"), Some(Value::Number(1230i64.into())));
    assert_eq!(
        coerce_string("9007199254740993.0"),
        Some(Value::Number(9007199254740993i64.into()))
    );
    assert_eq!(
        coerce_string("9223372036854775807.0"),
        Some(Value::Number(i64::MAX.into()))
    );
    assert_eq!(
        coerce_string("-9223372036854775808.0"),
        Some(Value::Number(i64::MIN.into()))
    );
}

#[test]
fn coerce_non_integral_decimal_string_stays_float() {
    let value = 1e-1f64;
    assert_eq!(
        coerce_string("1e-1"),
        Some(Value::Number(serde_json::Number::from_f64(value).unwrap()))
    );
}

#[test]
fn coerce_out_of_range_integral_decimal_string_stays_float() {
    let value = 9223372036854775808.0f64;
    assert_eq!(
        coerce_string("9223372036854775808.0"),
        Some(Value::Number(serde_json::Number::from_f64(value).unwrap()))
    );
}

#[test]
fn coerce_string_to_bool() {
    assert_eq!(coerce_string("true"), Some(Value::Bool(true)));
    assert_eq!(coerce_string("false"), Some(Value::Bool(false)));
    assert_eq!(coerce_string("TRUE"), Some(Value::Bool(true)));
    assert_eq!(coerce_string("False"), Some(Value::Bool(false)));
}

#[test]
fn coerce_string_leaves_non_numeric_strings() {
    assert_eq!(coerce_string("10px"), None);
    assert_eq!(coerce_string("hello"), None);
    assert_eq!(coerce_string(""), None);
    assert_eq!(coerce_string("123abc"), None);
    assert_eq!(coerce_string("null"), None);
}

#[test]
fn coerce_string_scalars_recursive_object() {
    let input = json!({
        "name": "test",
        "count": "42",
        "enabled": "true",
        "nested": {
            "ratio": "4.567",
            "flag": "false"
        }
    });
    let result = coerce_string_scalars(&input).expect("should have coerced");
    assert_eq!(
        result,
        json!({
            "name": "test",
            "count": 42,
            "enabled": true,
            "nested": {
                "ratio": 4.567,
                "flag": false
            }
        })
    );
}

#[test]
fn coerce_string_scalars_recursive_array() {
    let input = json!({
        "items": [
            {"yield_time_ms": "10000", "cmd": "echo hi"},
            {"yield_time_ms": "5000", "cmd": "echo bye"}
        ]
    });
    let result = coerce_string_scalars(&input).expect("should have coerced");
    assert_eq!(
        result,
        json!({
            "items": [
                {"yield_time_ms": 10000, "cmd": "echo hi"},
                {"yield_time_ms": 5000, "cmd": "echo bye"}
            ]
        })
    );
}

#[test]
fn coerce_string_scalars_returns_none_when_no_change() {
    let input = json!({"name": "test", "cmd": "echo"});
    assert!(coerce_string_scalars(&input).is_none());
}

#[test]
fn coerce_string_json_array() {
    let s = r#"[{"text":"do something","state":"pending"}]"#;
    let result = coerce_string(s).expect("should parse JSON array");
    assert_eq!(
        result,
        json!([{"text": "do something", "state": "pending"}])
    );
}

#[test]
fn coerce_string_json_object() {
    let s = r#"{"key":"value","num":42}"#;
    let result = coerce_string(s).expect("should parse JSON object");
    assert_eq!(result, json!({"key": "value", "num": 42}));
}

#[test]
fn coerce_string_invalid_json_left_untouched() {
    // Starts with [ but is not valid JSON — should return None.
    assert_eq!(coerce_string("[invalid"), None);
    assert_eq!(coerce_string("{not json"), None);
}

#[test]
fn coerce_string_plain_text_with_brace_untouched() {
    // Text that merely starts with { or [ but is not JSON should be preserved.
    assert_eq!(coerce_string("{placeholder}"), None);
    assert_eq!(coerce_string("[link]"), None);
}

#[test]
fn coerce_string_scalars_json_string_field() {
    // Simulates the LLM mistake: todo_list serialized as a JSON string.
    let input = json!({
        "todo_list": "[{\"text\":\"step 1\",\"state\":\"pending\"}]"
    });
    let result = coerce_string_scalars(&input).expect("should have coerced");
    assert_eq!(
        result,
        json!({"todo_list": [{"text": "step 1", "state": "pending"}]})
    );
}

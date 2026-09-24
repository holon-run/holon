//! End-to-end parity harness for the local ONNX binding-decision provider.
//!
//! Skipped unless the environment provides EVAL75_JSONL, DECISION_MODEL_DIR,
//! and RUST_RESULTS_OUT. The Rust path renders B-slot states, marks entities,
//! encodes with the question-tail contract, and scores through ONNX Runtime;
//! results are compared against the Python reference offline.
#![cfg(feature = "onnx")]

use decision_core::{DecisionContext, DecisionOutcome, DecisionProvider, DecisionRequest};
use decision_local_onnx::{state, LocalOnnxConfig, LocalOnnxProvider};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, time::Duration};

const QUESTION: &str = "判断：仅就展示的上下文，这条新输入是否承接该上下文（回应其内容/建议/问题，应绑定到上述工作项）？";

#[test]
fn eval75_rust_end_to_end() {
    let rows_path = match std::env::var("EVAL75_JSONL") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            eprintln!("skipping: EVAL75_JSONL is not set");
            return;
        }
    };
    let model_dir =
        PathBuf::from(std::env::var("DECISION_MODEL_DIR").expect("DECISION_MODEL_DIR is required"));
    let out_path =
        PathBuf::from(std::env::var("RUST_RESULTS_OUT").expect("RUST_RESULTS_OUT is required"));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let provider = LocalOnnxProvider::new(LocalOnnxConfig {
            model_dir,
            variant: "fp32".into(),
            num_threads: 4,
            checksum: None,
        })
        .expect("provider");

        let rows: Vec<Value> = fs::read_to_string(&rows_path)
            .expect("rows")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("row"))
            .collect();

        let mut results = String::new();
        for row in rows {
            let input: Value = row["input"].clone();
            let kind = input["kind"].as_str().unwrap_or("operator_input").to_string();
            let text = input["text"].as_str().unwrap_or_default().to_string();
            let history: Vec<String> = input["history"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item["text"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let history_refs: Vec<&str> = history.iter().map(String::as_str).collect();
            let rendered = state::render_binding_state(&kind, &history_refs, &text);
            let marked = state::mark_entities(&rendered);

            let request = DecisionRequest {
                request_id: format!("eval75-{}", row["id"].as_str().unwrap_or("?")),
                input: json!({ "baseline": marked }),
                candidates: vec![json!("yes"), json!("no"), json!("noul")],
                schema: QUESTION.to_string(),
                schema_version: "eval75-local-v1".into(),
                metadata: Default::default(),
                deadline_ms: None,
            };
            let response = provider
                .decide(request, DecisionContext::with_timeout(Duration::from_secs(60)))
                .await
                .expect("decide");
            let probabilities: Vec<f32> = response
                .evidence
                .first()
                .and_then(|evidence| evidence.metadata.get("probabilities"))
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_default();
            let (option, confidence) = match &response.outcome {
                DecisionOutcome::Select { value } => {
                    (value.as_str().unwrap_or("?").to_string(), response.confidence.unwrap_or(0.0))
                }
                _ => ("uncertain".to_string(), response.confidence.unwrap_or(0.0)),
            };
            let label = match option.as_str() {
                "yes" => "yes",
                "no" => "no",
                "noul" => "na",
                _ => "uncertain",
            };
            let record = json!({
                "schema_version": "eval75-local-v1",
                "system": "rust-fp32/emark-B",
                "id": row["id"],
                "source_kind": row["source_kind"],
                "gold_A": row["gold_A"],
                "label": label,
                "forced_binary_label": if label == "yes" || label == "no" { json!(label) } else { json!(null) },
                "p_yes": probabilities.first().copied().unwrap_or(0.0),
                "confidence": confidence,
                "probs": {
                    "yes": probabilities.first().copied().unwrap_or(0.0),
                    "no": probabilities.get(1).copied().unwrap_or(0.0),
                    "noul": probabilities.get(2).copied().unwrap_or(0.0),
                },
            });
            results.push_str(&serde_json::to_string(&record).expect("record"));
            results.push('\n');
        }
        fs::write(&out_path, results).expect("write results");
        eprintln!("rust results written to {}", out_path.display());
    });
}

//! Round-level bounded admission for a single provider response.
//!
//! A provider response must pass every quota below before any managed tool
//! execution, command start, task creation, or control tool effect for the
//! round. Any violation fails the whole round with a stable
//! `provider_round_limit_exceeded` reason and no partial execution; see
//! `docs/rfcs/provider-round-admission.md` for the contract.
//!
//! All byte accounting is bounded: values are exact while within budget and
//! become lower bounds once accounting stops at the budget.

use std::io;

use crate::provider::ModelBlock;
use crate::runtime_error::{ProviderRoundLimitExceeded, ProviderRoundLimitQuota};

/// Maximum managed tool calls (`ModelBlock::ToolUse`) in one provider
/// response. Legal, unknown, and malformed names all count.
pub(crate) const MAX_MANAGED_TOOL_CALLS_PER_ROUND: usize = 64;

/// Maximum compact-JSON serialized UTF-8 bytes for one managed tool call
/// `input` value. Equal is accepted; greater is rejected.
pub(crate) const MAX_TOOL_CALL_INPUT_BYTES: u64 = 128 * 1024;

/// Maximum visible payload bytes for a round that contains at least one
/// managed tool call: every `Text.text` byte plus every managed call's
/// `id` + `name` + input bytes. Pure-text rounds are not subject to this
/// quota.
pub(crate) const MAX_ROUND_VISIBLE_PAYLOAD_BYTES: u64 = 256 * 1024;

/// Maximum managed work items for one provider response after expanding
/// batch wrappers: a non-batch managed call counts as one, an
/// `ExecCommandBatch` call counts its raw `items` array length.
pub(crate) const MAX_EXPANDED_MANAGED_WORK_ITEMS_PER_ROUND: usize = 64;

/// Counting writer that stops at a byte budget. When serialization is
/// aborted at the budget the reported count is the budget itself, a lower
/// bound of the real size.
struct BoundedByteCounter {
    budget: u64,
    written: u64,
    truncated: bool,
}

impl BoundedByteCounter {
    fn new(budget: u64) -> Self {
        Self {
            budget,
            written: 0,
            truncated: false,
        }
    }
}

impl io::Write for BoundedByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let chunk = buf.len() as u64;
        match self.written.checked_add(chunk) {
            Some(total) if total <= self.budget => {
                self.written = total;
                Ok(buf.len())
            }
            _ => {
                self.truncated = true;
                self.written = self.budget;
                Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "provider round admission byte budget exceeded",
                ))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Saturating capped addition: any overflow counts as the cap so callers
/// only ever observe values within `cap`.
fn add_capped(current: u64, addend: u64, cap: u64) -> u64 {
    current.checked_add(addend).unwrap_or(u64::MAX).min(cap)
}

fn reject(
    quota: ProviderRoundLimitQuota,
    round: usize,
    limit: u64,
    observed: u64,
    count_is_lower_bound: bool,
    managed_tool_calls: usize,
) -> ProviderRoundLimitExceeded {
    ProviderRoundLimitExceeded {
        quota,
        round,
        limit,
        observed,
        count_is_lower_bound,
        managed_tool_calls,
    }
}

/// Admit one fully parsed provider response before any round record,
/// transcript entry, or tool execution is produced from it.
///
/// `round` is the provider round number within the turn, used only as
/// trusted diagnostic context in the rejection.
pub(crate) fn admit_provider_round(
    round: usize,
    blocks: &[ModelBlock],
) -> Result<(), ProviderRoundLimitExceeded> {
    let payload_cap = MAX_ROUND_VISIBLE_PAYLOAD_BYTES + 1;
    let mut managed_tool_calls = 0usize;
    let mut expanded_work_items = 0usize;
    // Capped at the payload limit plus one: exact while within the limit and
    // a lower bound above it.
    let mut payload_bytes = 0u64;
    // The visible payload quota only applies once the round contains at
    // least one managed tool call.
    let mut payload_armed = false;

    for block in blocks {
        match block {
            ModelBlock::Text { text } => {
                payload_bytes = add_capped(payload_bytes, text.len() as u64, payload_cap);
                if payload_armed && payload_bytes > MAX_ROUND_VISIBLE_PAYLOAD_BYTES {
                    return Err(reject(
                        ProviderRoundLimitQuota::RoundVisiblePayloadBytes,
                        round,
                        MAX_ROUND_VISIBLE_PAYLOAD_BYTES,
                        payload_bytes,
                        true,
                        managed_tool_calls,
                    ));
                }
            }
            ModelBlock::ToolUse {
                id, name, input, ..
            } => {
                managed_tool_calls += 1;
                if managed_tool_calls > MAX_MANAGED_TOOL_CALLS_PER_ROUND {
                    return Err(reject(
                        ProviderRoundLimitQuota::ManagedToolCalls,
                        round,
                        MAX_MANAGED_TOOL_CALLS_PER_ROUND as u64,
                        managed_tool_calls as u64,
                        true,
                        managed_tool_calls,
                    ));
                }
                payload_armed = true;
                payload_bytes =
                    add_capped(payload_bytes, (id.len() + name.len()) as u64, payload_cap);
                if payload_bytes > MAX_ROUND_VISIBLE_PAYLOAD_BYTES {
                    return Err(reject(
                        ProviderRoundLimitQuota::RoundVisiblePayloadBytes,
                        round,
                        MAX_ROUND_VISIBLE_PAYLOAD_BYTES,
                        payload_bytes,
                        true,
                        managed_tool_calls,
                    ));
                }

                // One bounded compact-JSON measurement pass shared by the
                // per-call and round payload quotas. The budget is the
                // smaller unresolved quota: writing exactly the budget is
                // still accepted, and any write crossing it means that
                // quota was exceeded by the serialized input.
                let remaining_round = MAX_ROUND_VISIBLE_PAYLOAD_BYTES.saturating_sub(payload_bytes);
                let cap = remaining_round.min(MAX_TOOL_CALL_INPUT_BYTES);
                let mut counter = BoundedByteCounter::new(cap);
                if serde_json::to_writer(&mut counter, input).is_err() {
                    counter.truncated = true;
                }
                if counter.truncated {
                    if cap == MAX_TOOL_CALL_INPUT_BYTES {
                        // Per-call budget exhausted; the round payload
                        // still had at least this much room.
                        return Err(reject(
                            ProviderRoundLimitQuota::ToolCallInputBytes,
                            round,
                            MAX_TOOL_CALL_INPUT_BYTES,
                            MAX_TOOL_CALL_INPUT_BYTES.saturating_add(1),
                            true,
                            managed_tool_calls,
                        ));
                    }
                    // Round payload budget exhausted first.
                    return Err(reject(
                        ProviderRoundLimitQuota::RoundVisiblePayloadBytes,
                        round,
                        MAX_ROUND_VISIBLE_PAYLOAD_BYTES,
                        MAX_ROUND_VISIBLE_PAYLOAD_BYTES.saturating_add(1),
                        true,
                        managed_tool_calls,
                    ));
                }
                payload_bytes = add_capped(payload_bytes, counter.written, payload_cap);
                if payload_bytes > MAX_ROUND_VISIBLE_PAYLOAD_BYTES {
                    return Err(reject(
                        ProviderRoundLimitQuota::RoundVisiblePayloadBytes,
                        round,
                        MAX_ROUND_VISIBLE_PAYLOAD_BYTES,
                        payload_bytes,
                        false,
                        managed_tool_calls,
                    ));
                }

                // Raw byte quotas precede the shared, side-effect-free
                // parser shape recovery. Only genuinely unrecoverable
                // batches count as one and keep invalid_tool_input.
                if name == holon_tools::builtins::exec_command_batch::NAME {
                    match holon_tools::builtins::exec_command_batch::batch_item_count(input) {
                        Some(item_count) => {
                            if item_count > holon_tools::builtins::exec_command_batch::MAX_ITEMS {
                                return Err(reject(
                                    ProviderRoundLimitQuota::BatchItems,
                                    round,
                                    holon_tools::builtins::exec_command_batch::MAX_ITEMS as u64,
                                    item_count as u64,
                                    false,
                                    managed_tool_calls,
                                ));
                            }
                            expanded_work_items = expanded_work_items.saturating_add(item_count);
                        }
                        None => {
                            expanded_work_items = expanded_work_items.saturating_add(1);
                        }
                    }
                } else {
                    expanded_work_items = expanded_work_items.saturating_add(1);
                }
                if expanded_work_items > MAX_EXPANDED_MANAGED_WORK_ITEMS_PER_ROUND {
                    return Err(reject(
                        ProviderRoundLimitQuota::ExpandedWorkItems,
                        round,
                        MAX_EXPANDED_MANAGED_WORK_ITEMS_PER_ROUND as u64,
                        expanded_work_items as u64,
                        true,
                        managed_tool_calls,
                    ));
                }
            }
            ModelBlock::Citations { .. }
            | ModelBlock::ProviderToolUse { .. }
            | ModelBlock::ProviderToolResult { .. }
            | ModelBlock::Thinking { .. }
            | ModelBlock::ReasoningText { .. }
            | ModelBlock::RedactedThinking { .. } => {}
        }
    }
    Ok(())
}

#[cfg(holon_lib_test_shard = "a")]
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn tool_use(id: &str, name: &str, input: Value) -> ModelBlock {
        ModelBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
            kind: Default::default(),
            provider_data: None,
        }
    }

    fn text(size_bytes: usize) -> ModelBlock {
        ModelBlock::Text {
            text: "x".repeat(size_bytes),
        }
    }

    /// Compact serialized size of a value, matching the admission writer.
    fn compact_len(value: &Value) -> u64 {
        serde_json::to_string(value)
            .expect("value serializes")
            .len() as u64
    }

    #[test]
    fn managed_tool_calls_boundary_is_minus_one_equal_plus_one() {
        let calls = |n: usize| -> Vec<ModelBlock> {
            (0..n)
                .map(|i| tool_use(&format!("t{i}"), "Exec", json!({})))
                .collect()
        };
        assert!(admit_provider_round(0, &calls(63)).is_ok());
        assert!(admit_provider_round(0, &calls(64)).is_ok());
        let rejection = admit_provider_round(2, &calls(65)).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ManagedToolCalls);
        assert_eq!(rejection.limit, 64);
        assert_eq!(rejection.observed, 65);
        assert!(rejection.count_is_lower_bound);
        assert_eq!(rejection.managed_tool_calls, 65);
        assert_eq!(rejection.round, 2);
    }

    #[test]
    fn unknown_and_malformed_names_still_count() {
        let blocks: Vec<ModelBlock> = ["", "?weird name", "NotARealTool"]
            .iter()
            .enumerate()
            .map(|(i, name)| tool_use(&format!("id{i}"), name, json!({})))
            .collect();
        assert!(admit_provider_round(0, &blocks).is_ok());
    }

    #[test]
    fn per_call_input_bytes_boundary_is_minus_one_equal_plus_one() {
        let base = compact_len(&json!({"pad": ""})) as usize;
        let padded = |total: usize| json!({"pad": "x".repeat(total - base)});
        let make = |total: usize| vec![tool_use("a", "Exec", padded(total))];
        assert!(admit_provider_round(0, &make(MAX_TOOL_CALL_INPUT_BYTES as usize - 1)).is_ok());
        assert!(admit_provider_round(0, &make(MAX_TOOL_CALL_INPUT_BYTES as usize)).is_ok());
        let rejection =
            admit_provider_round(1, &make(MAX_TOOL_CALL_INPUT_BYTES as usize + 1)).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ToolCallInputBytes);
        assert_eq!(rejection.limit, MAX_TOOL_CALL_INPUT_BYTES);
        assert!(rejection.count_is_lower_bound);
        assert!(rejection.observed > MAX_TOOL_CALL_INPUT_BYTES);
    }

    #[test]
    fn oversized_single_argument_reports_lower_bound_only() {
        // 1.68 MiB-style flood argument: exact size is never measured.
        let input = json!({"blob": "x".repeat(1_800_000)});
        let rejection = admit_provider_round(0, &[tool_use("a", "Exec", input)]).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ToolCallInputBytes);
        assert!(rejection.count_is_lower_bound);
        assert_eq!(rejection.observed, MAX_TOOL_CALL_INPUT_BYTES + 1);
    }

    #[test]
    fn unicode_and_nested_json_count_compact_bytes() {
        // Multibyte content and nested structures are measured by compact
        // JSON bytes; inner string whitespace is preserved, not trimmed.
        let input = json!({
            "nested": {"array": [1, 2, {"k": "中文 value"}]},
            "text": "  padded  "
        });
        let measured = compact_len(&input);
        assert!(measured > 40);
        let make = |pad: &Value| vec![tool_use("a", "Exec", pad.clone())];
        assert!(admit_provider_round(0, &make(&input)).is_ok());

        // Push exactly one byte over the per-call limit by growing the
        // `text` string: its serialized form is its chars plus two quotes,
        // replacing the original 12 serialized bytes of "  padded  ".
        let target = (MAX_TOOL_CALL_INPUT_BYTES + 1) as usize;
        let current = measured as usize;
        let grown_text_len = 12 + target - current - 2;
        let mut oversized = input.clone();
        oversized["text"] = json!("x".repeat(grown_text_len));
        assert_eq!(compact_len(&oversized) as usize, target);
        let rejection = admit_provider_round(0, &make(&oversized)).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ToolCallInputBytes);
    }

    #[test]
    fn round_payload_boundary_counts_text_and_tool_bytes() {
        let call = tool_use("a", "Exec", Value::Null);
        // id(1) + name(4) + compact null(4) = 9 bytes.
        let call_bytes = 1 + 4 + compact_len(&Value::Null);
        let accept = MAX_ROUND_VISIBLE_PAYLOAD_BYTES - call_bytes;
        assert!(admit_provider_round(0, &[text(accept as usize), call.clone()]).is_ok());

        let over = MAX_ROUND_VISIBLE_PAYLOAD_BYTES - call_bytes + 1;
        let rejection = admit_provider_round(3, &[text(over as usize), call]).unwrap_err();
        assert_eq!(
            rejection.quota,
            ProviderRoundLimitQuota::RoundVisiblePayloadBytes
        );
        assert_eq!(rejection.limit, MAX_ROUND_VISIBLE_PAYLOAD_BYTES);
        assert_eq!(rejection.managed_tool_calls, 1);
    }

    #[test]
    fn pure_text_rounds_are_not_subject_to_payload_quota() {
        assert!(admit_provider_round(0, &[text(600_000)]).is_ok());
    }

    #[test]
    fn payload_counts_text_after_calls_too() {
        let call = tool_use("a", "Exec", Value::Null);
        let call_bytes = 1 + 4 + compact_len(&Value::Null);
        let text_bytes = (MAX_ROUND_VISIBLE_PAYLOAD_BYTES - call_bytes + 1) as usize;
        let rejection = admit_provider_round(0, &[call, text(text_bytes)]).unwrap_err();
        assert_eq!(
            rejection.quota,
            ProviderRoundLimitQuota::RoundVisiblePayloadBytes
        );
        assert!(rejection.count_is_lower_bound);
    }

    #[test]
    fn provider_owned_and_thinking_blocks_are_never_counted() {
        let mut blocks: Vec<ModelBlock> = (0..64)
            .map(|i| tool_use(&format!("t{i}"), "Exec", json!({})))
            .collect();
        for _ in 0..100 {
            blocks.push(ModelBlock::ProviderToolUse {
                id: "p".into(),
                name: "provider_tool".into(),
                input: json!({"blob": "x".repeat(10_000)}),
                status: None,
                provider_data: None,
            });
            blocks.push(ModelBlock::Thinking {
                text: "y".repeat(20_000),
                signature: "sig".into(),
            });
            blocks.push(ModelBlock::RedactedThinking {
                data: "z".repeat(20_000),
            });
            blocks.push(ModelBlock::Citations {
                citations: Vec::new(),
            });
        }
        assert!(admit_provider_round(0, &blocks).is_ok());
    }

    fn batch(items: usize) -> ModelBlock {
        let array: Vec<Value> = (0..items).map(|_| json!({})).collect();
        tool_use(
            "b",
            holon_tools::builtins::exec_command_batch::NAME,
            json!({"items": array}),
        )
    }

    #[test]
    fn batch_items_boundary_is_twelve_accept_thirteen_reject() {
        assert!(admit_provider_round(0, &[batch(12)]).is_ok());
        let rejection = admit_provider_round(0, &[batch(13)]).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::BatchItems);
        assert_eq!(rejection.limit, 12);
        assert_eq!(rejection.observed, 13);
        assert!(!rejection.count_is_lower_bound);
    }

    #[test]
    fn flood_of_empty_batch_objects_is_rejected_before_item_parsing() {
        let rejection = admit_provider_round(0, &[batch(18_471)]).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::BatchItems);
        assert_eq!(rejection.observed, 18_471);
    }

    #[test]
    fn malformed_batch_items_count_as_one_and_stay_admissible() {
        let missing = tool_use(
            "b",
            holon_tools::builtins::exec_command_batch::NAME,
            json!({"items": null}),
        );
        let non_array = tool_use(
            "b",
            holon_tools::builtins::exec_command_batch::NAME,
            json!({"items": "nope"}),
        );
        assert!(admit_provider_round(0, &[missing, non_array]).is_ok());
    }

    #[test]
    fn expanded_work_items_boundary_and_mixed_batches() {
        let plain = |i: usize| tool_use(&format!("p{i}"), "Exec", json!({}));
        let sixty_four: Vec<ModelBlock> = (0..64).map(plain).collect();
        assert!(admit_provider_round(0, &sixty_four).is_ok());

        // Expansion can exceed the cap while the managed call count does
        // not: six full batches expand to 72 work items from six calls.
        let six_full_batches: Vec<ModelBlock> = (0..6).map(|_| batch(12)).collect();
        let rejection = admit_provider_round(0, &six_full_batches).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ExpandedWorkItems);
        assert_eq!(rejection.observed, 72);

        // Five full batches (60) plus four plain calls is exactly 64.
        let mixed: Vec<ModelBlock> = (0..5).map(|_| batch(12)).chain((0..4).map(plain)).collect();
        assert!(admit_provider_round(0, &mixed).is_ok());
        let mut mixed_over = mixed;
        mixed_over.push(plain(99));
        let rejection = admit_provider_round(0, &mixed_over).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ExpandedWorkItems);
        assert_eq!(rejection.observed, 65);

        // The top-level call-count limit still applies independently of
        // expansion: 64 batch wrappers of one item each pass expansion but
        // a 65th wrapper hits the managed-call quota first.
        let wrappers: Vec<ModelBlock> = (0..64).map(|_| batch(1)).collect();
        assert!(admit_provider_round(0, &wrappers).is_ok());
        let mut wrappers_over = wrappers;
        wrappers_over.push(batch(1));
        let rejection = admit_provider_round(0, &wrappers_over).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ManagedToolCalls);
    }

    #[test]
    fn prefix_stop_counts_are_lower_bounds_with_unscanned_tail() {
        let calls: Vec<_> = (0..9_412)
            .map(|i| tool_use(&format!("t{i}"), "UnknownTool", json!({})))
            .collect();
        let rejection = admit_provider_round(0, &calls).unwrap_err();
        assert_eq!(rejection.observed, 65);
        assert_eq!(rejection.managed_tool_calls, 65);
        assert!(rejection.count_is_lower_bound);

        let calls: Vec<_> = (0..9).map(|_| batch(12)).collect();
        let rejection = admit_provider_round(0, &calls).unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ExpandedWorkItems);
        assert_eq!(rejection.observed, 72);
        assert_eq!(rejection.managed_tool_calls, 6);
        assert!(rejection.count_is_lower_bound);
    }

    fn recovered_batch_inputs(count: usize) -> Vec<Value> {
        let items = json!(vec![json!({"cmd": "printf ok"}); count]);
        let raw = json!({"items": items});
        vec![
            raw.clone(),
            json!({"items": items.to_string()}),
            json!(raw.to_string()),
            json!({"arguments": raw}),
            json!({"parameters": raw.to_string()}),
            json!({"params": raw}),
            json!({"input": raw}),
            json!({
                "arguments": {"items": vec![json!({}); 13]},
                "items": items.to_string(),
            }),
        ]
    }

    #[test]
    fn recovered_batches_obey_batch_and_expanded_boundaries() {
        let name = holon_tools::builtins::exec_command_batch::NAME;
        for input in recovered_batch_inputs(12) {
            assert!(admit_provider_round(0, &[tool_use("b", name, input.clone())]).is_ok());
            let calls: Vec<_> = (0..6)
                .map(|i| tool_use(&format!("b{i}"), name, input.clone()))
                .collect();
            let rejection = admit_provider_round(0, &calls).unwrap_err();
            assert_eq!(rejection.quota, ProviderRoundLimitQuota::ExpandedWorkItems);
            assert_eq!(rejection.observed, 72);
            assert!(rejection.count_is_lower_bound);

            let mut mixed = calls[..5].to_vec();
            mixed.extend((0..4).map(|i| tool_use(&format!("p{i}"), "Unknown", json!({}))));
            assert!(admit_provider_round(0, &mixed).is_ok());
            mixed.push(tool_use("extra", "Unknown", json!({})));
            let rejection = admit_provider_round(0, &mixed).unwrap_err();
            assert_eq!(rejection.quota, ProviderRoundLimitQuota::ExpandedWorkItems);
            assert_eq!(rejection.observed, 65);
            assert!(rejection.count_is_lower_bound);
        }
        for input in recovered_batch_inputs(13) {
            let rejection = admit_provider_round(0, &[tool_use("b", name, input)]).unwrap_err();
            assert_eq!(rejection.quota, ProviderRoundLimitQuota::BatchItems);
            assert_eq!(rejection.observed, 13);
            assert!(!rejection.count_is_lower_bound);
        }
    }

    #[test]
    fn raw_byte_quotas_precede_recovered_batch_shape() {
        let input = json!({
            "items": vec![json!({}); 13],
            "workdir": "x".repeat(MAX_TOOL_CALL_INPUT_BYTES as usize),
        });
        let rejection = admit_provider_round(
            0,
            &[tool_use(
                "b",
                holon_tools::builtins::exec_command_batch::NAME,
                input,
            )],
        )
        .unwrap_err();
        assert_eq!(rejection.quota, ProviderRoundLimitQuota::ToolCallInputBytes);
        assert!(rejection.count_is_lower_bound);
    }
}

//! Frozen Phase 0 corpus case specs for baseline manifest generation.
//!
//! Each spec mirrors a durable-fact scenario from `corpus/cases.json` and
//! is consumed by `baseline::generate_manifest`.

use crate::projection_eval::baseline::{BaselineCaseSpec, BaselineEvidenceSpec, EvidenceTier};
use crate::projection_eval::{ProjectionBindingSummary, ProjectionEvidenceRole, ProjectionOwner};

fn legacy_unbound() -> ProjectionOwner {
    ProjectionOwner::LegacyUnbound {
        agent_id: "agent-1".into(),
    }
}

fn work_item(id: &str) -> ProjectionOwner {
    ProjectionOwner::WorkItem {
        work_item_id: id.into(),
    }
}

fn ev(
    reference: &str,
    role: ProjectionEvidenceRole,
    owner: &ProjectionOwner,
    tier: EvidenceTier,
    content: &str,
) -> BaselineEvidenceSpec {
    BaselineEvidenceSpec {
        reference: reference.into(),
        role,
        owner: owner.clone(),
        tier,
        content: content.into(),
    }
}

fn binding(
    message_id: &str,
    turn_id: &str,
    work_item_id: Option<&str>,
) -> ProjectionBindingSummary {
    ProjectionBindingSummary {
        source_message_id: message_id.into(),
        turn_id: turn_id.into(),
        work_item_id: work_item_id.map(|s| s.into()),
        claimed_work_revision: None,
    }
}

/// Generate evidence refs for alternating operator/assistant turns.
fn alternating_turns(
    count: usize,
    early_decision_turn: usize,
    owner: &ProjectionOwner,
) -> Vec<BaselineEvidenceSpec> {
    let mut specs = Vec::new();
    for turn in 1..=count {
        let is_current = turn == count;
        let is_early_decision = turn == early_decision_turn;
        // The reference turn (last turn) is always an operator message
        // with CurrentInput role, regardless of parity.
        if is_current {
            specs.push(ev(
                &format!("message:turn-{turn}"),
                ProjectionEvidenceRole::CurrentInput,
                owner,
                EvidenceTier::Current,
                &format!("Operator turn {turn} message content."),
            ));
            continue;
        }
        if turn % 2 == 1 {
            // Operator message
            let tier = EvidenceTier::Core;
            let role = ProjectionEvidenceRole::Input;
            specs.push(ev(
                &format!("message:turn-{turn}"),
                role,
                owner,
                tier,
                &format!("Operator turn {turn} message content."),
            ));
        } else {
            // Assistant brief
            let tier = if is_early_decision {
                EvidenceTier::Predecessor
            } else {
                EvidenceTier::Core
            };
            let role = if is_early_decision {
                ProjectionEvidenceRole::DirectPredecessor
            } else {
                ProjectionEvidenceRole::Result
            };
            let label = if is_early_decision {
                "decision"
            } else {
                "response"
            };
            specs.push(ev(
                &format!("brief:turn-{turn}-{label}"),
                role,
                owner,
                tier,
                &format!("Assistant turn {turn} {label} brief content."),
            ));
        }
    }
    specs
}

/// Return all 12 frozen Phase 0 corpus case specs.
pub fn baseline_case_specs() -> Vec<BaselineCaseSpec> {
    vec![
        // 1. issue-2512-short-reference-need (3 turns)
        BaselineCaseSpec {
            case_id: "issue-2512-short-reference-need".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:2512-need", "turn-3", None)),
            turn_id: Some("turn-3".into()),
            evidence: vec![
                ev(
                    "message:2512-plan",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "先讨论系统投影重构方案",
                ),
                ev(
                    "brief:2512-plan",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "给出 WorkItem + Turn 方案",
                ),
                ev(
                    "message:2512-need",
                    ProjectionEvidenceRole::CurrentInput,
                    &legacy_unbound(),
                    EvidenceTier::Current,
                    "需要",
                ),
            ],
            forbidden_refs: vec![],
        },
        // 2. issue-2512-short-reference-can (4 turns)
        BaselineCaseSpec {
            case_id: "issue-2512-short-reference-can".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:2512-can", "turn-4", None)),
            turn_id: Some("turn-4".into()),
            evidence: vec![
                ev(
                    "message:2512-plan",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "先讨论系统投影重构方案",
                ),
                ev(
                    "brief:2512-plan",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "给出两个阶段",
                ),
                ev(
                    "message:2512-question",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "先做评估工具吗",
                ),
                ev(
                    "message:2512-can",
                    ProjectionEvidenceRole::CurrentInput,
                    &legacy_unbound(),
                    EvidenceTier::Current,
                    "可以",
                ),
            ],
            forbidden_refs: vec![],
        },
        // 3. issue-2512-ordinal-reference (5 turns)
        BaselineCaseSpec {
            case_id: "issue-2512-ordinal-reference".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:previous-plan", "turn-5", None)),
            turn_id: Some("turn-5".into()),
            evidence: vec![
                ev(
                    "message:options",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "给三个候选方案",
                ),
                ev(
                    "brief:options",
                    ProjectionEvidenceRole::DirectPredecessor,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "第一、第二、第三方案",
                ),
                ev(
                    "message:second",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "第二个",
                ),
                ev(
                    "brief:second",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "展开第二方案",
                ),
                ev(
                    "message:previous-plan",
                    ProjectionEvidenceRole::CurrentInput,
                    &legacy_unbound(),
                    EvidenceTier::Current,
                    "上一个方案呢",
                ),
            ],
            forbidden_refs: vec![],
        },
        // 4. continuity-10-turns
        BaselineCaseSpec {
            case_id: "continuity-10-turns".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:turn-10", "turn-10", None)),
            turn_id: Some("turn-10".into()),
            evidence: alternating_turns(10, 2, &legacy_unbound()),
            forbidden_refs: vec![],
        },
        // 5. continuity-20-turns
        BaselineCaseSpec {
            case_id: "continuity-20-turns".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:turn-20", "turn-20", None)),
            turn_id: Some("turn-20".into()),
            evidence: alternating_turns(20, 3, &legacy_unbound()),
            forbidden_refs: vec![],
        },
        // 6. continuity-40-turns
        BaselineCaseSpec {
            case_id: "continuity-40-turns".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:turn-40", "turn-40", None)),
            turn_id: Some("turn-40".into()),
            evidence: alternating_turns(40, 4, &legacy_unbound()),
            forbidden_refs: vec![],
        },
        // 7. new-topic-no-false-carry-over (8 turns)
        BaselineCaseSpec {
            case_id: "new-topic-no-false-carry-over".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:topic-b-current", "turn-8", None)),
            turn_id: Some("turn-8".into()),
            evidence: {
                let mut evs = Vec::new();
                // Topic A (turns 1-4)
                evs.push(ev(
                    "message:topic-a-start",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "Start topic A discussion.",
                ));
                evs.push(ev(
                    "brief:topic-a-response",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "Topic A assistant response.",
                ));
                evs.push(ev(
                    "message:topic-a-followup",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "Topic A follow-up question.",
                ));
                evs.push(ev(
                    "brief:topic-a-abandoned-action",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "Topic A abandoned action item.",
                ));
                // Topic B (turns 5-8) — boundary at turn 5
                evs.push(ev(
                    "message:topic-b-start",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "Start topic B discussion.",
                ));
                evs.push(ev(
                    "brief:topic-b-response",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "Topic B assistant response.",
                ));
                evs.push(ev(
                    "message:topic-b-followup",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "Topic B follow-up.",
                ));
                evs.push(ev(
                    "message:topic-b-current",
                    ProjectionEvidenceRole::CurrentInput,
                    &legacy_unbound(),
                    EvidenceTier::Current,
                    "Current topic B message.",
                ));
                evs
            },
            forbidden_refs: vec!["brief:topic-a-abandoned-action".into()],
        },
        // 8. return-to-old-topic (12 turns)
        BaselineCaseSpec {
            case_id: "return-to-old-topic".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:return-topic-a", "turn-12", None)),
            turn_id: Some("turn-12".into()),
            evidence: {
                let mut evs = Vec::new();
                // Topic A (turns 1-4)
                evs.push(ev(
                    "message:topic-a-start",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Core,
                    "Start topic A.",
                ));
                evs.push(ev(
                    "brief:topic-a-decision",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "Topic A decision brief.",
                ));
                // Topic B (turns 5-11) — return at turn 12
                evs.push(ev(
                    "message:topic-b-start",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "Start topic B.",
                ));
                evs.push(ev(
                    "brief:topic-b-unrelated-detail",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "Topic B unrelated detail.",
                ));
                evs.push(ev(
                    "message:topic-b-more",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "More topic B.",
                ));
                // Return to topic A (turn 12)
                evs.push(ev(
                    "message:return-topic-a",
                    ProjectionEvidenceRole::CurrentInput,
                    &legacy_unbound(),
                    EvidenceTier::Current,
                    "Return to topic A.",
                ));
                evs
            },
            forbidden_refs: vec!["brief:topic-b-unrelated-detail".into()],
        },
        // 9. work-item-switch-return (9 turns)
        BaselineCaseSpec {
            case_id: "work-item-switch-return".into(),
            owner: work_item("a"),
            binding: Some(binding("message:a-input", "turn-1", Some("a"))),
            turn_id: Some("turn-return-b-a".into()),
            evidence: vec![
                ev(
                    "work_item:a",
                    ProjectionEvidenceRole::WorkItemState,
                    &work_item("a"),
                    EvidenceTier::Runtime,
                    "Work item A created.",
                ),
                ev(
                    "message:a-input",
                    ProjectionEvidenceRole::CurrentInput,
                    &work_item("a"),
                    EvidenceTier::Current,
                    "Work item A input message.",
                ),
                ev(
                    "work_item:b",
                    ProjectionEvidenceRole::WorkItemState,
                    &work_item("b"),
                    EvidenceTier::Background,
                    "Work item B created.",
                ),
                ev(
                    "turn:switch-a-b",
                    ProjectionEvidenceRole::Turn,
                    &work_item("b"),
                    EvidenceTier::Background,
                    "Switch from A to B.",
                ),
                ev(
                    "message:b-task-result",
                    ProjectionEvidenceRole::ToolResult,
                    &work_item("b"),
                    EvidenceTier::Background,
                    "Work item B task result.",
                ),
                ev(
                    "brief:b-complete",
                    ProjectionEvidenceRole::WorkItemState,
                    &work_item("b"),
                    EvidenceTier::Background,
                    "Work item B completed.",
                ),
                ev(
                    "turn:return-b-a",
                    ProjectionEvidenceRole::Turn,
                    &work_item("a"),
                    EvidenceTier::Runtime,
                    "Return from B to A.",
                ),
            ],
            forbidden_refs: vec!["message:b-task-result".into()],
        },
        // 10. work-item-wait-task-result-recovery (7 turns)
        BaselineCaseSpec {
            case_id: "work-item-wait-task-result-recovery".into(),
            owner: work_item("recovery"),
            binding: Some(binding("message:work-request", "turn-1", Some("recovery"))),
            turn_id: Some("turn-recovery".into()),
            evidence: vec![
                ev(
                    "message:work-request",
                    ProjectionEvidenceRole::CurrentInput,
                    &work_item("recovery"),
                    EvidenceTier::Current,
                    "Work request message.",
                ),
                ev(
                    "wait:task-1",
                    ProjectionEvidenceRole::WaitState,
                    &work_item("recovery"),
                    EvidenceTier::Runtime,
                    "Waiting for task 1 result.",
                ),
                ev(
                    "turn:daemon-restart",
                    ProjectionEvidenceRole::Lifecycle,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "Daemon restart occurred.",
                ),
                ev(
                    "message:task-1-result",
                    ProjectionEvidenceRole::ToolResult,
                    &work_item("recovery"),
                    EvidenceTier::Runtime,
                    "Task 1 result arrived.",
                ),
                ev(
                    "turn:recovery",
                    ProjectionEvidenceRole::Turn,
                    &work_item("recovery"),
                    EvidenceTier::Runtime,
                    "Recovery turn.",
                ),
            ],
            forbidden_refs: vec![],
        },
        // 11. discussion-interleaved-runtime-wakes (11 turns)
        BaselineCaseSpec {
            case_id: "discussion-interleaved-runtime-wakes".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:discussion-followup", "turn-11", None)),
            turn_id: Some("turn-11".into()),
            evidence: vec![
                ev(
                    "message:discussion-anchor",
                    ProjectionEvidenceRole::Input,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "Discussion anchor message.",
                ),
                ev(
                    "message:external-wake",
                    ProjectionEvidenceRole::Supporting,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "External wake event.",
                ),
                ev(
                    "message:timer-wake",
                    ProjectionEvidenceRole::Supporting,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "Timer wake event.",
                ),
                ev(
                    "message:lifecycle-wake",
                    ProjectionEvidenceRole::Supporting,
                    &legacy_unbound(),
                    EvidenceTier::Background,
                    "Lifecycle wake event.",
                ),
                ev(
                    "message:discussion-followup",
                    ProjectionEvidenceRole::CurrentInput,
                    &legacy_unbound(),
                    EvidenceTier::Current,
                    "Discussion follow-up message.",
                ),
            ],
            forbidden_refs: vec![],
        },
        // 12. restart-compaction-equivalence (40 turns)
        BaselineCaseSpec {
            case_id: "restart-compaction-equivalence".into(),
            owner: legacy_unbound(),
            binding: Some(binding("message:turn-40", "turn-40", None)),
            turn_id: Some("turn-40".into()),
            evidence: {
                let mut evs = alternating_turns(40, 4, &legacy_unbound());
                // Add compaction anchor
                evs.push(ev(
                    "brief:compaction-anchor",
                    ProjectionEvidenceRole::Result,
                    &legacy_unbound(),
                    EvidenceTier::Predecessor,
                    "Compaction anchor brief.",
                ));
                evs
            },
            forbidden_refs: vec![],
        },
    ]
}

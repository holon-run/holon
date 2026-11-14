pub use holon_domain::execution_protocol::*;

#[cfg(holon_lib_test_shard = "c")]
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn task_rejoin_source_preserves_parent_wire_and_command_creation_wire() {
        for (wire, parent, creation) in [
            (
                serde_json::json!({
                    "obligation_id": "task-a",
                    "generation": 1,
                    "parent_turn_id": "turn-parent"
                }),
                Some("turn-parent"),
                None,
            ),
            (
                serde_json::json!({
                    "obligation_id": "task-a",
                    "generation": 1,
                    "command_creation_id": "task-a"
                }),
                None,
                Some("task-a"),
            ),
        ] {
            let fence: RejoinFence = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(fence.source.parent_turn_id(), parent);
            assert_eq!(fence.source.command_creation_id(), creation);
            assert_eq!(serde_json::to_value(fence).unwrap(), wire);
        }
    }

    #[test]
    fn task_rejoin_source_rejects_null_ambiguous_and_duplicate_fields() {
        for wire in [
            r#"{}"#,
            r#"{"parent_turn_id":null}"#,
            r#"{"command_creation_id":null}"#,
            r#"{"parent_turn_id":"turn-parent","command_creation_id":null}"#,
            r#"{"parent_turn_id":null,"command_creation_id":"task-a"}"#,
            r#"{"parent_turn_id":null,"command_creation_id":null}"#,
            r#"{"parent_turn_id":"turn-parent","command_creation_id":"task-a"}"#,
            r#"{"parent_turn_id":""}"#,
            r#"{"command_creation_id":""}"#,
            r#"{"parent_turn_id":42}"#,
            r#"{"command_creation_id":false}"#,
            r#"{"parent_turn_id":"turn-parent","parent_turn_id":"turn-parent"}"#,
            r#"{"command_creation_id":"task-a","command_creation_id":"task-a"}"#,
            r#"{"result_execution_turn_id":"future-turn"}"#,
        ] {
            assert!(
                serde_json::from_str::<TaskRejoinSource>(wire).is_err(),
                "{wire}"
            );
            let fields = &wire[1..wire.len() - 1];
            let separator = if fields.is_empty() { "" } else { "," };
            let flattened =
                format!(r#"{{"obligation_id":"task-a","generation":1{separator}{fields}}}"#);
            assert!(
                serde_json::from_str::<RejoinFence>(&flattened).is_err(),
                "{flattened}"
            );
        }
    }

    #[test]
    fn context_budget_recheck_requires_exact_pause_changed_condition_and_preserves_source() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        let expected = WorkItemExecutionRecord {
            source_revision: 3,
            state: WorkItemExecutionState::Paused {
                generation: 5,
                reason: ContextBudgetStop {
                    source_revision: 3,
                    source_turn_id: "turn-a".into(),
                    configured_estimated_tokens: 4096,
                }
                .policy()
                .unwrap(),
            },
        };
        state.work_items.insert("work-a".into(), expected.clone());
        let mut command = RecheckContextBudgetStop {
            command_id: "budget-recheck-a".into(),
            work_item_id: "work-a".into(),
            expected,
            configured_estimated_tokens: 4096,
        };
        assert!(recheck_context_budget_stop(&state, &command).is_err());
        command.configured_estimated_tokens = 8192;
        let resumed = recheck_context_budget_stop(&state, &command).unwrap();
        assert_eq!(resumed.state.work_items["work-a"].source_revision, 3);
        assert_eq!(
            resumed.state.work_items["work-a"].state,
            WorkItemExecutionState::Runnable {
                generation: 6,
                recovery_ref: None,
            }
        );
        assert!(recheck_context_budget_stop(&resumed.state, &command).is_err());
        command.configured_estimated_tokens = 4096;
        command.expected.source_revision = 4;
        state
            .work_items
            .insert("work-a".into(), command.expected.clone());
        assert!(recheck_context_budget_stop(&state, &command).is_ok());
        command.expected.state = WorkItemExecutionState::Paused {
            generation: 5,
            reason: "independent business blocker".into(),
        };
        state
            .work_items
            .insert("work-a".into(), command.expected.clone());
        assert!(recheck_context_budget_stop(&state, &command).is_err());
    }

    fn work_item_record(state: WorkItemExecutionState) -> WorkItemExecutionRecord {
        WorkItemExecutionRecord {
            source_revision: state.generation(),
            state,
        }
    }

    fn attempt(id: &str, recovery_of: Option<&str>) -> ExecutionAttempt {
        ExecutionAttempt {
            input_projection: None,
            attempt_id: id.into(),
            agent_id: "agent-a".into(),
            source_message_id: Some(format!("message:{id}")),
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::WorkItemContinuation {
                    work_item_id: "work-a".into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::WorkItem {
                work_item_id: "work-a".into(),
            },
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::System,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Normal,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: Some(1),
                work_item_generation: Some(if recovery_of.is_some() { 2 } else { 1 }),
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: None,
            recovery_of_attempt_id: recovery_of.map(str::to_owned),
            terminal_outcome_id: None,
            admitted_at: "2026-08-01T00:00:00Z".into(),
            terminal_at: None,
        }
    }

    #[test]
    fn work_item_registration_is_additive_and_idempotent() {
        let state = ExecutionProtocolState::empty("agent-a");
        let command = RegisterWorkItemExecution {
            work_item_id: "work-a".into(),
            record: work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        };
        let registered = register_work_item_execution(&state, &command).unwrap();
        let replayed = register_work_item_execution(&registered.state, &command).unwrap();
        assert_eq!(registered.state, replayed.state);

        let conflict = RegisterWorkItemExecution {
            work_item_id: "work-a".into(),
            record: work_item_record(WorkItemExecutionState::Paused {
                generation: 1,
                reason: "changed".into(),
            }),
        };
        assert!(register_work_item_execution(&registered.state, &conflict)
            .unwrap_err()
            .contains("different state"));
    }

    #[test]
    fn readiness_transition_is_fenced_and_advances_generation() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        let runnable = work_item_record(WorkItemExecutionState::Runnable {
            generation: 1,
            recovery_ref: None,
        });
        state.work_items.insert("work-a".into(), runnable.clone());
        let paused = WorkItemExecutionRecord {
            source_revision: 2,
            state: WorkItemExecutionState::Paused {
                generation: 2,
                reason: "manual hold".into(),
            },
        };
        let paused = set_work_item_readiness(
            &state,
            &SetWorkItemReadiness {
                command_id: "pause-work-a".into(),
                work_item_id: "work-a".into(),
                expected: runnable,
                record: paused,
            },
        )
        .unwrap()
        .state;
        let paused_record = paused.work_items.get("work-a").unwrap().clone();
        assert!(matches!(
            paused_record.state,
            WorkItemExecutionState::Paused { generation: 2, .. }
        ));

        let resumed = set_work_item_readiness(
            &paused,
            &SetWorkItemReadiness {
                command_id: "resume-work-a".into(),
                work_item_id: "work-a".into(),
                expected: paused_record,
                record: WorkItemExecutionRecord {
                    source_revision: 3,
                    state: WorkItemExecutionState::Runnable {
                        generation: 3,
                        recovery_ref: None,
                    },
                },
            },
        )
        .unwrap();
        assert!(matches!(
            resumed.state.work_items["work-a"].state,
            WorkItemExecutionState::Runnable { generation: 3, .. }
        ));

        let stale = SetWorkItemReadiness {
            command_id: "stale-pause".into(),
            work_item_id: "work-a".into(),
            expected: work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
            record: WorkItemExecutionRecord {
                source_revision: 4,
                state: WorkItemExecutionState::Paused {
                    generation: 2,
                    reason: "stale".into(),
                },
            },
        };
        assert!(set_work_item_readiness(&resumed.state, &stale)
            .unwrap_err()
            .contains("fence is stale"));
    }

    #[test]
    fn continuation_resume_reuses_work_item_outcome_planner() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        let paused = work_item_record(WorkItemExecutionState::Paused {
            generation: 2,
            reason: "yielded_to:work-b".into(),
        });
        state.work_items.insert("work-a".into(), paused.clone());

        let resumed = resume_work_item_continuation(
            &state,
            &ResumeWorkItemContinuation {
                command_id: "resume:continuation-a".into(),
                work_item_id: "work-a".into(),
                active_work_item_id: "work-b".into(),
                continuation_id: "continuation-a".into(),
                expected: paused.clone(),
                source: WorkItemContinuationResumeSource {
                    work_item_revision: 1,
                    blocked_by: None,
                    active_wait_ids: Vec::new(),
                },
                outcome: WorkItemOutcome::Continue,
            },
        )
        .unwrap();
        assert_eq!(
            resumed.state.work_items["work-a"].state,
            WorkItemExecutionState::Runnable {
                generation: 3,
                recovery_ref: None,
            }
        );

        let stale = ResumeWorkItemContinuation {
            command_id: "resume:continuation-a:stale".into(),
            work_item_id: "work-a".into(),
            active_work_item_id: "work-b".into(),
            continuation_id: "continuation-a".into(),
            expected: paused,
            source: WorkItemContinuationResumeSource {
                work_item_revision: 1,
                blocked_by: None,
                active_wait_ids: Vec::new(),
            },
            outcome: WorkItemOutcome::Continue,
        };
        assert!(resume_work_item_continuation(&resumed.state, &stale)
            .unwrap_err()
            .contains("stale"));
    }

    #[test]
    fn continuation_suspension_requires_runnable_parent_and_exact_fence() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        let runnable = work_item_record(WorkItemExecutionState::Runnable {
            generation: 2,
            recovery_ref: None,
        });
        state.work_items.insert("work-a".into(), runnable.clone());

        let suspended = suspend_work_item_continuation(
            &state,
            &SuspendWorkItemContinuation {
                command_id: "suspend:continuation-a".into(),
                work_item_id: "work-a".into(),
                active_work_item_id: "work-b".into(),
                continuation_id: "continuation-a".into(),
                expected: runnable.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            suspended.state.work_items["work-a"].state,
            WorkItemExecutionState::Paused {
                generation: 3,
                reason: "yielded_to:work-b".into(),
            }
        );

        let stale = SuspendWorkItemContinuation {
            command_id: "suspend:continuation-a:stale".into(),
            work_item_id: "work-a".into(),
            active_work_item_id: "work-b".into(),
            continuation_id: "continuation-a".into(),
            expected: runnable,
        };
        assert!(suspend_work_item_continuation(&suspended.state, &stale)
            .unwrap_err()
            .contains("stale"));
    }

    #[test]
    fn continuation_reconciliation_advances_generation_and_preserves_source_revision() {
        let expected = work_item_record(WorkItemExecutionState::Paused {
            generation: 7,
            reason: "yielded_to:old-child".into(),
        });
        let state = ExecutionProtocolState {
            agent_id: "agent-a".into(),
            attempts: BTreeMap::new(),
            work_items: BTreeMap::from([("parent".into(), expected.clone())]),
            outcomes: BTreeMap::new(),
        };
        let command = ReconcileWorkItemContinuationYield {
            command_id: "reconcile:frame-a".into(),
            work_item_id: "parent".into(),
            continuation_id: "frame-a".into(),
            stale_active_work_item_id: "old-child".into(),
            active_work_item_id: "new-child".into(),
            expected: expected.clone(),
            expected_frame_updated_at: "2026-08-11T00:00:00Z".into(),
            active_work_item_source_revision: 3,
            stale_active_work_item_source_revision: 4,
        };

        let transition = reconcile_work_item_continuation_yield(&state, &command).unwrap();
        assert_eq!(
            transition.state.work_items["parent"],
            WorkItemExecutionRecord {
                source_revision: expected.source_revision,
                state: WorkItemExecutionState::Paused {
                    generation: 8,
                    reason: "yielded_to:new-child".into(),
                },
            }
        );
        let stale = ReconcileWorkItemContinuationYield {
            expected: WorkItemExecutionRecord {
                source_revision: expected.source_revision,
                state: WorkItemExecutionState::Paused {
                    generation: 6,
                    reason: "yielded_to:old-child".into(),
                },
            },
            ..command.clone()
        };
        assert!(reconcile_work_item_continuation_yield(&state, &stale)
            .unwrap_err()
            .contains("fence is stale"));
        let matched = ReconcileWorkItemContinuationYield {
            stale_active_work_item_id: "new-child".into(),
            ..command.clone()
        };
        assert!(reconcile_work_item_continuation_yield(&state, &matched)
            .unwrap_err()
            .contains("distinct identity"));
        for aliased_child in ["old-child", "new-child"] {
            let aliased = ReconcileWorkItemContinuationYield {
                work_item_id: aliased_child.into(),
                ..command.clone()
            };
            assert!(reconcile_work_item_continuation_yield(&state, &aliased)
                .unwrap_err()
                .contains("distinct identity"));
        }
    }

    #[test]
    fn source_revision_can_advance_without_stealing_in_flight_ownership() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: attempt("attempt-a", None),
            },
        )
        .unwrap()
        .state;
        let advanced = advance_work_item_source_revision(
            &admitted,
            &AdvanceWorkItemSourceRevision {
                command_id: "advance-work-a".into(),
                work_item_id: "work-a".into(),
                expected_source_revision: 1,
                source_revision: 2,
            },
        )
        .unwrap();
        let record = &advanced.state.work_items["work-a"];
        assert_eq!(record.source_revision, 2);
        assert!(matches!(
            record.state,
            WorkItemExecutionState::InFlight {
                ref attempt_id,
                generation: 1
            } if attempt_id == "attempt-a"
        ));
    }

    #[test]
    fn interrupted_attempt_releases_lane_and_allows_model_reentry() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: attempt("attempt-1", None),
            },
        )
        .unwrap();
        let interrupted = interrupt_execution(
            &admitted.state,
            &InterruptExecution {
                attempt_id: "attempt-1".into(),
                outcome_id: "outcome-1".into(),
                reason: "runtime_restart".into(),
                interrupted_at: "2026-08-01T00:01:00Z".into(),
            },
        )
        .unwrap();
        assert!(interrupted.state.open_attempt().is_none());
        assert!(matches!(
            interrupted.state.work_items["work-a"].state,
            WorkItemExecutionState::Runnable {
                generation: 2,
                recovery_ref: Some(_)
            }
        ));

        let recovered = admit_execution(
            &interrupted.state,
            &AdmitExecution {
                attempt: attempt("attempt-2", Some("attempt-1")),
            },
        )
        .unwrap();
        assert_eq!(
            recovered
                .state
                .open_attempt()
                .map(|attempt| attempt.attempt_id.as_str()),
            Some("attempt-2")
        );
    }

    #[test]
    fn wait_resume_admits_from_waiting_and_settles_to_runnable() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Waiting {
                generation: 1,
                wait: WaitReference {
                    wait_id: "wait-1".into(),
                },
            }),
        );
        let mut wait_attempt = attempt("attempt-1", None);
        wait_attempt.source.identity = ExecutionSourceIdentity::TriggeredWait {
            wait_id: "wait-1".into(),
            trigger_message_id: "trigger-1".into(),
            ingress: None,
        };
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: wait_attempt,
            },
        )
        .unwrap();
        assert!(matches!(
            admitted.state.work_items["work-a"].state,
            WorkItemExecutionState::InFlight { generation: 1, .. }
        ));
        let settled = settle_execution(
            &admitted.state,
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "outcome-1".into(),
                    attempt_id: "attempt-1".into(),
                    outcome: ExecutionOutcome::WorkItem(WorkItemOutcome::Continue),
                    created_at: "2026-08-01T00:01:00Z".into(),
                },
            },
        )
        .unwrap();
        assert!(matches!(
            settled.state.work_items["work-a"].state,
            WorkItemExecutionState::Runnable {
                generation: 2,
                recovery_ref: None
            }
        ));
    }

    #[test]
    fn source_binding_matrix_rejects_unsupported_combinations() {
        use ExecutionBinding::*;

        let conversation = Conversation {
            interaction_id: "i1".into(),
        };
        let work_item = WorkItem {
            work_item_id: "w1".into(),
        };
        let agent_lifecycle = AgentLifecycle {
            agent_id: "agent-a".into(),
        };
        let fences = AdmittedFences {
            source_revision: 1,
            work_item_source_revision: None,
            work_item_generation: None,
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        };
        let source = |identity| ExecutionSource {
            identity,
            generation: 1,
        };

        // Allowed combinations
        let queue = source(ExecutionSourceIdentity::QueueMessage {
            message_id: "m1".into(),
        });
        let continuation = source(ExecutionSourceIdentity::WorkItemContinuation {
            work_item_id: "w1".into(),
        });
        let recovery = source(ExecutionSourceIdentity::RuntimeRecovery {
            recovery_id: "r1".into(),
        });
        assert!(validate_source_binding(&queue, &conversation, &fences).is_ok());
        assert!(validate_source_binding(&queue, &work_item, &fences).is_ok());
        assert!(validate_source_binding(&queue, &agent_lifecycle, &fences).is_ok());
        assert!(validate_source_binding(&continuation, &work_item, &fences).is_ok());
        assert!(validate_source_binding(&recovery, &ExecutionBinding::Command, &fences).is_ok());
        assert!(validate_source_binding(&recovery, &work_item, &fences).is_ok());
        assert!(validate_source_binding(&recovery, &conversation, &fences).is_ok());
        assert!(validate_source_binding(&recovery, &agent_lifecycle, &fences).is_ok());

        // Rejected combinations
        assert!(validate_source_binding(&queue, &ExecutionBinding::Command, &fences).is_err());
        assert!(validate_source_binding(&continuation, &conversation, &fences).is_err());
    }

    #[test]
    fn task_and_child_results_require_exact_live_rejoin_fences() {
        let binding = ExecutionBinding::WorkItem {
            work_item_id: "work-a".into(),
        };
        let task = ExecutionSource {
            identity: ExecutionSourceIdentity::TaskResult {
                task_id: "task-1".into(),
                result_message_id: "message-1".into(),
            },
            generation: 3,
        };
        let child = ExecutionSource {
            identity: ExecutionSourceIdentity::ChildResult {
                child_agent_id: "child-a".into(),
                task_id: "task-2".into(),
                result_message_id: "message-2".into(),
            },
            generation: 4,
        };
        let mut fences = AdmittedFences {
            source_revision: 3,
            work_item_source_revision: Some(1),
            work_item_generation: Some(1),
            rejoin: None,
            agent_control_revision: 1,
            host_registry_revision: 1,
        };
        assert!(validate_source_binding(&task, &binding, &fences)
            .unwrap_err()
            .contains("live rejoin"));

        fences.rejoin = Some(RejoinFence {
            obligation_id: "rejoin-task-1".into(),
            generation: 1,
            source: crate::domain::execution_protocol::TaskRejoinSource::ParentTurn {
                parent_turn_id: "turn-parent".into(),
            },
        });
        assert!(validate_source_binding(&task, &binding, &fences).is_ok());

        fences.source_revision = 4;
        fences.rejoin = Some(RejoinFence {
            obligation_id: String::new(),
            generation: 1,
            source: crate::domain::execution_protocol::TaskRejoinSource::ParentTurn {
                parent_turn_id: "turn-parent".into(),
            },
        });
        assert!(validate_source_binding(&child, &binding, &fences)
            .unwrap_err()
            .contains("identity"));
    }

    #[test]
    fn admission_rejects_stale_source_fence_and_invalid_provenance() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let mut stale = attempt("attempt-stale", None);
        stale.admitted_fences.source_revision = 2;
        assert!(admit_execution(&state, &AdmitExecution { attempt: stale })
            .unwrap_err()
            .contains("fences"));

        let mut invalid = attempt("attempt-invalid-provenance", None);
        invalid.provenance.origin = ExecutionOrigin::Operator;
        assert!(
            admit_execution(&state, &AdmitExecution { attempt: invalid })
                .unwrap_err()
                .contains("provenance")
        );
    }

    #[test]
    fn triggered_wait_followup_ingress_keeps_provenance_boundary() {
        let mut followup = attempt("followup-wait", None);
        followup.source.identity = ExecutionSourceIdentity::TriggeredWait {
            wait_id: "wait-1".into(),
            trigger_message_id: "reply-1".into(),
            ingress: None,
        };
        followup.provenance.trust = ExecutionTrust::OperatorInstruction;
        assert!(validate_provenance(&followup).is_err());

        followup.source.identity = ExecutionSourceIdentity::TriggeredWait {
            wait_id: "wait-1".into(),
            trigger_message_id: "reply-1".into(),
            ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
        };
        for origin in [ExecutionOrigin::System, ExecutionOrigin::Task] {
            followup.provenance.origin = origin;
            for binding in [
                ExecutionBinding::WorkItem {
                    work_item_id: "work-a".into(),
                },
                ExecutionBinding::AgentLifecycle {
                    agent_id: "agent-a".into(),
                },
            ] {
                followup.binding = binding;
                assert!(validate_provenance(&followup).is_ok());
            }
            followup.binding = ExecutionBinding::Conversation {
                interaction_id: "interaction-a".into(),
            };
            assert!(validate_provenance(&followup).is_err());
        }
        followup.binding = ExecutionBinding::WorkItem {
            work_item_id: "work-a".into(),
        };
        followup.provenance.origin = ExecutionOrigin::Channel;
        assert!(validate_provenance(&followup).is_err());
    }

    #[test]
    fn triggered_wait_ingress_serialization_is_backward_compatible() {
        let legacy = serde_json::json!({
            "kind": "triggered_wait",
            "wait_id": "wait-1",
            "trigger_message_id": "reply-1"
        });
        let source: ExecutionSourceIdentity = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&source).unwrap(), legacy);
        let marked = ExecutionSourceIdentity::TriggeredWait {
            wait_id: "wait-1".into(),
            trigger_message_id: "reply-1".into(),
            ingress: Some(ExecutionWaitIngress::RuntimeOwnedInternalFollowup),
        };
        let restored: ExecutionSourceIdentity =
            serde_json::from_value(serde_json::to_value(&marked).unwrap()).unwrap();
        assert_eq!(restored, marked);
    }

    #[test]
    fn admission_accepts_operator_origin_with_low_content_trust() {
        for (index, trust) in [
            (1, ExecutionTrust::IntegrationSignal),
            (2, ExecutionTrust::ExternalEvidence),
        ] {
            let mut state = ExecutionProtocolState::empty("agent-a");
            state.work_items.insert(
                "work-a".into(),
                work_item_record(WorkItemExecutionState::Runnable {
                    generation: 1,
                    recovery_ref: None,
                }),
            );
            let mut attempt = attempt(&format!("attempt-operator-low-trust-{index}"), None);
            attempt.provenance.origin = ExecutionOrigin::Operator;
            attempt.provenance.trust = trust;
            let transition = admit_execution(&state, &AdmitExecution { attempt }).unwrap();
            state = transition.state;
            let attempt_id = format!("attempt-operator-low-trust-{index}");
            assert_eq!(state.attempts[&attempt_id].provenance.trust, trust);
        }
    }

    #[test]
    fn admission_fences_source_and_work_item_revisions_independently() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let mut independent = attempt("attempt-independent-fences", None);
        independent.source.generation = 7;
        independent.admitted_fences.source_revision = 7;
        let transition = admit_execution(
            &state,
            &AdmitExecution {
                attempt: independent,
            },
        )
        .unwrap();
        assert!(matches!(
            transition.state.work_items["work-a"].state,
            WorkItemExecutionState::InFlight { generation: 1, .. }
        ));
    }

    #[test]
    fn source_identity_must_match_bound_work_item() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let mut mismatched = attempt("attempt-mismatch", None);
        mismatched.source.identity = ExecutionSourceIdentity::WorkItemContinuation {
            work_item_id: "work-other".into(),
        };
        assert!(admit_execution(
            &state,
            &AdmitExecution {
                attempt: mismatched,
            },
        )
        .unwrap_err()
        .contains("unsupported source-binding"));
    }

    #[test]
    fn provider_recovery_inherits_complete_unbound_predecessor_binding() {
        for binding in [
            ExecutionBinding::Conversation {
                interaction_id: "original-interaction".into(),
            },
            ExecutionBinding::AgentLifecycle {
                agent_id: "agent-a".into(),
            },
        ] {
            let mut source = attempt("source", None);
            source.source.identity = ExecutionSourceIdentity::QueueMessage {
                message_id: "message:source".into(),
            };
            source.binding = binding.clone();
            source.admitted_fences.work_item_source_revision = None;
            source.admitted_fences.work_item_generation = None;
            let admitted = admit_execution(
                &ExecutionProtocolState::empty("agent-a"),
                &AdmitExecution { attempt: source },
            )
            .unwrap();
            let settled = settle_execution(
                &admitted.state,
                &SettleExecution {
                    outcome: ExecutionOutcomeRecord {
                        outcome_id: "source-outcome".into(),
                        attempt_id: "source".into(),
                        outcome: ExecutionOutcome::Conversation(ConversationOutcome::Failed {
                            policy: "provider-failure".into(),
                        }),
                        created_at: "2026-10-10T00:00:00Z".into(),
                    },
                },
            )
            .unwrap();
            let mut recovery = attempt("recovery", Some("source"));
            recovery.source.identity = ExecutionSourceIdentity::RuntimeRecovery {
                recovery_id: "message:recovery".into(),
            };
            recovery.binding = binding;
            recovery.provenance.origin = ExecutionOrigin::RuntimeRecovery;
            recovery.admitted_fences.work_item_source_revision = None;
            recovery.admitted_fences.work_item_generation = None;
            assert!(admit_execution(
                &settled.state,
                &AdmitExecution {
                    attempt: recovery.clone()
                }
            )
            .is_ok());
            recovery.binding = ExecutionBinding::Conversation {
                interaction_id: "unrelated-interaction".into(),
            };
            assert!(admit_execution(
                &settled.state,
                &AdmitExecution {
                    attempt: recovery.clone()
                }
            )
            .unwrap_err()
            .contains("inherit"));
            recovery.recovery_of_attempt_id = None;
            assert!(
                admit_execution(&settled.state, &AdmitExecution { attempt: recovery })
                    .unwrap_err()
                    .contains("predecessor")
            );
        }
    }

    #[test]
    fn runtime_recovery_command_settles_without_workitem() {
        let state = ExecutionProtocolState::empty("agent-a");
        let recovery_attempt = ExecutionAttempt {
            input_projection: None,
            attempt_id: "recovery-1".into(),
            agent_id: "agent-a".into(),
            source_message_id: None,
            source: ExecutionSource {
                identity: ExecutionSourceIdentity::RuntimeRecovery {
                    recovery_id: "bootstrap-recovery".into(),
                },
                generation: 1,
            },
            binding: ExecutionBinding::Command,
            provenance: ExecutionProvenance {
                origin: ExecutionOrigin::RuntimeRecovery,
                trust: ExecutionTrust::RuntimeInstruction,
                priority: ExecutionPriority::Background,
                correlation_id: None,
                causation_id: None,
            },
            admitted_fences: AdmittedFences {
                source_revision: 1,
                work_item_source_revision: None,
                work_item_generation: None,
                rejoin: None,
                agent_control_revision: 1,
                host_registry_revision: 1,
            },
            state: ExecutionAttemptState::Open,
            run_id: None,
            turn_id: None,
            recovery_of_attempt_id: None,
            terminal_outcome_id: None,
            admitted_at: "2026-08-01T00:00:00Z".into(),
            terminal_at: None,
        };
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: recovery_attempt,
            },
        )
        .unwrap();
        assert!(admitted.state.work_items.is_empty());
        let settled = settle_execution(
            &admitted.state,
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "recovery-outcome-1".into(),
                    attempt_id: "recovery-1".into(),
                    outcome: ExecutionOutcome::Command(CommandResult::Applied {
                        references: vec!["recovered:attempt-0".into()],
                    }),
                    created_at: "2026-08-01T00:01:00Z".into(),
                },
            },
        )
        .unwrap();
        assert!(settled.state.open_attempt().is_none());
        assert!(matches!(
            settled.state.attempts["recovery-1"].state,
            ExecutionAttemptState::Settled
        ));
    }

    #[test]
    fn command_binding_settles_with_unsupported_and_quarantined() {
        fn make_command_attempt(id: &str) -> ExecutionAttempt {
            ExecutionAttempt {
                input_projection: None,
                attempt_id: id.into(),
                agent_id: "agent-a".into(),
                source_message_id: None,
                source: ExecutionSource {
                    identity: ExecutionSourceIdentity::RuntimeRecovery {
                        recovery_id: "recovery".into(),
                    },
                    generation: 1,
                },
                binding: ExecutionBinding::Command,
                provenance: ExecutionProvenance {
                    origin: ExecutionOrigin::RuntimeRecovery,
                    trust: ExecutionTrust::RuntimeInstruction,
                    priority: ExecutionPriority::Background,
                    correlation_id: None,
                    causation_id: None,
                },
                admitted_fences: AdmittedFences {
                    source_revision: 1,
                    work_item_source_revision: None,
                    work_item_generation: None,
                    rejoin: None,
                    agent_control_revision: 1,
                    host_registry_revision: 1,
                },
                state: ExecutionAttemptState::Open,
                run_id: None,
                turn_id: None,
                recovery_of_attempt_id: None,
                terminal_outcome_id: None,
                admitted_at: "2026-08-01T00:00:00Z".into(),
                terminal_at: None,
            }
        }

        let state = ExecutionProtocolState::empty("agent-a");
        for (outcome_id, result) in [
            (
                "out-unsupported",
                CommandResult::Unsupported {
                    reason: "no matching candidate".into(),
                },
            ),
            (
                "out-quarantined",
                CommandResult::Quarantined {
                    reason: "untrusted source".into(),
                },
            ),
        ] {
            let admitted = admit_execution(
                &state,
                &AdmitExecution {
                    attempt: make_command_attempt("cmd-1"),
                },
            )
            .unwrap();
            let settled = settle_execution(
                &admitted.state,
                &SettleExecution {
                    outcome: ExecutionOutcomeRecord {
                        outcome_id: outcome_id.into(),
                        attempt_id: "cmd-1".into(),
                        outcome: ExecutionOutcome::Command(result),
                        created_at: "2026-08-01T00:01:00Z".into(),
                    },
                },
            )
            .unwrap();
            assert!(settled.state.open_attempt().is_none());
            assert!(matches!(
                settled.state.attempts["cmd-1"].state,
                ExecutionAttemptState::Settled
            ));
        }
    }

    #[test]
    fn settlement_rejects_owner_incompatible_outcome() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: attempt("attempt-1", None),
            },
        )
        .unwrap();
        let error = settle_execution(
            &admitted.state,
            &SettleExecution {
                outcome: ExecutionOutcomeRecord {
                    outcome_id: "outcome-1".into(),
                    attempt_id: "attempt-1".into(),
                    outcome: ExecutionOutcome::Conversation(ConversationOutcome::Replied),
                    created_at: "2026-08-01T00:01:00Z".into(),
                },
            },
        )
        .unwrap_err();
        assert!(error.contains("incompatible"));
    }

    #[test]
    fn finalize_interrupted_execution_releases_replay_anchor_only() {
        let mut state = ExecutionProtocolState::empty("agent-a");
        state.work_items.insert(
            "work-a".into(),
            work_item_record(WorkItemExecutionState::Runnable {
                generation: 1,
                recovery_ref: None,
            }),
        );
        let admitted = admit_execution(
            &state,
            &AdmitExecution {
                attempt: attempt("attempt-1", None),
            },
        )
        .unwrap();
        let interrupted = interrupt_execution(
            &admitted.state,
            &InterruptExecution {
                attempt_id: "attempt-1".into(),
                outcome_id: "outcome-1".into(),
                reason: "runtime_interrupted".into(),
                interrupted_at: "2026-08-01T00:01:00Z".into(),
            },
        )
        .unwrap();
        assert_eq!(
            interrupted.state.attempts["attempt-1"].state,
            ExecutionAttemptState::Interrupted
        );

        let finalized = finalize_interrupted_execution(
            &interrupted.state,
            &FinalizeInterruptedExecution {
                command_id: "startup_finalize_interrupted:attempt-1".into(),
                attempt_id: "attempt-1".into(),
                reason: "startup_recovery/interrupted_replay_source_aborted".into(),
                finalized_at: "2026-08-02T00:00:00Z".into(),
            },
        )
        .unwrap();
        let attempt = &finalized.state.attempts["attempt-1"];
        assert_eq!(attempt.state, ExecutionAttemptState::Settled);
        assert_eq!(attempt.terminal_outcome_id.as_deref(), Some("outcome-1"));

        // Finalization is a one-way release: a settled attempt cannot
        // re-finalize, and identity fields are mandatory.
        let error = finalize_interrupted_execution(
            &finalized.state,
            &FinalizeInterruptedExecution {
                command_id: "retry".into(),
                attempt_id: "attempt-1".into(),
                reason: "again".into(),
                finalized_at: "2026-08-03T00:00:00Z".into(),
            },
        )
        .unwrap_err();
        assert!(error.contains("only an interrupted attempt may finalize"));
        let error = finalize_interrupted_execution(
            &interrupted.state,
            &FinalizeInterruptedExecution {
                command_id: String::new(),
                attempt_id: "attempt-1".into(),
                reason: "again".into(),
                finalized_at: "2026-08-03T00:00:00Z".into(),
            },
        )
        .unwrap_err();
        assert!(error.contains("complete identity"));
    }
}

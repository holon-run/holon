use super::*;
use holon_domain::{AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority};

struct ResultFields {
    result_identity: String,
    task_id: String,
    task_status: String,
    work_item_id: Option<String>,
    source: TaskRejoinSource,
    rejoin_generation: u64,
}

impl ResultFields {
    fn header(&self) -> TaskResultHeader<'_> {
        TaskResultHeader {
            result_identity: &self.result_identity,
            task_id: &self.task_id,
            task_status: &self.task_status,
            work_item_id: self.work_item_id.as_deref(),
            source: &self.source,
            rejoin_generation: self.rejoin_generation,
        }
    }
}

/// Trial one projected candidate without materializing its retained preview.
fn pending_task_results_preview_measure_with_projected_candidate<'a>(
    pending: impl Iterator<Item = &'a PendingTaskResultPreview> + Clone,
    current_message: &MessageEnvelope,
    candidate: &ResultFields,
    source_projection: &'a IdleInputBatchItem,
    reply_projection: Option<&'a IdleInputBatchItem>,
) -> (usize, usize) {
    measure_with_projected_result(
        pending,
        current_message,
        candidate.header(),
        source_projection.into(),
        reply_projection.map(TaskResultProjection::from),
    )
}

#[test]
fn pending_preview_borrowed_dedup_matches_original_formatter() {
    let previews = (0..12)
        .map(|index| PendingTaskResultPreview {
            lines: vec![
                format!("result-{index}: é\n你好"),
                format!("source-body-{index}"),
                format!("reply-body-{index}"),
            ],
            result_identity: format!("result-{}", index % 8),
            source_id: Some(format!("message-{}", index % 3)),
            source_lines: 1..2,
            reply_id: Some(format!("message-{}", index % 2)),
            reply_lines: 2..3,
        })
        .collect::<Vec<_>>();
    let mut current = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "current".into(),
        },
    );
    for anchor in ["unrelated", "message-0", "message-1"] {
        current.id = anchor.into();
        for excluded in [BTreeSet::new(), BTreeSet::from(["message-2".into()])] {
            for count in [0, 1, 2, 8, 12] {
                let pending = &previews[..count];
                let mut expected = String::new();
                let mut seen = BTreeSet::new();
                let mut identities = BTreeSet::new();
                if count != 0 {
                    expected.push_str("Previously received terminal task results admitted to this execution (each result identity appears once):");
                }
                for entry in pending {
                    if !identities.insert(entry.result_identity.as_str()) {
                        continue;
                    }
                    let source = entry.source_id.as_deref().is_some_and(|id| {
                        id != current.id && !excluded.contains(id) && seen.insert(id)
                    });
                    let reply = entry.reply_id.as_deref().is_some_and(|id| {
                        id != current.id && !excluded.contains(id) && seen.insert(id)
                    });
                    for (index, line) in entry.lines.iter().enumerate() {
                        if (source || !entry.source_lines.contains(&index))
                            && (reply || !entry.reply_lines.contains(&index))
                        {
                            expected.push('\n');
                            expected.push_str(line);
                        }
                    }
                }
                let mut rendered = String::new();
                write_pending_task_result_previews(
                    &mut rendered,
                    pending.iter(),
                    count,
                    &current,
                    &excluded,
                )
                .unwrap();
                assert_eq!(rendered, expected);
                if excluded.is_empty() {
                    assert_eq!(
                        pending_task_results_preview_measure(pending.iter(), &current),
                        (expected.len(), expected.chars().count())
                    );
                }
            }
        }
    }
}

fn projected_pending_preview_fixture(
    index: usize,
) -> (
    ResultFields,
    MessageEnvelope,
    IdleInputBatchItem,
    IdleInputBatchItem,
) {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::TaskResult,
        MessageOrigin::Task {
            task_id: format!("projected-task-{index}"),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: format!("legacy-source-{index}"),
        },
    );
    message.id = format!("projected-source-{index}");
    let record = ResultFields {
        result_identity: format!("result-{index}"),
        task_id: format!("projected-task-{index}"),
        task_status: "completed".into(),
        work_item_id: Some("work-current".into()),
        source: TaskRejoinSource::ParentTurn {
            parent_turn_id: "parent-turn".into(),
        },
        rejoin_generation: 1,
    };
    let source = IdleInputBatchItem {
        message_id: message.id.clone(),
        source_ref: format!("message:{}", message.id),
        rendered: format!("projected-source-body-{index}\nsecond source line\n"),
    };
    let reply = IdleInputBatchItem {
        message_id: format!("projected-reply-{index}"),
        source_ref: format!("message:projected-reply-{index}"),
        rendered: format!("projected-reply-body-{index}\nsecond reply line\n"),
    };
    (record, message, source, reply)
}

fn assert_projected_pending_preview_equivalent(
    pending: &[PendingTaskResultPreview],
    current: &MessageEnvelope,
    candidate: &ResultFields,
    source: &IdleInputBatchItem,
    reply: Option<&IdleInputBatchItem>,
) -> String {
    let retained = pending_task_result_preview(candidate.header(), None, None, Some(source), reply);
    assert_eq!(retained.lines.capacity(), PENDING_TASK_RESULT_PREVIEW_LINES);
    assert!(retained.lines.len() <= PENDING_TASK_RESULT_PREVIEW_LINES);
    let mut expected = String::new();
    write_pending_task_result_previews(
        &mut expected,
        pending.iter().chain(std::iter::once(&retained)),
        pending.len() + 1,
        current,
        &BTreeSet::new(),
    )
    .unwrap();
    let mut borrowed = String::new();
    write_pending_task_result_preview_entries(
        &mut borrowed,
        pending
            .iter()
            .map(PendingTaskResultPreviewEntry::Retained)
            .chain(std::iter::once(PendingTaskResultPreviewEntry::Projected {
                record: candidate.header(),
                source: source.into(),
                reply: reply.map(TaskResultProjection::from),
            })),
        pending.len() + 1,
        current,
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(borrowed, expected);
    // Filter deliberately removes ExactSizeIterator, as in selection trials.
    assert_eq!(
        pending_task_results_preview_measure_with_projected_candidate(
            pending.iter().filter(|_| true),
            current,
            candidate,
            source,
            reply,
        ),
        (expected.len(), expected.chars().count())
    );
    expected
}

#[test]
fn pending_projected_candidate_measure_matches_retained_formatter() {
    let (mut record, mut current, source, reply) = projected_pending_preview_fixture(0);
    current.id = "unrelated-anchor".into();
    for command in [false, true] {
        record.source = if command {
            TaskRejoinSource::CommandCreation {
                command_creation_id: "command-creation".into(),
            }
        } else {
            TaskRejoinSource::ParentTurn {
                parent_turn_id: "parent-turn".into(),
            }
        };
        for owner in [None, Some("work-current".to_string())] {
            record.work_item_id = owner;
            for reply in [None, Some(&reply)] {
                let rendered = assert_projected_pending_preview_equivalent(
                    &[],
                    &current,
                    &record,
                    &source,
                    reply,
                );
                let source_header = if command {
                    "command_creation_id: command-creation"
                } else {
                    "parent_turn_id: parent-turn"
                };
                let owner = record
                    .work_item_id
                    .as_ref()
                    .map(|id| format!("work_item:{id}"))
                    .unwrap_or_else(|| "agent_lifecycle".into());
                assert_eq!(
                    rendered,
                    format!(
                        "Previously received terminal task results admitted to this execution (each result identity appears once):\n- result_identity: {}\n  task_id: {}\n  task_status: completed\n  owner: {owner}\n  {source_header}\n  rejoin_generation: 1\n{}{}",
                        record.result_identity,
                        record.task_id,
                        source.rendered,
                        reply.map(|reply| format!("\n{}", reply.rendered)).unwrap_or_default(),
                    )
                );
            }
        }
    }
}

#[test]
fn pending_projected_candidate_measure_matches_mixed_preview_paths() {
    let (legacy, message, _, _) = projected_pending_preview_fixture(0);
    let (projected, _, prior_source, prior_reply) = projected_pending_preview_fixture(1);
    let (candidate, mut current, source, reply) = projected_pending_preview_fixture(2);
    current.id = "unrelated-anchor".into();
    let pending = [
        pending_task_result_preview(legacy.header(), Some(&message), None, None, None),
        pending_task_result_preview(
            projected.header(),
            None,
            None,
            Some(&prior_source),
            Some(&prior_reply),
        ),
    ];
    let rendered = assert_projected_pending_preview_equivalent(
        &pending,
        &current,
        &candidate,
        &source,
        Some(&reply),
    );
    assert_eq!(
        pending[0].lines.capacity(),
        PENDING_TASK_RESULT_PREVIEW_LINES
    );
    assert!(pending[0].lines.len() <= PENDING_TASK_RESULT_PREVIEW_LINES);
    assert!(rendered.contains("summary: legacy-source-0"));
    assert!(rendered.contains("retrieval: use TaskStatus"));
    assert!(
        rendered.find(&prior_reply.rendered).unwrap() < rendered.find(&source.rendered).unwrap()
    );
    assert!(rendered.find(&source.rendered).unwrap() < rendered.find(&reply.rendered).unwrap());
}

#[test]
fn pending_projected_candidate_measure_preserves_identity_dedup() {
    let (mut candidate, mut current, source, reply) = projected_pending_preview_fixture(0);
    current.id = "unrelated-anchor".into();
    let pending = [
        PendingTaskResultPreview {
            lines: vec!["retained-result".into()],
            result_identity: candidate.result_identity.clone(),
            source_id: None,
            source_lines: 0..0,
            reply_id: None,
            reply_lines: 0..0,
        },
        PendingTaskResultPreview {
            lines: vec!["suppressed-duplicate".into()],
            result_identity: candidate.result_identity.clone(),
            source_id: Some(source.message_id.clone()),
            source_lines: 0..0,
            reply_id: Some(reply.message_id.clone()),
            reply_lines: 0..0,
        },
    ];
    let duplicate = assert_projected_pending_preview_equivalent(
        &pending,
        &current,
        &candidate,
        &source,
        Some(&reply),
    );
    assert!(!duplicate.contains("suppressed-duplicate"));
    assert!(!duplicate.contains(&source.rendered));
    assert_eq!(
        pending_task_results_preview_measure_with_projected_candidate(
            pending.iter(),
            &current,
            &candidate,
            &source,
            Some(&reply),
        ),
        pending_task_results_preview_measure(pending.iter(), &current)
    );
    candidate.result_identity.push_str("-distinct");
    let distinct = assert_projected_pending_preview_equivalent(
        &pending,
        &current,
        &candidate,
        &source,
        Some(&reply),
    );
    assert!(
        distinct.contains(&source.rendered),
        "duplicate identities cannot reserve Message IDs"
    );
    assert!(distinct.contains(&reply.rendered));
}

#[test]
fn pending_projected_candidate_measure_preserves_shared_messages_and_order() {
    let (prior, _, mut prior_source, mut prior_reply) = projected_pending_preview_fixture(0);
    let (candidate, mut current, source, mut reply) = projected_pending_preview_fixture(1);
    current.id = "unrelated-anchor".into();
    for share_source in [false, true] {
        for share_reply in [false, true] {
            prior_source.message_id = if share_source {
                source.message_id.clone()
            } else {
                "prior-only-source".into()
            };
            prior_reply.message_id = if share_reply {
                reply.message_id.clone()
            } else {
                "prior-only-reply".into()
            };
            let pending = [pending_task_result_preview(
                prior.header(),
                None,
                None,
                Some(&prior_source),
                Some(&prior_reply),
            )];
            let rendered = assert_projected_pending_preview_equivalent(
                &pending,
                &current,
                &candidate,
                &source,
                Some(&reply),
            );
            assert_eq!(rendered.contains(&source.rendered), !share_source);
            assert_eq!(rendered.contains(&reply.rendered), !share_reply);
        }
    }
    // Cross-role dedup and same-entry source-before-reply are unchanged.
    prior_reply.message_id = source.message_id.clone();
    prior_source.message_id = reply.message_id.clone();
    let pending = [pending_task_result_preview(
        prior.header(),
        None,
        None,
        Some(&prior_source),
        Some(&prior_reply),
    )];
    let rendered = assert_projected_pending_preview_equivalent(
        &pending,
        &current,
        &candidate,
        &source,
        Some(&reply),
    );
    assert!(!rendered.contains(&source.rendered));
    assert!(!rendered.contains(&reply.rendered));
    reply.message_id = source.message_id.clone();
    let rendered = assert_projected_pending_preview_equivalent(
        &[],
        &current,
        &candidate,
        &source,
        Some(&reply),
    );
    assert!(rendered.contains(&source.rendered));
    assert!(!rendered.contains(&reply.rendered));
}

#[test]
fn pending_projected_candidate_measure_preserves_anchor_and_exclusions() {
    let (record, mut current, source, reply) = projected_pending_preview_fixture(0);
    let retained =
        pending_task_result_preview(record.header(), None, None, Some(&source), Some(&reply));
    for anchor in ["unrelated-anchor", &source.message_id, &reply.message_id] {
        current.id = anchor.into();
        let rendered = assert_projected_pending_preview_equivalent(
            &[],
            &current,
            &record,
            &source,
            Some(&reply),
        );
        assert_eq!(
            rendered.contains(&source.rendered),
            anchor != source.message_id
        );
        assert_eq!(
            rendered.contains(&reply.rendered),
            anchor != reply.message_id
        );
        for excluded in [
            BTreeSet::new(),
            BTreeSet::from([source.message_id.clone()]),
            BTreeSet::from([reply.message_id.clone()]),
        ] {
            let mut expected = String::new();
            write_pending_task_result_previews(
                &mut expected,
                std::iter::once(&retained),
                1,
                &current,
                &excluded,
            )
            .unwrap();
            let mut borrowed = String::new();
            write_pending_task_result_preview_entries(
                &mut borrowed,
                std::iter::once(PendingTaskResultPreviewEntry::Projected {
                    record: record.header(),
                    source: (&source).into(),
                    reply: Some((&reply).into()),
                }),
                1,
                &current,
                &excluded,
            )
            .unwrap();
            assert_eq!(borrowed, expected);
        }
    }
}

#[test]
fn pending_projected_candidate_measure_preserves_unicode_inline_sanitization() {
    for value in [
        "",
        " \t\r\n",
        "plain",
        "  é\u{a0}你好\u{2003}🙂\r\n  fin  ",
        "\u{85}a\u{b}\u{c}b\u{2028}c\u{2029}",
        "a\u{200b}b\0c",
    ] {
        assert_eq!(
            PendingTaskResultInline(value).to_string(),
            sanitize_inline(value)
        );
    }
    let (mut record, mut current, mut source, mut reply) = projected_pending_preview_fixture(0);
    current.id = "unrelated-anchor".into();
    record.result_identity = "  résultat\u{a0}你好\n🙂  ".into();
    record.task_id = "\t任务\r\né\u{2003}🙂 ".into();
    record.task_status = "  完成\u{2028}é ".into();
    record.work_item_id = Some("  任务\n原样  ".into());
    record.source = TaskRejoinSource::CommandCreation {
        command_creation_id: "  création\u{85}你好\n🙂 ".into(),
    };
    record.rejoin_generation = u64::MAX;
    source.rendered = "  source: é\u{a0}你好\n🙂\tkeep whitespace\r\n".into();
    reply.rendered = "\nréponse: 你好🙂\n".into();
    let rendered =
        assert_projected_pending_preview_equivalent(&[], &current, &record, &source, Some(&reply));
    assert!(rendered.contains("- result_identity: résultat 你好 🙂\n"));
    assert!(rendered.contains("  task_id: 任务 é 🙂\n"));
    assert!(rendered.contains("  task_status: 完成 é\n"));
    assert!(rendered.contains("  owner: work_item:  任务\n原样  \n"));
    assert!(rendered.contains("  command_creation_id: création 你好 🙂\n"));
    assert!(rendered.contains("  rejoin_generation: 18446744073709551615\n"));
    assert!(rendered.contains(&source.rendered));
    assert!(rendered.contains(&reply.rendered));
    assert!(rendered.len() > rendered.chars().count());
}

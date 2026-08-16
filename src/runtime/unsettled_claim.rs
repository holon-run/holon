use crate::{domain::execution_protocol::ExecutionAttemptState, types::QueueEntryStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsettledClaimFacts {
    pub queue_status: QueueEntryStatus,
    pub attempt_state: ExecutionAttemptState,
    pub terminal_turn_completed: Option<bool>,
    pub replay_is_exactly_fenced: bool,
    pub recovery_of_attempt_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnsettledClaimDecision {
    SettleFromTerminal {
        queue_status: QueueEntryStatus,
        reason: &'static str,
    },
    InterruptAndRequeue {
        reason: &'static str,
    },
    InterruptAndQuarantine {
        reason: &'static str,
    },
    QuarantineSettled {
        reason: &'static str,
    },
    NoopAlreadyConverged,
}

pub(crate) fn plan_unsettled_claim(facts: &UnsettledClaimFacts) -> UnsettledClaimDecision {
    if facts.queue_status != QueueEntryStatus::Dequeued {
        return UnsettledClaimDecision::NoopAlreadyConverged;
    }
    if let Some(completed) = facts.terminal_turn_completed {
        return UnsettledClaimDecision::SettleFromTerminal {
            queue_status: if completed {
                QueueEntryStatus::Processed
            } else {
                QueueEntryStatus::Aborted
            },
            reason: "terminal_turn_settlement",
        };
    }
    if facts.attempt_state != ExecutionAttemptState::Open {
        return UnsettledClaimDecision::QuarantineSettled {
            reason: "terminal_attempt_missing_terminal_turn",
        };
    }
    // Recovery lineage takes precedence over a valid fence so one replay cannot recurse.
    if facts.recovery_of_attempt_id.is_some() {
        return UnsettledClaimDecision::InterruptAndQuarantine {
            reason: "bounded_replay_exhausted",
        };
    }
    if facts.replay_is_exactly_fenced {
        return UnsettledClaimDecision::InterruptAndRequeue {
            reason: "exact_fence_replay",
        };
    }
    UnsettledClaimDecision::InterruptAndQuarantine {
        reason: "replay_fence_ambiguous",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> UnsettledClaimFacts {
        UnsettledClaimFacts {
            queue_status: QueueEntryStatus::Dequeued,
            attempt_state: ExecutionAttemptState::Open,
            terminal_turn_completed: None,
            replay_is_exactly_fenced: false,
            recovery_of_attempt_id: None,
        }
    }

    #[test]
    fn terminal_turn_settles_without_replay() {
        assert_eq!(
            plan_unsettled_claim(&UnsettledClaimFacts {
                terminal_turn_completed: Some(true),
                ..facts()
            }),
            UnsettledClaimDecision::SettleFromTerminal {
                queue_status: QueueEntryStatus::Processed,
                reason: "terminal_turn_settlement",
            }
        );
    }

    #[test]
    fn exact_fence_allows_one_replay() {
        assert_eq!(
            plan_unsettled_claim(&UnsettledClaimFacts {
                replay_is_exactly_fenced: true,
                ..facts()
            }),
            UnsettledClaimDecision::InterruptAndRequeue {
                reason: "exact_fence_replay",
            }
        );
    }

    #[test]
    fn replay_attempt_is_quarantined_instead_of_looping() {
        assert_eq!(
            plan_unsettled_claim(&UnsettledClaimFacts {
                replay_is_exactly_fenced: true,
                recovery_of_attempt_id: Some("attempt:original".into()),
                ..facts()
            }),
            UnsettledClaimDecision::InterruptAndQuarantine {
                reason: "bounded_replay_exhausted",
            }
        );
    }

    #[test]
    fn ambiguous_replay_is_interrupt_quarantined() {
        assert_eq!(
            plan_unsettled_claim(&facts()),
            UnsettledClaimDecision::InterruptAndQuarantine {
                reason: "replay_fence_ambiguous",
            }
        );
    }

    #[test]
    fn settled_attempt_without_terminal_is_quarantined() {
        assert_eq!(
            plan_unsettled_claim(&UnsettledClaimFacts {
                attempt_state: ExecutionAttemptState::Interrupted,
                ..facts()
            }),
            UnsettledClaimDecision::QuarantineSettled {
                reason: "terminal_attempt_missing_terminal_turn",
            }
        );
    }
}

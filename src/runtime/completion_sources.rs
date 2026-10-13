//! Focused optional completion edges captured in the canonical read view.
//! These observations do not change ordinary input lookup availability.

use serde::{Deserialize, Serialize};

use super::resident_input_evidence::{
    EvidenceLocator, InputEvidenceFamily, InputEvidenceKey, PreparedInputEvidence,
    QualifiedInputEvidence,
};
use crate::types::TranscriptEntryKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompletionSourceEdge {
    BriefTranscript,
    IncomingTranscriptMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompletionOptionalAbsence {
    GlobalAbsent,
    HistoricalTableUnavailable,
}

/// Minted by same-transaction qualification after the head and owner qualify.
/// A source/head version or target write invalidates this exact observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletionOptionalSourceObservation {
    pub head: EvidenceLocator,
    pub source: EvidenceLocator,
    pub edge: CompletionSourceEdge,
    pub declared_target: InputEvidenceKey,
    pub availability: CompletionOptionalAbsence,
}

impl CompletionOptionalSourceObservation {
    pub(crate) fn matches(
        &self,
        head: &QualifiedInputEvidence,
        source: &QualifiedInputEvidence,
    ) -> bool {
        if self.head != head.locator
            || self.source != source.locator
            || self.head.scope != self.source.scope
            || self.head.scope.agent_id.is_empty()
            || self.head.scope.identity_incarnation == 0
            || self.head.revision == 0
            || self.source.revision == 0
            || self.head.content_digest.is_empty()
            || self.source.content_digest.is_empty()
        {
            return false;
        }
        let PreparedInputEvidence::Brief(brief) = &head.record else {
            return false;
        };
        let Some(transcript_id) = brief.content_source.transcript_entry_id() else {
            return false;
        };
        match (self.edge, &source.record) {
            (CompletionSourceEdge::BriefTranscript, PreparedInputEvidence::Brief(_)) => {
                self.head == self.source
                    && self.declared_target
                        == InputEvidenceKey::new(InputEvidenceFamily::Transcript, transcript_id)
            }
            (
                CompletionSourceEdge::IncomingTranscriptMessage,
                PreparedInputEvidence::Transcript(transcript),
            ) => {
                transcript.id == transcript_id
                    && transcript.kind == TranscriptEntryKind::IncomingMessage
                    && transcript
                        .related_message_id
                        .as_deref()
                        .is_some_and(|message_id| {
                            self.declared_target
                                == InputEvidenceKey::new(InputEvidenceFamily::Message, message_id)
                        })
            }
            _ => false,
        }
    }

    pub(crate) fn observes_key(&self, key: &InputEvidenceKey) -> bool {
        &self.head.key == key || &self.source.key == key || &self.declared_target == key
    }
}

/// Each owning path remains independent. A strict path to the same Transcript
/// must still resolve its Message even when a completion-only path may omit it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CompletionInputPath {
    Strict,
    Head(String),
    Transcript(String),
}

pub(crate) fn completion_references(
    owner: &QualifiedInputEvidence,
    path: &CompletionInputPath,
    head: Option<&QualifiedInputEvidence>,
    observations: &[CompletionOptionalSourceObservation],
    references: std::collections::BTreeSet<InputEvidenceKey>,
) -> anyhow::Result<Vec<(InputEvidenceKey, CompletionInputPath)>> {
    use CompletionInputPath::*;
    if matches!(path, Strict) {
        return Ok(references.into_iter().map(|key| (key, Strict)).collect());
    }
    let head = head.ok_or_else(|| {
        super::resident_input_evidence::InputEvidenceReadError::NeedsPreparation(
            InputEvidenceKey::new(
                InputEvidenceFamily::Brief,
                match path {
                    Head(id) | Transcript(id) => id,
                    Strict => unreachable!(),
                },
            ),
        )
    })?;
    let PreparedInputEvidence::Brief(brief) = &head.record else {
        anyhow::bail!("completion head is not a qualified Brief");
    };
    let transcript_key = brief
        .content_source
        .transcript_entry_id()
        .map(|id| InputEvidenceKey::new(InputEvidenceFamily::Transcript, id));
    match path {
        Head(id) => anyhow::ensure!(
            owner.locator == head.locator && brief.id == *id,
            "completion path differs from its qualified head"
        ),
        Transcript(id) => anyhow::ensure!(
            brief.id == *id && transcript_key.as_ref() == Some(&owner.locator.key),
            "completion path differs from its declared Transcript"
        ),
        Strict => unreachable!(),
    }
    Ok(references.into_iter().filter_map(|key| {
        let optional_edge = match path {
            Head(_) if transcript_key.as_ref() == Some(&key) => Some(CompletionSourceEdge::BriefTranscript),
            Transcript(_) if matches!(&owner.record, PreparedInputEvidence::Transcript(entry)
                if entry.kind == TranscriptEntryKind::IncomingMessage
                && entry.related_message_id.as_ref().is_some_and(|id| key == InputEvidenceKey::new(InputEvidenceFamily::Message,id))) => Some(CompletionSourceEdge::IncomingTranscriptMessage),
            _ => None,
        };
        if optional_edge.is_some_and(|edge| observations.iter().any(|proof|
            proof.edge == edge && proof.declared_target == key && proof.matches(head,owner))) {
            return None;
        }
        let child_path = if matches!(path, Head(_)) && optional_edge == Some(CompletionSourceEdge::BriefTranscript) {
            Transcript(brief.id.clone())
        } else { Strict };
        Some((key, child_path))
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        BriefContentSource, BriefContentSourceRelation, BriefKind, BriefRecord, TranscriptEntry,
    };

    fn qualified(record: PreparedInputEvidence) -> QualifiedInputEvidence {
        let key = record.key();
        QualifiedInputEvidence {
            locator: EvidenceLocator {
                scope: super::super::resident_input_evidence::InputEvidenceScope {
                    agent_id: "completion-owner".into(),
                    identity_incarnation: 4,
                },
                key,
                revision: 7,
                content_digest: "a".repeat(64),
            },
            record,
            created_at: "2026-10-12T00:00:00Z".into(),
            order_sequence: None,
            turn_index: None,
            owner: None,
            resident_bytes: 128,
            attempt_turn_keys: None,
        }
    }
    fn head(id: &str, transcript_id: &str) -> QualifiedInputEvidence {
        let mut brief = BriefRecord::new(
            "completion-owner",
            BriefKind::Result,
            "done",
            Some("unrelated-brief-message".into()),
            None,
        );
        brief.id = id.into();
        brief.content_source = BriefContentSource::TranscriptEntry {
            entry_id: transcript_id.into(),
            relation: BriefContentSourceRelation::DerivedFrom,
        };
        qualified(PreparedInputEvidence::Brief(brief))
    }
    fn transcript(kind: TranscriptEntryKind) -> QualifiedInputEvidence {
        let mut entry = TranscriptEntry::new(
            "completion-owner",
            kind,
            None,
            Some("source-message".into()),
            serde_json::json!({"text":"source"}),
        );
        entry.id = "source-transcript".into();
        qualified(PreparedInputEvidence::Transcript(entry))
    }
    fn observation(
        head: &QualifiedInputEvidence,
        source: &QualifiedInputEvidence,
        edge: CompletionSourceEdge,
        family: InputEvidenceFamily,
        id: &str,
    ) -> CompletionOptionalSourceObservation {
        CompletionOptionalSourceObservation {
            head: head.locator.clone(),
            source: source.locator.clone(),
            edge,
            declared_target: InputEvidenceKey::new(family, id),
            availability: CompletionOptionalAbsence::GlobalAbsent,
        }
    }
    #[test]
    fn old_observation_cannot_authorize_new_head_source_or_identity() {
        let head = head("head", "source-transcript");
        let source = transcript(TranscriptEntryKind::IncomingMessage);
        let proof = observation(
            &head,
            &source,
            CompletionSourceEdge::IncomingTranscriptMessage,
            InputEvidenceFamily::Message,
            "source-message",
        );
        assert!(proof.matches(&head, &source));
        let mut revised = head.clone();
        revised.locator.revision += 1;
        assert!(!proof.matches(&revised, &source));
        let mut rewritten = source.clone();
        rewritten.locator.content_digest = "b".repeat(64);
        assert!(!proof.matches(&head, &rewritten));
        let mut recreated = head.clone();
        recreated.locator.scope.identity_incarnation += 1;
        assert!(!proof.matches(&recreated, &source));
        let mut cross_scope = proof.clone();
        cross_scope.source.scope.agent_id = "foreign".into();
        assert!(!cross_scope.matches(&head, &source));
    }
    #[test]
    fn target_key_equality_does_not_transfer_an_observation_to_another_owner_edge() {
        let head = head("head", "missing-transcript");
        let proof = observation(
            &head,
            &head,
            CompletionSourceEdge::BriefTranscript,
            InputEvidenceFamily::Transcript,
            "missing-transcript",
        );
        assert!(proof.matches(&head, &head));
        let other = self::head("other-head", "missing-transcript");
        assert!(!proof.matches(&other, &other));
        let mut wrong_edge = proof.clone();
        wrong_edge.edge = CompletionSourceEdge::IncomingTranscriptMessage;
        assert!(!wrong_edge.matches(&head, &head));
        assert!(proof.observes_key(&proof.declared_target));
        assert!(proof.observes_key(&head.locator.key));
        assert!(!proof.observes_key(&other.locator.key));
    }
    #[test]
    fn incoming_source_uses_the_actual_transcript_message_and_never_brief_related_message() {
        let head = head("head", "source-transcript");
        let source = transcript(TranscriptEntryKind::IncomingMessage);
        let proof = observation(
            &head,
            &source,
            CompletionSourceEdge::IncomingTranscriptMessage,
            InputEvidenceFamily::Message,
            "source-message",
        );
        assert!(proof.matches(&head, &source));
        let mut wrong_target = proof.clone();
        wrong_target.declared_target.id = "unrelated-brief-message".into();
        assert!(!wrong_target.matches(&head, &source));
        let assistant = transcript(TranscriptEntryKind::AssistantRound);
        assert!(!proof.matches(&head, &assistant));
        let mut wrong_transcript = source.clone();
        if let PreparedInputEvidence::Transcript(entry) = &mut wrong_transcript.record {
            entry.id = "other-transcript".into();
        }
        assert!(!proof.matches(&head, &wrong_transcript));
    }
    #[test]
    fn an_independent_owning_path_keeps_the_same_source_message_strict() {
        let head = head("head", "source-transcript");
        let source = transcript(TranscriptEntryKind::IncomingMessage);
        let proof = observation(
            &head,
            &source,
            CompletionSourceEdge::IncomingTranscriptMessage,
            InputEvidenceFamily::Message,
            "source-message",
        );
        let optional = completion_references(
            &source,
            &CompletionInputPath::Transcript("head".into()),
            Some(&head),
            std::slice::from_ref(&proof),
            source.references(),
        )
        .unwrap();
        assert!(optional.is_empty());
        let strict = completion_references(
            &source,
            &CompletionInputPath::Strict,
            None,
            std::slice::from_ref(&proof),
            source.references(),
        )
        .unwrap();
        assert_eq!(
            strict,
            vec![(proof.declared_target.clone(), CompletionInputPath::Strict)]
        );
        let head_refs = completion_references(
            &head,
            &CompletionInputPath::Head("head".into()),
            Some(&head),
            &[],
            head.references(),
        )
        .unwrap();
        assert!(head_refs.contains(&(
            InputEvidenceKey::new(InputEvidenceFamily::Message, "unrelated-brief-message"),
            CompletionInputPath::Strict
        )));
        assert!(head_refs.contains(&(
            source.locator.key.clone(),
            CompletionInputPath::Transcript("head".into())
        )));
    }
}

use std::sync::Arc;

use anyhow::{ensure, Result};
use serde::{Serialize, Serializer};

use super::{turns::HistoryTurnMaterial, PreparedInputEvidence, QualifiedInputEvidence};
use crate::{diagnostics::resource_io, types::MessageBody};

/// A single text field retains its qualified source instead of copying its body.
#[derive(Debug, Clone)]
pub(crate) struct ContextHistoryBriefText(HistoryBriefText);

#[derive(Debug, Clone)]
enum HistoryBriefText {
    Qualified {
        evidence: Arc<QualifiedInputEvidence>,
        _material: Arc<HistoryTurnMaterial>,
    },
    Owned(String),
}

pub(super) fn borrowed_text(record: &PreparedInputEvidence) -> Option<&str> {
    match record {
        PreparedInputEvidence::Brief(brief) => Some(&brief.text),
        PreparedInputEvidence::Message(message) => match &message.body {
            MessageBody::Text { text } | MessageBody::Brief { text, .. }
                if !text.trim().is_empty() =>
            {
                Some(text)
            }
            _ => None,
        },
        PreparedInputEvidence::Transcript(entry) => {
            crate::object_resolver::transcript_text_projection(entry)?.borrowed_text()
        }
        _ => None,
    }
}

impl ContextHistoryBriefText {
    pub(super) fn qualified(
        evidence: Arc<QualifiedInputEvidence>,
        material: Arc<HistoryTurnMaterial>,
    ) -> Result<Self> {
        resource_io::check_active()?;
        ensure!(
            matches!(
                evidence.record,
                PreparedInputEvidence::Brief(_)
                    | PreparedInputEvidence::Message(_)
                    | PreparedInputEvidence::Transcript(_)
            ),
            "resident evidence family mismatch"
        );
        ensure!(
            borrowed_text(&evidence.record).is_some(),
            "resident history source requires owning text resolution"
        );
        resource_io::charge_owned_metadata(std::mem::size_of::<Self>() as u64, 1)?;
        Ok(Self(HistoryBriefText::Qualified {
            evidence,
            _material: material,
        }))
    }

    /// The owning resolver has already admitted this string.
    pub(crate) fn owned(text: String) -> Self {
        Self(HistoryBriefText::Owned(text))
    }

    pub(crate) fn as_str(&self) -> &str {
        match &self.0 {
            HistoryBriefText::Qualified { evidence, .. } => {
                borrowed_text(&evidence.record).expect("qualified single text field")
            }
            HistoryBriefText::Owned(text) => text,
        }
    }
}

impl Serialize for ContextHistoryBriefText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_str().serialize(serializer)
    }
}

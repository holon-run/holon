use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;

/// The only editable fields of a timer. Title is never used to build a wake.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimerContent {
    pub title: Option<String>,
    pub instruction: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct TimerContentWire<'a> {
    title: &'a Option<String>,
    instruction: &'a Option<String>,
    /// Legacy alias for `instruction`.
    summary: &'a Option<String>,
}

impl JsonSchema for TimerContent {
    fn schema_name() -> Cow<'static, str> {
        "TimerContent".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        TimerContentWire::json_schema(generator)
    }
}

impl Serialize for TimerContent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        TimerContentWire {
            title: &self.title,
            instruction: &self.instruction,
            summary: &self.instruction,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TimerContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            title: Option<String>,
            #[serde(default, deserialize_with = "present")]
            instruction: Option<Option<String>>,
            summary: Option<String>,
        }

        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            title: fields.title,
            instruction: fields.instruction.unwrap_or(fields.summary),
        })
    }
}

/// Omitted fields are unchanged; an explicit null clears the value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TimerContentPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub title: Option<Option<String>>,
    #[serde(
        default,
        alias = "summary",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub instruction: Option<Option<String>>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimerContentError {
    #[error("provide either timer instruction or its legacy summary alias, not both")]
    ConflictingInstruction,
    #[error("timer title must not be blank")]
    BlankTitle,
    #[error("timer instruction must not be blank")]
    BlankInstruction,
}

impl TimerContentPatch {
    pub fn apply(self, content: &mut TimerContent) -> Result<(), TimerContentError> {
        // Validate before mutating either field; do not revalidate untouched legacy text.
        if self.title.as_ref().is_some_and(is_blank) {
            return Err(TimerContentError::BlankTitle);
        }
        if self.instruction.as_ref().is_some_and(is_blank) {
            return Err(TimerContentError::BlankInstruction);
        }
        if let Some(title) = self.title {
            content.title = title;
        }
        if let Some(instruction) = self.instruction {
            content.instruction = instruction;
        }
        Ok(())
    }
}

impl TimerContent {
    pub fn for_create(
        title: Option<String>,
        instruction: Option<String>,
        summary: Option<String>,
    ) -> Result<Self, TimerContentError> {
        if instruction.is_some() && summary.is_some() {
            return Err(TimerContentError::ConflictingInstruction);
        }
        let mut content = Self::from_legacy_summary(summary);
        TimerContentPatch {
            title: title.map(Some),
            instruction: instruction.map(Some),
        }
        .apply(&mut content)?;
        Ok(content)
    }

    /// Legacy creation accepts the original `summary` unchanged.
    pub fn from_legacy_summary(summary: Option<String>) -> Self {
        Self {
            title: None,
            instruction: summary,
        }
    }
}

fn is_blank(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|text| text.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TimerRecord;
    use serde_json::{from_value, json, to_value};

    #[test]
    fn create_accepts_legacy_or_new_instruction_without_requiring_either() {
        assert_eq!(
            TimerContent::for_create(None, None, None).unwrap(),
            TimerContent::default()
        );
        let legacy = TimerContent::for_create(None, None, Some(" ".into())).unwrap();
        assert_eq!(legacy.instruction.as_deref(), Some(" "));
        let named =
            TimerContent::for_create(Some("Report".into()), Some("Draft".into()), None).unwrap();
        assert_eq!(named.title.as_deref(), Some("Report"));
        assert_eq!(named.instruction.as_deref(), Some("Draft"));
        assert_eq!(
            TimerContent::for_create(None, Some("Draft".into()), Some("Legacy".into())),
            Err(TimerContentError::ConflictingInstruction)
        );
        assert_eq!(
            TimerContent::for_create(None, Some(" ".into()), None),
            Err(TimerContentError::BlankInstruction)
        );
    }

    #[test]
    fn legacy_timer_payload_reads_and_round_trips_with_flat_content() {
        let mut timer: TimerRecord = from_value(json!({
            "id": "timer-1",
            "agent_id": "agent",
            "created_at": "2026-01-01T00:00:00Z",
            "duration_ms": 1000,
            "repeat": false,
            "summary": "legacy instructions"
        }))
        .unwrap();
        timer.content.title = Some("display only".into());
        assert_eq!(timer.trigger_text(), "legacy instructions");
        let wire = to_value(&timer).unwrap();
        assert_eq!(wire["title"], "display only");
        assert_eq!(wire["instruction"], wire["summary"]);
        assert_eq!(from_value::<TimerRecord>(wire).unwrap(), timer);
        let schema = serde_json::to_value(schemars::schema_for!(TimerRecord)).unwrap();
        for field in ["title", "instruction", "summary"] {
            assert!(schema["properties"].get(field).is_some(), "{field}");
        }
        timer.content.instruction = None;
        assert_eq!(timer.trigger_text(), "timer timer-1 fired");
    }

    #[test]
    fn legacy_summary_decodes_as_instruction_and_both_names_encode_one_value() {
        let content: TimerContent = from_value(json!({"summary": "do work"})).unwrap();
        assert_eq!(content.title, None);
        assert_eq!(content.instruction.as_deref(), Some("do work"));
        assert_eq!(
            to_value(content).unwrap(),
            json!({"title": null, "instruction": "do work", "summary": "do work"})
        );
        let cleared: TimerContent =
            from_value(json!({"instruction": null, "summary": "old text"})).unwrap();
        assert_eq!(cleared.instruction, None);
        assert_eq!(
            from_value::<TimerContent>(json!({})).unwrap(),
            TimerContent::default()
        );
    }

    #[test]
    fn patch_distinguishes_omitted_null_and_legacy_summary() {
        let mut content = TimerContent::default();
        let patch: TimerContentPatch =
            from_value(json!({"title": "display", "summary": " do work\n"})).unwrap();
        patch.apply(&mut content).unwrap();
        assert_eq!(content.title.as_deref(), Some("display"));
        assert_eq!(content.instruction.as_deref(), Some(" do work\n"));
        let patch: TimerContentPatch = from_value(json!({"title": null})).unwrap();
        assert_eq!(to_value(&patch).unwrap(), json!({"title": null}));
        patch.apply(&mut content).unwrap();
        assert_eq!(content.title, None);
        assert_eq!(content.instruction.as_deref(), Some(" do work\n"));
        from_value::<TimerContentPatch>(json!({"summary": null}))
            .unwrap()
            .apply(&mut content)
            .unwrap();
        assert_eq!(content, TimerContent::default());
    }

    #[test]
    fn patch_rejects_other_fields_dual_names_and_blank_values_atomically() {
        assert!(from_value::<TimerContentPatch>(json!({"duration_ms": 10})).is_err());
        assert!(from_value::<TimerContentPatch>(
            json!({"summary": "legacy", "instruction": "canonical"})
        )
        .is_err());
        let mut content = TimerContent::default();
        let patch: TimerContentPatch =
            from_value(json!({"title": "display", "instruction": " \n"})).unwrap();
        assert_eq!(
            patch.apply(&mut content),
            Err(TimerContentError::BlankInstruction)
        );
        assert_eq!(content, TimerContent::default());
        let patch: TimerContentPatch = from_value(json!({"title": "  "})).unwrap();
        assert_eq!(
            patch.apply(&mut content),
            Err(TimerContentError::BlankTitle)
        );
    }
}

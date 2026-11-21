//! Admit and clone the selected coverage, without a discarded full-domain copy.
use super::*;
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};

pub(super) struct CaptureCoverage<'a> {
    pub(super) coverage: &'a SchedulingCoverage,
    pub(super) request: &'a SchedulingCaptureRequest,
    pub(super) selected: &'a BTreeSet<SchedulingKey>,
}

impl Serialize for CaptureCoverage<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let c = self.coverage;
        let live = self.request.include_live;
        let empty = BTreeSet::new();
        let mut fields = serializer.serialize_struct("SchedulingCoverage", 10)?;
        fields.serialize_field("revision", &c.revision)?;
        fields.serialize_field(
            "active_timers_complete",
            &(live && c.active_timers_complete),
        )?;
        fields.serialize_field(
            "pending_wakes_complete",
            &(live && c.pending_wakes_complete),
        )?;
        fields.serialize_field(
            "active_triggers_complete",
            &(live && c.active_triggers_complete),
        )?;
        fields.serialize_field(
            "active_timers",
            if live { &c.active_timers } else { &empty },
        )?;
        fields.serialize_field(
            "pending_wakes",
            if live { &c.pending_wakes } else { &empty },
        )?;
        fields.serialize_field(
            "active_triggers",
            if live { &c.active_triggers } else { &empty },
        )?;
        fields.serialize_field(
            "history",
            &HistoryView(&c.history, self.request.history_limit),
        )?;
        fields.serialize_field("required_keys", &self.request.required_keys)?;
        fields.serialize_field(
            "observations",
            &ObservationView(&c.observations, self.selected),
        )?;
        fields.end()
    }
}

struct HistoryView<'a>(&'a BTreeMap<SchedulingFamily, SchedulingHistory>, usize);
impl Serialize for HistoryView<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct History<'a> {
            keys: &'a [SchedulingKey],
            loaded_keys: &'a [SchedulingKey],
            is_exhaustive: bool,
        }
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (family, history) in self.0 {
            map.serialize_entry(
                family,
                &History {
                    keys: &history.keys[..history.keys.len().min(self.1)],
                    loaded_keys: &history.loaded_keys[..history.loaded_keys.len().min(self.1)],
                    is_exhaustive: history.is_exhaustive && history.keys.len() <= self.1,
                },
            )?;
        }
        map.end()
    }
}

struct ObservationView<'a>(&'a [SchedulingObservation], &'a BTreeSet<SchedulingKey>);
impl Serialize for ObservationView<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        for observation in self
            .0
            .iter()
            .filter(|observation| self.1.contains(&observation.key))
        {
            seq.serialize_element(observation)?;
        }
        seq.end()
    }
}

pub(super) fn clone_selected_coverage(
    coverage: &SchedulingCoverage,
    request: &SchedulingCaptureRequest,
    selected: &BTreeSet<SchedulingKey>,
) -> Result<SchedulingCoverage> {
    crate::diagnostics::resource_io::admit_typed_cloned_value(&CaptureCoverage {
        coverage,
        request,
        selected,
    })?;
    Ok(SchedulingCoverage {
        revision: coverage.revision,
        active_timers_complete: request.include_live && coverage.active_timers_complete,
        pending_wakes_complete: request.include_live && coverage.pending_wakes_complete,
        active_triggers_complete: request.include_live && coverage.active_triggers_complete,
        active_timers: if request.include_live {
            coverage.active_timers.clone()
        } else {
            BTreeSet::new()
        },
        pending_wakes: if request.include_live {
            coverage.pending_wakes.clone()
        } else {
            BTreeSet::new()
        },
        active_triggers: if request.include_live {
            coverage.active_triggers.clone()
        } else {
            BTreeSet::new()
        },
        history: coverage
            .history
            .iter()
            .map(|(family, history)| {
                (
                    *family,
                    SchedulingHistory {
                        keys: history.keys[..history.keys.len().min(request.history_limit)]
                            .to_vec(),
                        loaded_keys: history.loaded_keys
                            [..history.loaded_keys.len().min(request.history_limit)]
                            .to_vec(),
                        is_exhaustive: history.is_exhaustive
                            && history.keys.len() <= request.history_limit,
                    },
                )
            })
            .collect(),
        required_keys: request.required_keys.clone(),
        observations: coverage
            .observations
            .iter()
            .filter(|observation| selected.contains(&observation.key))
            .cloned()
            .collect(),
    })
}

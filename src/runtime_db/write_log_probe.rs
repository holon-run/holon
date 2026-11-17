//! Test-only tracing capture for runtime db write outcome events, used by the
//! #4232 reader-reprepare logging regression tests.

use std::sync::{Arc, Mutex};
use tracing_subscriber::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteLogEvent {
    pub(crate) level: tracing::Level,
    pub(crate) message: String,
    pub(crate) reader_reprepare: bool,
}

#[derive(Default, Clone)]
pub(crate) struct WriteLogProbe {
    events: Arc<Mutex<Vec<WriteLogEvent>>>,
}

#[derive(Default)]
struct RecordedFields {
    message: Option<String>,
    reader_reprepare: bool,
}

impl tracing::field::Visit for RecordedFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() == "reader_reprepare" {
            self.reader_reprepare = value;
        }
    }
}

impl<S> tracing_subscriber::layer::Layer<S> for WriteLogProbe
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = event.metadata().level();
        if !matches!(level, &tracing::Level::DEBUG | &tracing::Level::WARN) {
            return;
        }
        let target = event.metadata().target();
        // Storage transaction classification events, plus the terminal
        // reprepare alerts from the layer owning the bounded attempts.
        let storage_connection = matches!(
            target,
            "holon::runtime_db::connection" | "holon_storage::connection"
        );
        let reprepare_budget_layer = target == "holon::runtime_db::transitions::prepared_input";
        if !storage_connection && !(reprepare_budget_layer && level == &tracing::Level::WARN) {
            return;
        }
        let mut fields = RecordedFields::default();
        event.record(&mut fields);
        self.events.lock().unwrap().push(WriteLogEvent {
            level: level.clone(),
            message: fields.message.unwrap_or_default(),
            reader_reprepare: fields.reader_reprepare,
        });
    }
}

impl WriteLogProbe {
    /// Runs `run` with this probe installed as the thread-local default
    /// subscriber. Runtime db write jobs execute inline on the calling
    /// thread, so their events are captured synchronously.
    pub(crate) fn capture<T>(&self, run: impl FnOnce() -> T) -> T {
        tracing::subscriber::with_default(tracing_subscriber::registry().with(self.clone()), run)
    }

    pub(crate) fn events(&self) -> Vec<WriteLogEvent> {
        self.events.lock().unwrap().clone()
    }

    /// WARN events presenting a write as failed ("runtime db write callback
    /// failed").
    pub(crate) fn write_failure_warns(&self) -> usize {
        self.events()
            .iter()
            .filter(|event| {
                matches!(event.level, tracing::Level::WARN)
                    && event.message.contains("write callback failed")
            })
            .count()
    }

    /// Non-failure debug events for intermediate reader reprepare attempts.
    pub(crate) fn deferred_reprepare_events(&self) -> usize {
        self.events()
            .iter()
            .filter(|event| {
                matches!(event.level, tracing::Level::DEBUG)
                    && event.reader_reprepare
                    && event.message.contains("deferred to reader reprepare")
            })
            .count()
    }

    /// Terminal alerts from the layer owning the reprepare budget.
    pub(crate) fn terminal_alerts(&self, needle: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| {
                matches!(event.level, tracing::Level::WARN) && event.message.contains(needle)
            })
            .count()
    }
}

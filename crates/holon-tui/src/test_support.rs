//! Exercises the terminal projection from the runtime's pipeline tests.
use std::path::Path;

use anyhow::Result;
use holon_client::{AgentStateSnapshot, AgentStreamEvent};

use crate::tui::{logging::TuiLogWriter, projection::TuiProjection};

pub struct PresentationLog {
    writer: TuiLogWriter,
}

impl PresentationLog {
    pub fn new(log_dir: &Path, max_bytes: u64) -> Result<Self> {
        Ok(Self {
            writer: TuiLogWriter::new(log_dir)?.with_presentation_logging(max_bytes),
        })
    }

    pub fn root(&self) -> &Path {
        self.writer.root()
    }

    pub fn project(
        &self,
        snapshot: AgentStateSnapshot,
        events: impl IntoIterator<Item = AgentStreamEvent>,
    ) {
        let mut projection = TuiProjection::from_snapshot(snapshot);
        for event in events {
            projection.apply_event(event, &self.writer);
        }
    }
}

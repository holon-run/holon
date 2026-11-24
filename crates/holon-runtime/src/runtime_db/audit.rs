use anyhow::Result;

pub use holon_storage::audit::{
    AgentBriefIntegrityAudit, BriefIntegrityAudit, BriefIntegrityCategory, BriefIntegrityCounts,
    ProjectionEffectInventoryGroup, ProjectionEffectsAudit, RuntimeDbAuditBaseline,
    RuntimeDbAuditCheck, RuntimeDbAuditDatabase, RuntimeDbAuditOptions, RuntimeDbAuditReport,
    ViolationSplit,
};

use super::{migrations::current_schema_version, RuntimeDb};

impl RuntimeDb {
    pub fn audit(&self, options: RuntimeDbAuditOptions) -> Result<RuntimeDbAuditReport> {
        let connection = self.connection()?;
        holon_storage::audit::audit(&connection, options, |connection| {
            Ok(RuntimeDbAuditDatabase {
                path: self.path().display().to_string(),
                schema_version: current_schema_version(connection)?,
                runtime_id: self.runtime_id()?,
                event_log_epoch: self.event_log_epoch()?,
            })
        })
    }
}

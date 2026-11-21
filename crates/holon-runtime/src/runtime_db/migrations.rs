//! Runtime qualification hooks for SQLite schema upgrades.
use anyhow::Result;
use chrono::Utc;
use holon_storage::migrations::{self, RuntimeBackfill};
use rusqlite::{Connection, Transaction};

pub use migrations::Migration;
pub(crate) use migrations::*;

#[cfg(test)]
pub(crate) fn apply_migration(connection: &mut Connection, migration: &Migration) -> Result<()> {
    migrations::apply_migration(connection, migration, apply_runtime_backfill)
}

pub(super) fn migrate(connection: &mut Connection) -> Result<()> {
    migrations::migrate(connection, apply_runtime_backfill)
}

fn apply_runtime_backfill(transaction: &Transaction<'_>, step: RuntimeBackfill) -> Result<()> {
    match step {
        RuntimeBackfill::EpisodeContinuity => {
            super::episode_snapshot::capture_episode_continuity_upgrade_tx(
                transaction,
                Utc::now(),
            )?;
        }
        RuntimeBackfill::InputEvidence => {
            super::input_evidence_snapshot::capture_input_evidence_upgrade_tx(
                transaction,
                Utc::now(),
            )?;
        }
        RuntimeBackfill::WorkDomain => {
            super::work_snapshot::capture_work_domain_upgrade_tx(transaction)?;
        }
        RuntimeBackfill::TaskDomain => {
            super::task_snapshot::capture_task_domain_upgrade_tx(transaction)?;
            super::task_snapshot::capture_status_sources_upgrade_tx(transaction)?;
        }
        RuntimeBackfill::Scheduling => {
            super::scheduling_snapshot::consume_scheduling_upgrade_tx(transaction)?;
        }
        RuntimeBackfill::CurrentRead => {
            super::episode_snapshot::current_upgrade::capture_tx(transaction)?;
        }
        RuntimeBackfill::DispatchMemory => {
            super::dispatch_memory::capture_legacy_dispatches_at_upgrade_tx(transaction)?;
        }
    }
    Ok(())
}

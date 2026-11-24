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

/// Seals current dispatches that the fixed v99 upgrade could not seal because a
/// clean shutdown had already cleared the State run (#4445). Runs after schema
/// migration and before agent recovery on every open; the routine is
/// idempotent and proves each address from the migrated database alone.
pub(super) fn backfill_legacy_dispatch_seals(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction()?;
    let inserted = super::dispatch_memory::backfill_legacy_dispatch_seals_tx(&transaction)?;
    transaction.commit()?;
    if inserted > 0 {
        tracing::info!(
            inserted,
            "sealed legacy current dispatches missed by the fixed v99 upgrade"
        );
    }
    Ok(())
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

//! Storage domain checkpoint and cutover helpers.

use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::runtime_db::migrations::{max_known_migration_version, timestamp};

/// Snapshot of a storage domain at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageDomainSnapshot {
    pub domain: String,
    pub schema_version: i64,
    pub import_status: String,
    pub canonical_source: String,
    pub source_checkpoint_json: Option<String>,
    pub imported_at: Option<String>,
    pub updated_at: String,
}

/// Legacy JSONL export posture for a storage domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyJsonlPosture {
    Disabled,
    DebugExportOnly,
    AuditMirror,
    LegacyImportOnly,
}

impl LegacyJsonlPosture {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::DebugExportOnly => "debug_export_only",
            Self::AuditMirror => "audit_mirror",
            Self::LegacyImportOnly => "legacy_import_only",
        }
    }
}

/// Expected storage domain configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedStorageDomain {
    pub domain: &'static str,
    pub canonical_source: &'static str,
    pub legacy_jsonl_posture: LegacyJsonlPosture,
}

pub(crate) fn read_storage_domain_connection(
    connection: &Connection,
    domain: &str,
) -> Result<Option<StorageDomainSnapshot>> {
    connection
        .query_row(
            "SELECT domain, schema_version, import_status, canonical_source,
                    source_checkpoint_json, imported_at, updated_at
             FROM storage_domains WHERE domain = ?1",
            [domain],
            |row| {
                Ok(StorageDomainSnapshot {
                    domain: row.get(0)?,
                    schema_version: row.get(1)?,
                    import_status: row.get(2)?,
                    canonical_source: row.get(3)?,
                    source_checkpoint_json: row.get(4)?,
                    imported_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn upsert_storage_domain(
    tx: &Transaction<'_>,
    domain: &str,
    import_status: &str,
    canonical_source: &str,
    checkpoint: Option<serde_json::Value>,
) -> Result<()> {
    upsert_storage_domain_checkpoint_json(
        tx,
        domain,
        import_status,
        canonical_source,
        checkpoint.map(|value| value.to_string()),
    )
}

pub(crate) fn upsert_storage_domain_checkpoint_json(
    tx: &Transaction<'_>,
    domain: &str,
    import_status: &str,
    canonical_source: &str,
    checkpoint_json: Option<String>,
) -> Result<()> {
    let now = timestamp(Utc::now());
    let imported_at = (import_status == "complete").then(|| now.clone());
    tx.execute(
        "INSERT INTO storage_domains (
            domain, schema_version, import_status, canonical_source,
            source_checkpoint_json, imported_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(domain) DO UPDATE SET
            schema_version = excluded.schema_version,
            import_status = excluded.import_status,
            canonical_source = excluded.canonical_source,
            source_checkpoint_json = excluded.source_checkpoint_json,
            imported_at = excluded.imported_at,
            updated_at = excluded.updated_at",
        params![
            domain,
            max_known_migration_version(),
            import_status,
            canonical_source,
            checkpoint_json,
            imported_at,
            now
        ],
    )?;
    Ok(())
}

use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
};

use crate::{
    agent_package_io::RuntimePackageIo,
    config::{
        credential_store_path, load_credential_store_at, AgentTemplateRemoteSourceConfigFile,
        AgentTemplatesConfigFile, CredentialKind,
    },
    runtime_db::RuntimeDb,
    types::{AgentTemplateCatalogEntry, AgentTemplateDetail},
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use holon_agent_templates::templates::{
    self as package, BuiltinSkill, BuiltinSkillFile, BuiltinTemplate, TemplateEnvironment,
};
use holon_github::archive::DEFAULT_ARCHIVE_BASE;
use rusqlite::{params, OptionalExtension};

pub use package::{
    agent_memory_operator_path, agent_memory_self_path, ensure_agent_home_layout,
    materialize_local_skill_ref, recover_template_staging, remove_materialized_skill_destination,
    remove_user_template, seed_builtin_templates, seed_builtin_templates_for_home,
    template_provenance_path, template_staging_path, user_home_dir, validate_github_template_url,
    AgentTemplateCatalogDiagnostic, AgentTemplateRemoteSourceStatus,
    AgentTemplateRemoteSourceSyncStatus, TemplateProvenanceRecord, TemplateProvenanceSource,
    DEFAULT_AGENT_TEMPLATE_ID, GITHUB_SOLVE_AGENT_TEMPLATE_ID,
    OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_ID, OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_URL,
    REQUIRED_AGENT_HOME_GUIDANCE,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTemplateRemoteCatalogSnapshot {
    pub catalog: Vec<AgentTemplateCatalogEntry>,
    pub sources: Vec<AgentTemplateRemoteSourceStatus>,
    pub diagnostics: Vec<AgentTemplateCatalogDiagnostic>,
}

const DEFAULT_BUILTIN_TEMPLATE: BuiltinTemplate = BuiltinTemplate {
    template_id: "holon-default",
    agents_md: include_str!("../builtin_templates/holon-default/AGENTS.md"),
    template_toml: include_str!("../builtin_templates/holon-default/template.toml"),
    skill_names: &[],
};
const BUILTIN_TEMPLATES: &[BuiltinTemplate] = &[DEFAULT_BUILTIN_TEMPLATE];

const BUILTIN_SKILLS: &[BuiltinSkill] = &[
    BuiltinSkill {
        name: "ghx",
        skill_md: include_str!("../skills/ghx/SKILL.md"),
        files: &[],
    },
    BuiltinSkill {
        name: "github-issue-solve",
        skill_md: include_str!("../skills/github-issue-solve/SKILL.md"),
        files: &[BuiltinSkillFile {
            path: "references/issue-solve-workflow.md",
            content: include_str!(
                "../skills/github-issue-solve/references/issue-solve-workflow.md"
            ),
        }],
    },
    BuiltinSkill {
        name: "github-pr-fix",
        skill_md: include_str!("../skills/github-pr-fix/SKILL.md"),
        files: &[
            BuiltinSkillFile {
                path: "references/diagnostics.md",
                content: include_str!("../skills/github-pr-fix/references/diagnostics.md"),
            },
            BuiltinSkillFile {
                path: "references/pr-fix-workflow.md",
                content: include_str!("../skills/github-pr-fix/references/pr-fix-workflow.md"),
            },
        ],
    },
    BuiltinSkill {
        name: "github-review",
        skill_md: include_str!("../skills/github-review/SKILL.md"),
        files: &[],
    },
];

const TEMPLATES: TemplateEnvironment = TemplateEnvironment::new(
    &RuntimePackageIo,
    BUILTIN_TEMPLATES,
    BUILTIN_SKILLS,
    github_template_archive_base,
);

fn github_template_archive_base() -> String {
    if cfg!(test) {
        env::var("HOLON_TEMPLATE_GITHUB_API_BASE")
            .unwrap_or_else(|_| DEFAULT_ARCHIVE_BASE.to_string())
    } else {
        DEFAULT_ARCHIVE_BASE.to_string()
    }
}

pub fn effective_agent_template_remote_sources(
    config: &AgentTemplatesConfigFile,
) -> BTreeMap<String, AgentTemplateRemoteSourceConfigFile> {
    let mut sources = config.remote_sources.clone();
    sources
        .entry(OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_ID.to_string())
        .or_insert_with(|| AgentTemplateRemoteSourceConfigFile {
            url: OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_URL.to_string(),
            git_ref: None,
            enabled: Some(true),
            credential_profile: None,
        });
    sources
}

pub fn load_remote_template_catalog_snapshot(
    db: &RuntimeDb,
    configured_sources: &BTreeMap<String, AgentTemplateRemoteSourceConfigFile>,
) -> Result<AgentTemplateRemoteCatalogSnapshot> {
    let rows = db.with_read(|connection| {
        configured_sources
            .keys()
            .map(|source_id| load_remote_source_stored_row(connection, source_id))
            .collect::<Result<Vec<_>>>()
    })?;
    let catalog = Vec::new();
    let mut sources = Vec::new();
    let mut diagnostics = Vec::new();

    for ((source_id, config), row) in configured_sources.iter().zip(rows) {
        let enabled = config.enabled();
        let Some(row) = row.map(RemoteSourceStoredRow::decode).transpose()? else {
            let status = if enabled {
                AgentTemplateRemoteSourceSyncStatus::NotSynced
            } else {
                AgentTemplateRemoteSourceSyncStatus::Disabled
            };
            sources.push(AgentTemplateRemoteSourceStatus {
                source_id: source_id.clone(),
                kind: "github".into(),
                url: config.url.clone(),
                requested_ref: config.git_ref.clone(),
                enabled,
                status,
                last_synced_at: None,
                resolved_ref: None,
                resolved_revision: None,
                error: None,
            });
            continue;
        };
        diagnostics.extend(row.diagnostics.iter().cloned());
        let mut status = row.status;
        if !enabled {
            status = AgentTemplateRemoteSourceSyncStatus::Disabled;
        }
        sources.push(AgentTemplateRemoteSourceStatus {
            source_id: source_id.clone(),
            kind: row.kind,
            url: config.url.clone(),
            requested_ref: config.git_ref.clone(),
            enabled,
            status,
            last_synced_at: row.last_synced_at,
            resolved_ref: row.resolved_ref,
            resolved_revision: row.resolved_revision,
            error: row.error,
        });
    }

    Ok(AgentTemplateRemoteCatalogSnapshot {
        catalog,
        sources,
        diagnostics,
    })
}

pub fn agent_template_remote_source_needs_sync(
    db: &RuntimeDb,
    source_id: &str,
    stale_after: chrono::Duration,
) -> Result<bool> {
    let Some(row) = load_remote_source_row(db, source_id)? else {
        return Ok(true);
    };
    if row.status != AgentTemplateRemoteSourceSyncStatus::Synced {
        if row
            .next_retry_at
            .is_some_and(|next_retry_at| next_retry_at > Utc::now())
        {
            return Ok(false);
        }
        return Ok(true);
    }
    let Some(last_synced_at) = row.last_synced_at else {
        return Ok(row
            .next_retry_at
            .is_none_or(|next_retry_at| next_retry_at <= Utc::now()));
    };
    if row
        .next_retry_at
        .is_some_and(|next_retry_at| next_retry_at > Utc::now())
    {
        return Ok(false);
    }
    Ok(Utc::now().signed_duration_since(last_synced_at) >= stale_after)
}

#[derive(Debug)]
struct RemoteSourceDbRow {
    kind: String,
    status: AgentTemplateRemoteSourceSyncStatus,
    last_synced_at: Option<DateTime<Utc>>,
    resolved_ref: Option<String>,
    resolved_revision: Option<String>,
    catalog: Vec<AgentTemplateCatalogEntry>,
    diagnostics: Vec<AgentTemplateCatalogDiagnostic>,
    error: Option<String>,
    next_retry_at: Option<DateTime<Utc>>,
}

fn load_remote_source_row(db: &RuntimeDb, source_id: &str) -> Result<Option<RemoteSourceDbRow>> {
    db.with_read(|connection| load_remote_source_stored_row(connection, source_id))?
        .map(RemoteSourceStoredRow::decode)
        .transpose()
}

struct RemoteSourceStoredRow {
    kind: String,
    status: String,
    last_synced_at: Option<String>,
    resolved_ref: Option<String>,
    resolved_revision: Option<String>,
    catalog_json: String,
    diagnostics_json: String,
    error: Option<String>,
    next_retry_at: Option<String>,
}

impl RemoteSourceStoredRow {
    fn decode(self) -> Result<RemoteSourceDbRow> {
        Ok(RemoteSourceDbRow {
            kind: self.kind,
            status: parse_remote_source_sync_status(&self.status),
            last_synced_at: self
                .last_synced_at
                .map(|value| value.parse())
                .transpose()
                .context("failed to parse remote source last_synced_at")?,
            resolved_ref: self.resolved_ref,
            resolved_revision: self.resolved_revision,
            catalog: serde_json::from_str(&self.catalog_json)
                .context("failed to decode remote template catalog_json")?,
            diagnostics: serde_json::from_str(&self.diagnostics_json)
                .context("failed to decode remote template diagnostics_json")?,
            error: self.error,
            next_retry_at: self
                .next_retry_at
                .map(|value| value.parse())
                .transpose()
                .context("failed to parse remote source next_retry_at")?,
        })
    }
}

fn load_remote_source_stored_row(
    connection: &rusqlite::Connection,
    source_id: &str,
) -> Result<Option<RemoteSourceStoredRow>> {
    Ok(connection
        .query_row(
            "SELECT kind, status, last_synced_at, resolved_ref, resolved_revision, catalog_json, diagnostics_json, error, next_retry_at \
             FROM agent_template_remote_source_syncs WHERE source_id = ?1",
            [source_id],
            |row| {
                let status: String = row.get(1)?;
                let last_synced_at: Option<String> = row.get(2)?;
                let catalog_json: String = row.get(5)?;
                let diagnostics_json: String = row.get(6)?;
                Ok(RemoteSourceStoredRow {
                    kind: row.get::<_, String>(0)?,
                    status,
                    last_synced_at,
                    resolved_ref: row.get::<_, Option<String>>(3)?,
                    resolved_revision: row.get::<_, Option<String>>(4)?,
                    catalog_json,
                    diagnostics_json,
                    error: row.get::<_, Option<String>>(7)?,
                    next_retry_at: row.get::<_, Option<String>>(8)?,
                })
            },
        )
        .optional()?)
}

fn parse_remote_source_sync_status(value: &str) -> AgentTemplateRemoteSourceSyncStatus {
    match value {
        "synced" => AgentTemplateRemoteSourceSyncStatus::Synced,
        "partially_failed" => AgentTemplateRemoteSourceSyncStatus::PartiallyFailed,
        "failed" => AgentTemplateRemoteSourceSyncStatus::Failed,
        "disabled" => AgentTemplateRemoteSourceSyncStatus::Disabled,
        _ => AgentTemplateRemoteSourceSyncStatus::NotSynced,
    }
}

fn remote_source_sync_status_label(status: &AgentTemplateRemoteSourceSyncStatus) -> &'static str {
    match status {
        AgentTemplateRemoteSourceSyncStatus::NotSynced => "not_synced",
        AgentTemplateRemoteSourceSyncStatus::Synced => "synced",
        AgentTemplateRemoteSourceSyncStatus::PartiallyFailed => "partially_failed",
        AgentTemplateRemoteSourceSyncStatus::Failed => "failed",
        AgentTemplateRemoteSourceSyncStatus::Disabled => "disabled",
    }
}

pub async fn sync_agent_template_remote_source(
    db: &RuntimeDb,
    user_home: &Path,
    source_id: &str,
    config: &AgentTemplateRemoteSourceConfigFile,
) -> Result<AgentTemplateRemoteSourceStatus> {
    if !config.enabled() {
        let status = AgentTemplateRemoteSourceStatus {
            source_id: source_id.to_string(),
            kind: "github".into(),
            url: config.url.clone(),
            requested_ref: config.git_ref.clone(),
            enabled: false,
            status: AgentTemplateRemoteSourceSyncStatus::Disabled,
            last_synced_at: None,
            resolved_ref: None,
            resolved_revision: None,
            error: None,
        };
        upsert_remote_source_sync(
            db,
            &status,
            &[],
            &[AgentTemplateCatalogDiagnostic {
                source_id: source_id.to_string(),
                severity: "info".into(),
                message: "remote source is disabled".into(),
            }],
        )?;
        return Ok(status);
    }

    let parsed = package::parse_github_repo_url(&config.url)?;
    let token = config
        .credential_profile
        .as_deref()
        .map(|profile| load_github_template_credential_profile_token(user_home, profile))
        .transpose()?;
    let sync = package::install_remote_template_source(
        TEMPLATES,
        user_home,
        source_id,
        &config.url,
        parsed,
        config.git_ref.as_deref(),
        token,
    )
    .await?;
    upsert_remote_source_sync(db, &sync.status, &sync.catalog, &sync.diagnostics)?;
    Ok(sync.status)
}

pub fn record_agent_template_remote_source_sync_failure(
    db: &RuntimeDb,
    source_id: &str,
    config: &AgentTemplateRemoteSourceConfigFile,
    error: &str,
) -> Result<AgentTemplateRemoteSourceStatus> {
    let previous_catalog = load_remote_source_row(db, source_id)?
        .map(|row| row.catalog)
        .unwrap_or_default();
    let diagnostic = AgentTemplateCatalogDiagnostic {
        source_id: source_id.to_string(),
        severity: "error".into(),
        message: error.to_string(),
    };
    let status = AgentTemplateRemoteSourceStatus {
        source_id: source_id.to_string(),
        kind: "github".into(),
        url: config.url.clone(),
        requested_ref: config.git_ref.clone(),
        enabled: config.enabled(),
        status: AgentTemplateRemoteSourceSyncStatus::Failed,
        last_synced_at: Some(Utc::now()),
        resolved_ref: None,
        resolved_revision: None,
        error: Some(error.to_string()),
    };
    upsert_remote_source_sync(db, &status, &previous_catalog, &[diagnostic])?;
    Ok(status)
}

fn upsert_remote_source_sync(
    db: &RuntimeDb,
    status: &AgentTemplateRemoteSourceStatus,
    catalog: &[AgentTemplateCatalogEntry],
    diagnostics: &[AgentTemplateCatalogDiagnostic],
) -> Result<()> {
    let status_label = remote_source_sync_status_label(&status.status);
    let last_synced_at = status.last_synced_at.map(|t| t.to_rfc3339());
    let catalog_json = serde_json::to_string(catalog)?;
    let diagnostics_json = serde_json::to_string(diagnostics)?;
    let now = Utc::now().to_rfc3339();
    let created_at = now.clone();
    db.transaction(|tx| {
        let previous_retry_count: i64 = tx
            .query_row(
                "SELECT COALESCE(retry_count, 0) FROM agent_template_remote_source_syncs \
                 WHERE source_id = ?1",
                [status.source_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let retrying = matches!(
            status.status,
            AgentTemplateRemoteSourceSyncStatus::Failed
                | AgentTemplateRemoteSourceSyncStatus::PartiallyFailed
        );
        let retry_count = if retrying {
            previous_retry_count.saturating_add(1)
        } else {
            0
        };
        let retry_after = status
            .error
            .as_deref()
            .and_then(|error| error.split_once("retry after "))
            .and_then(|(_, value)| value.split_whitespace().next())
            .map(|value| {
                value.trim_end_matches(|character| matches!(character, ')' | '.' | ',' | ';'))
            })
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc));
        let next_retry_at = if retrying {
            let deadline = retry_after.unwrap_or_else(|| {
                let delay_seconds = 60_i64
                    .saturating_mul(1_i64 << (retry_count as u32).min(8))
                    .min(6 * 60 * 60);
                Utc::now() + chrono::Duration::seconds(delay_seconds)
            });
            Some(deadline.to_rfc3339())
        } else {
            None
        };
        tx.execute(
            "INSERT INTO agent_template_remote_source_syncs \
             (source_id, kind, url, requested_ref, enabled, status, last_synced_at, resolved_ref, resolved_revision, catalog_json, diagnostics_json, error, next_retry_at, retry_count, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
             ON CONFLICT(source_id) DO UPDATE SET \
               kind = excluded.kind, url = excluded.url, requested_ref = excluded.requested_ref, \
               enabled = excluded.enabled, status = excluded.status, last_synced_at = excluded.last_synced_at, \
               resolved_ref = excluded.resolved_ref, resolved_revision = excluded.resolved_revision, \
               catalog_json = excluded.catalog_json, diagnostics_json = excluded.diagnostics_json, \
               error = excluded.error, next_retry_at = excluded.next_retry_at, \
               retry_count = excluded.retry_count, updated_at = excluded.updated_at",
            params![
                status.source_id,
                status.kind,
                status.url,
                status.requested_ref,
                if status.enabled { 1 } else { 0 },
                status_label,
                last_synced_at,
                status.resolved_ref,
                status.resolved_revision,
                catalog_json,
                diagnostics_json,
                status.error,
                next_retry_at,
                retry_count,
                created_at,
                now,
            ],
        )?;
        Ok(())
    })
}

fn load_github_template_credential_profile_token(
    user_home: &Path,
    profile: &str,
) -> Result<String> {
    let store_path = credential_store_path(user_home);
    let store = load_credential_store_at(&store_path)?;
    let entry = store.profiles.get(profile).ok_or_else(|| {
        anyhow!("GitHub template credential profile '{profile}' is not configured")
    })?;
    if entry.kind != CredentialKind::BearerToken && entry.kind != CredentialKind::ApiKey {
        bail!("GitHub template credential profile '{profile}' must be api_key or bearer_token");
    }
    let token = entry.material.trim();
    if token.is_empty() {
        bail!("GitHub template credential profile '{profile}' is empty");
    }
    Ok(token.to_string())
}

pub async fn initialize_agent_home_from_template(
    agent_home: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    package::initialize_agent_home_from_template(TEMPLATES, agent_home, template).await
}

pub async fn initialize_agent_home_from_template_with_catalog(
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    package::initialize_agent_home_from_template_with_catalog(
        TEMPLATES,
        agent_home,
        home_dir,
        catalog_agent_home,
        template,
    )
    .await
}

pub async fn initialize_agent_home_without_template(
    agent_home: &Path,
) -> Result<TemplateProvenanceRecord> {
    package::initialize_agent_home_without_template(TEMPLATES, agent_home).await
}

pub async fn initialize_agent_home_without_template_with_home(
    agent_home: &Path,
    home_dir: &Path,
) -> Result<TemplateProvenanceRecord> {
    package::initialize_agent_home_without_template_with_home(TEMPLATES, agent_home, home_dir).await
}

pub async fn initialize_agent_home_from_template_with_home(
    agent_home: &Path,
    home_dir: &Path,
    template: &str,
) -> Result<TemplateProvenanceRecord> {
    package::initialize_agent_home_from_template_with_home(
        TEMPLATES, agent_home, home_dir, template,
    )
    .await
}

pub async fn ensure_agent_home_agents_md_from_template(
    agent_home: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    package::ensure_agent_home_agents_md_from_template(TEMPLATES, agent_home, template).await
}

pub async fn ensure_agent_home_agents_md_from_template_with_catalog(
    agent_home: &Path,
    home_dir: &Path,
    catalog_agent_home: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    package::ensure_agent_home_agents_md_from_template_with_catalog(
        TEMPLATES,
        agent_home,
        home_dir,
        catalog_agent_home,
        template,
    )
    .await
}

pub async fn ensure_agent_home_agents_md_from_template_with_home(
    agent_home: &Path,
    home_dir: &Path,
    template: &str,
) -> Result<Option<TemplateProvenanceRecord>> {
    package::ensure_agent_home_agents_md_from_template_with_home(
        TEMPLATES, agent_home, home_dir, template,
    )
    .await
}

pub async fn ensure_agent_home_agents_md_without_template_with_home(
    agent_home: &Path,
    home_dir: &Path,
) -> Result<Option<TemplateProvenanceRecord>> {
    package::ensure_agent_home_agents_md_without_template_with_home(TEMPLATES, agent_home, home_dir)
        .await
}

pub(crate) fn discover_agent_templates_catalog(
    user_home: Option<&Path>,
    agent_home: &Path,
) -> Vec<AgentTemplateCatalogEntry> {
    package::discover_agent_templates_catalog(TEMPLATES, user_home, agent_home)
}

pub(crate) fn resolve_agent_template_detail(
    entry: &AgentTemplateCatalogEntry,
) -> Option<AgentTemplateDetail> {
    package::resolve_agent_template_detail(TEMPLATES, entry)
}

pub(crate) async fn resolve_remote_agent_template_detail(
    entry: &AgentTemplateCatalogEntry,
) -> Result<AgentTemplateDetail> {
    package::resolve_remote_agent_template_detail(TEMPLATES, entry).await
}

pub fn builtin_skill_names() -> Vec<&'static str> {
    package::builtin_skill_names(TEMPLATES)
}

pub fn materialize_builtin_skill_ref(skills_root: &Path, name: &str) -> Result<PathBuf> {
    package::materialize_builtin_skill_ref(TEMPLATES, skills_root, name)
}

pub fn parse_template_manifest_for_api(template_dir: &Path) -> Option<serde_json::Value> {
    package::parse_template_manifest_for_api(TEMPLATES, template_dir)
}

pub fn install_template_from_github(user_home: &Path, github_url: &str) -> Result<String> {
    package::install_template_from_github(TEMPLATES, user_home, github_url)
}

#[cfg(all(test, holon_lib_test_shard = "c"))]
mod tests {
    use super::*;
    use crate::types::AgentTemplateSourceKind;
    use holon_agent_templates::templates::test_support::{self, EnvGuard};
    use std::{fs, thread, time::Duration};
    use tempfile::tempdir;

    mod archive_tests;

    const TEMPLATE_AGENTS_FILENAME: &str = "AGENTS.md";
    const TEMPLATE_MANIFEST_FILENAME: &str = "template.toml";
    const TEMPLATE_SKILLS_FILENAME: &str = "skills.toml";

    fn parse_skill_refs(path: PathBuf) -> Result<impl std::fmt::Debug> {
        test_support::parse_skill_refs(TEMPLATES, path)
    }

    fn templates_root_for_home(home: &Path) -> PathBuf {
        package::templates_root_for_home(home)
    }

    fn builtin_template_catalog_entry(template: &BuiltinTemplate) -> AgentTemplateCatalogEntry {
        test_support::builtin_template_catalog_entry(TEMPLATES, template)
    }

    #[test]
    fn template_parse_read_rejection_does_not_allocate_path_context() {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let home = tempdir().unwrap();
        let path = home.path().join(TEMPLATE_SKILLS_FILENAME);
        fs::write(&path, "skills=[]").unwrap();
        let scope = InputEvidenceScope {
            agent_id: "parse-read".into(),
            identity_incarnation: 1,
        };
        let header = (scope.agent_id.len() as u64 + 256) * 8;
        let path_bytes = (path.as_os_str().len() as u64 * 4 + 512) * 8;
        // Reject either the pathname itself or the following body admission.
        for admitted_path in [0, path_bytes] {
            let ledger = InputEvidenceBudgetLedger::new(Default::default()).unwrap();
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                header + admitted_path,
                1024,
            )
            .unwrap();
            let io = phase.enter();
            let error = parse_skill_refs(path.clone()).unwrap_err();
            assert!(error.is::<crate::runtime_error::RuntimeError>());
            assert_eq!(error.chain().count(), 1);
            assert!(!error.to_string().contains("failed to read"));
            assert!(phase.finish().is_err());
            assert_eq!(
                ledger.report().unwrap().observed_bytes,
                header + admitted_path
            );
            drop(io);
            drop(phase);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }

        fs::write(&path, [0xff]).unwrap();
        let ledger = InputEvidenceBudgetLedger::new(Default::default()).unwrap();
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let _io = phase.enter();
        let expected = crate::diagnostics::io_probe::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))
            .unwrap_err();
        assert!(expected
            .to_string()
            .starts_with(&format!("failed to read {}", path.display())));
        let error = parse_skill_refs(path).unwrap_err();
        assert_eq!(format!("{error:#}"), format!("{expected:#}"));
        phase.finish().unwrap();
    }

    #[test]
    fn template_parse_catalog_sequential_scratch_keeps_all_metadata_and_skills() {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let home = tempdir().unwrap();
        let agent_home = tempdir().unwrap();
        for index in 0..17 {
            let root = templates_root_for_home(home.path()).join(format!("template-{index}"));
            fs::create_dir_all(&root).unwrap();
            fs::write(
                root.join(TEMPLATE_AGENTS_FILENAME),
                "# Test\n\nCatalog summary.",
            )
            .unwrap();
            fs::write(root.join(TEMPLATE_MANIFEST_FILENAME), format!(
                "schema='1'\nid='template-{index}'\nname='Template {index}'\nsummary='Description {index}'\n"
            )).unwrap();
            let mut skills = (0..6)
                .map(|skill| {
                    format!(
                "[[skills]]\nkind='github'\nrepo='owner/repo'\npath='skills/skill-{skill}'\n"
            )
                })
                .collect::<String>();
            // Exercise the observed largest normal manifest, without HOME mutation.
            if index == 16 {
                skills.push('#');
                skills.extend(std::iter::repeat_n('a', 1138 - skills.len()));
            }
            fs::write(root.join(TEMPLATE_SKILLS_FILENAME), skills).unwrap();
        }
        let ledger = InputEvidenceBudgetLedger::new(Default::default()).unwrap();
        let scope = InputEvidenceScope {
            agent_id: "catalog".into(),
            identity_incarnation: 1,
        };
        let phase = ResourceReadMaterial::after(&ledger, &scope, (0, 1472)).unwrap();
        let _io = phase.enter();
        let catalog = discover_agent_templates_catalog(Some(home.path()), agent_home.path());
        phase.finish().unwrap();
        assert_eq!(catalog.len(), 17);
        for entry in &catalog {
            assert_eq!(entry.included_skills.len(), 6);
            assert!(entry.description.starts_with("Description "));
        }
        let retained = phase.guard().unwrap();
        drop(catalog);
        drop(_io);
        drop(phase);
        assert!(ledger.report().unwrap().observed_bytes > 0);
        drop(retained);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn template_parse_retains_unknown_field_error_key_paths() {
        use crate::{
            diagnostics::resource_io::{self, ResourceReadMaterial},
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let dotted = std::iter::repeat_n("a", 70).collect::<Vec<_>>().join(".");
        let input = format!(
            "[[skills]]\nkind='local'\npath='x'\n{dotted}={{{dotted}={{z=18446744073709551616}}}}\n"
        );
        let expected = test_support::parse_template_skills_manifest(&input).unwrap_err();
        assert_eq!(
            expected.message(),
            "invalid type: integer `18446744073709551616` as i128, expected any value"
        );
        // The locked parser's Debug exposes the escaping key path. Its Vec
        // needs at least 256 slots even though this input has only five '='.
        assert!(format!("{expected:?}").matches("\"a\"").count() >= 140);
        let ledger = InputEvidenceBudgetLedger::new(Default::default()).unwrap();
        let scope = InputEvidenceScope {
            agent_id: "parse".into(),
            identity_incarnation: 1,
        };
        let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
        let io = phase.enter();
        let before = phase.finish().unwrap();
        let scratch = resource_io::template_parse(&input).unwrap().unwrap();
        let actual = test_support::parse_template_skills_manifest(&input).unwrap_err();
        assert_eq!(actual, expected);
        drop(scratch);
        assert!(phase.finish().unwrap().1 - before.1 >= 256);
        let retained = phase.guard().unwrap();
        drop(io);
        drop(phase);
        assert!(ledger.report().unwrap().observed_bytes > 0);
        drop(actual);
        drop(retained);
        assert_eq!(ledger.report().unwrap().observed_bytes, 0);
    }

    #[test]
    fn template_parse_budget_preserves_stock_success_and_error_diagnostics() {
        use crate::{
            diagnostics::resource_io::ResourceReadMaterial,
            runtime::{
                input_evidence_budget::InputEvidenceBudgetLedger,
                resident_input_evidence::InputEvidenceScope,
            },
        };
        let home = tempdir().unwrap();
        let path = home.path().join(TEMPLATE_SKILLS_FILENAME);
        let mut inputs = vec![
            "skills=[]".to_owned(),
            "[[skills]]\nkind='local'\npath='a.b/ü'\nunknown={x=[1,2,3]}".to_owned(),
            "[[skills]]\nkind='github'\nuses='owner/repo/skills/demo@main'".to_owned(),
            "[[skills]]\nkind='unknown'\nunknown={x=[{y=1}]}".to_owned(),
            "[[skills]]\nkind='local'\nkind='github'\n".to_owned(),
            "skills=[{kind='local',path=\"\\x\"}]".to_owned(),
            "skills=[{kind='local',path='a'}".to_owned(),
            "skills=[{path='missing tag'}]".to_owned(),
            "ignored=1979-05-27T07:32:00Z\nskills=[]".to_owned(),
        ];
        inputs.push(format!(
            "{}=1",
            (0..81).map(|_| "a").collect::<Vec<_>>().join(".")
        ));
        inputs.push(format!("ignored=[{}]\nskills=[]", "1 ".repeat(128)));
        for input in inputs {
            fs::write(&path, &input).unwrap();
            let expected = parse_skill_refs(path.clone())
                .map(|refs| format!("{refs:?}"))
                .map_err(|error| format!("{error:#}"));
            let ledger = InputEvidenceBudgetLedger::new(Default::default()).unwrap();
            let scope = InputEvidenceScope {
                agent_id: "parse".into(),
                identity_incarnation: 1,
            };
            let phase = ResourceReadMaterial::new(&ledger, &scope).unwrap();
            let _io = phase.enter();
            let actual = parse_skill_refs(path.clone())
                .map(|refs| format!("{refs:?}"))
                .map_err(|error| format!("{error:#}"));
            phase.finish().unwrap();
            assert_eq!(actual, expected, "{input}");
            drop(actual);
            drop(_io);
            drop(phase);
            assert_eq!(ledger.report().unwrap().observed_bytes, 0);
        }
    }

    #[test]
    fn effective_remote_sources_include_official_source_by_default() {
        let config = AgentTemplatesConfigFile::default();

        let sources = effective_agent_template_remote_sources(&config);

        let official = sources
            .get(OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_ID)
            .expect("official remote source should be registered by default");
        assert_eq!(official.url, OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_URL);
        assert!(official.enabled());
    }

    #[test]
    fn effective_remote_sources_preserve_explicit_official_override() {
        let mut config = AgentTemplatesConfigFile::default();
        config.remote_sources.insert(
            OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_ID.to_string(),
            AgentTemplateRemoteSourceConfigFile {
                url: "https://github.com/example/custom".into(),
                git_ref: Some("dev".into()),
                enabled: Some(false),
                credential_profile: None,
            },
        );

        let sources = effective_agent_template_remote_sources(&config);

        let official = sources
            .get(OFFICIAL_AGENT_TEMPLATE_REMOTE_SOURCE_ID)
            .expect("explicit official remote source should be preserved");
        assert_eq!(official.url, "https://github.com/example/custom");
        assert_eq!(official.git_ref.as_deref(), Some("dev"));
        assert!(!official.enabled());
    }

    #[test]
    fn catalog_entry_has_name_and_schema_from_template_toml() {
        let entry = builtin_template_catalog_entry(
            BUILTIN_TEMPLATES
                .iter()
                .find(|t| t.template_id == "holon-default")
                .unwrap(),
        );
        assert_eq!(entry.name, "Holon Default Agent");
        assert_eq!(
            entry.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
        assert!(!entry.description.is_empty());
    }

    #[test]
    fn resolve_agent_template_detail_builtin() {
        let entry = builtin_template_catalog_entry(
            BUILTIN_TEMPLATES
                .iter()
                .find(|template| template.template_id == DEFAULT_AGENT_TEMPLATE_ID)
                .unwrap(),
        );
        let detail = resolve_agent_template_detail(&entry).unwrap();
        assert_eq!(detail.template_id, "holon-default");
        assert_eq!(detail.source, AgentTemplateSourceKind::Builtin);
        assert!(!detail.agents_md.is_empty());
        assert!(detail.skills.is_empty());
        assert_eq!(detail.name, "Holon Default Agent");
        assert_eq!(
            detail.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
    }

    #[tokio::test]
    async fn remote_source_needs_sync_when_never_synced_but_not_after_recent_success() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.sqlite.lock"),
        )
        .unwrap();
        let source_id = "official";
        let config = AgentTemplateRemoteSourceConfigFile {
            url: "https://github.com/owner/repo".into(),
            git_ref: Some("main".into()),
            enabled: Some(true),
            credential_profile: None,
        };

        assert!(agent_template_remote_source_needs_sync(
            &db,
            source_id,
            chrono::Duration::hours(24)
        )
        .unwrap());

        let status = AgentTemplateRemoteSourceStatus {
            source_id: source_id.into(),
            kind: "github".into(),
            url: config.url.clone(),
            requested_ref: config.git_ref.clone(),
            enabled: config.enabled(),
            status: AgentTemplateRemoteSourceSyncStatus::Synced,
            last_synced_at: Some(Utc::now()),
            resolved_ref: Some("main".into()),
            resolved_revision: Some("abc123".into()),
            error: None,
        };
        upsert_remote_source_sync(&db, &status, &[], &[]).unwrap();

        assert!(!agent_template_remote_source_needs_sync(
            &db,
            source_id,
            chrono::Duration::hours(24)
        )
        .unwrap());
    }

    fn remote_source_reader_fixture() -> Result<(
        tempfile::TempDir,
        RuntimeDb,
        AgentTemplateRemoteSourceConfigFile,
    )> {
        let home = tempdir()?;
        let mut db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.sqlite.lock"),
        )?;
        db.configure_reader_pool_for_tests(1, Duration::from_millis(100))?;
        let config = AgentTemplateRemoteSourceConfigFile {
            url: "https://github.com/owner/repo".into(),
            git_ref: Some("main".into()),
            enabled: Some(true),
            credential_profile: None,
        };
        let status = AgentTemplateRemoteSourceStatus {
            source_id: "official".into(),
            kind: "github".into(),
            url: config.url.clone(),
            requested_ref: config.git_ref.clone(),
            enabled: true,
            status: AgentTemplateRemoteSourceSyncStatus::Synced,
            last_synced_at: Some(Utc::now()),
            resolved_ref: Some("main".into()),
            resolved_revision: Some("abc123".into()),
            error: None,
        };
        let catalog = [AgentTemplateCatalogEntry {
            catalog_id: "remote:official:worker".into(),
            template: "remote:official:worker".into(),
            template_id: "worker".into(),
            source: AgentTemplateSourceKind::Remote,
            path: None,
            name: "Worker".into(),
            schema_version: None,
            description: String::new(),
            included_skills: Vec::new(),
            source_id: Some("official".into()),
            resolved_ref: Some("main".into()),
            resolved_revision: Some("abc123".into()),
            source_url: None,
        }];
        upsert_remote_source_sync(&db, &status, &catalog, &[])?;
        Ok((home, db, config))
    }

    fn remote_source_stored_payload(db: &RuntimeDb) -> Result<(String, String, String, i64)> {
        db.with_read(|connection| {
            Ok(connection.query_row(
                "SELECT status, catalog_json, diagnostics_json, retry_count \
                 FROM agent_template_remote_source_syncs WHERE source_id = 'official'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?)
        })
    }

    #[test]
    fn remote_source_readers_reuse_capacity_one_and_release_before_failure_write() -> Result<()> {
        let (_home, db, config) = remote_source_reader_fixture()?;
        let sources = BTreeMap::from([("official".into(), config.clone())]);
        let catalog = load_remote_source_row(&db, "official")?.unwrap().catalog;
        assert!(!catalog.is_empty());
        for _ in 0..3 {
            assert_eq!(
                load_remote_template_catalog_snapshot(&db, &sources)?
                    .sources
                    .len(),
                1
            );
            assert!(!agent_template_remote_source_needs_sync(
                &db,
                "official",
                chrono::Duration::hours(24),
            )?);
            record_agent_template_remote_source_sync_failure(
                &db,
                "official",
                &config,
                "temporary failure",
            )?;
            assert_eq!(
                load_remote_source_row(&db, "official")?.unwrap().catalog,
                catalog
            );
        }
        record_agent_template_remote_source_sync_failure(&db, "missing", &config, "first failure")?;
        assert!(load_remote_source_row(&db, "missing")?
            .unwrap()
            .catalog
            .is_empty());
        assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
        Ok(())
    }

    #[test]
    fn remote_source_reader_saturation_propagates_without_overwriting_catalog() -> Result<()> {
        let (_home, db, config) = remote_source_reader_fixture()?;
        let sources = BTreeMap::from([("official".into(), config.clone())]);
        let before = remote_source_stored_payload(&db)?;
        let worker_db = db.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            worker_db.with_read(|_| {
                started_tx.send(())?;
                release_rx.recv_timeout(Duration::from_secs(10))?;
                Ok(())
            })
        });
        started_rx.recv_timeout(Duration::from_secs(5))?;
        let reads = [
            (
                "snapshot",
                load_remote_template_catalog_snapshot(&db, &sources).map(drop),
            ),
            (
                "empty snapshot",
                load_remote_template_catalog_snapshot(&db, &BTreeMap::new()).map(drop),
            ),
            (
                "needs sync",
                agent_template_remote_source_needs_sync(
                    &db,
                    "official",
                    chrono::Duration::hours(24),
                )
                .map(drop),
            ),
            (
                "failure recorder",
                record_agent_template_remote_source_sync_failure(
                    &db, "official", &config, "failed",
                )
                .map(drop),
            ),
        ];
        release_tx.send(())?;
        worker.join().expect("reader thread panicked")?;
        for (name, result) in reads {
            let error = result.expect_err(name);
            assert!(
                error
                    .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
                    .is_some(),
                "{name}: {error:#}",
            );
        }
        assert_eq!(remote_source_stored_payload(&db)?, before);
        assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
        Ok(())
    }

    #[test]
    fn remote_source_decode_failure_does_not_overwrite_catalog() -> Result<()> {
        let (_home, db, config) = remote_source_reader_fixture()?;
        db.transaction(|tx| {
            tx.execute(
                "UPDATE agent_template_remote_source_syncs SET diagnostics_json = 'invalid json' \
                 WHERE source_id = 'official'",
                [],
            )?;
            Ok(())
        })?;
        let before = remote_source_stored_payload(&db)?;
        let error =
            record_agent_template_remote_source_sync_failure(&db, "official", &config, "failed")
                .expect_err("a decode failure must not replace a stored catalog with an empty one");
        assert!(
            format!("{error:#}").contains("failed to decode remote template diagnostics_json"),
            "{error:#}",
        );
        assert_eq!(remote_source_stored_payload(&db)?, before);
        assert_eq!(db.reader_pool_stats_for_tests().connections_opened, 1);
        Ok(())
    }

    #[tokio::test]
    async fn failed_remote_source_sync_is_deferred_until_retry_deadline() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let db = RuntimeDb::open_and_migrate(
            home.path().join("runtime.sqlite"),
            home.path().join("runtime.sqlite.lock"),
        )
        .unwrap();
        let status = AgentTemplateRemoteSourceStatus {
            source_id: "official".into(),
            kind: "github".into(),
            url: "https://github.com/owner/repo".into(),
            requested_ref: Some("main".into()),
            enabled: true,
            status: AgentTemplateRemoteSourceSyncStatus::Failed,
            last_synced_at: Some(Utc::now()),
            resolved_ref: None,
            resolved_revision: None,
            error: Some(
                "GitHub archive request failed (HTTP 429; retry after 2099-01-01T00:00:00+00:00)"
                    .into(),
            ),
        };
        upsert_remote_source_sync(&db, &status, &[], &[]).unwrap();

        let retry_at: String = db
            .connection()
            .unwrap()
            .query_row(
                "SELECT next_retry_at FROM agent_template_remote_source_syncs \
                 WHERE source_id = 'official'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(retry_at.starts_with("2099-01-01T00:00:00"));
        assert!(!agent_template_remote_source_needs_sync(
            &db,
            "official",
            chrono::Duration::hours(24)
        )
        .unwrap());
    }

    #[tokio::test]
    async fn provenance_builtin_template_has_schema_version() {
        let _lock = crate::test_env::lock_env();
        let home = tempdir().unwrap();
        let _guard = EnvGuard::set("HOME", home.path().display().to_string());
        seed_builtin_templates().unwrap();

        let agent_home = home.path().join("agent-builtin");
        let provenance = initialize_agent_home_without_template(&agent_home)
            .await
            .unwrap();

        assert_eq!(
            provenance.schema_version.as_deref(),
            Some("holon.agent_template.v1")
        );
    }
}

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use rusqlite::params;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    config::{load_persisted_config_at, HolonConfigFile, ModelRouteRef},
    runtime_db::RuntimeDb,
};

const AGENT_MODEL_ROUTE_FIELDS: [&str; 4] = [
    "model_override",
    "pending_fallback_model",
    "last_requested_model",
    "last_active_model",
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelConfigMigrationStatus {
    Canonical,
    Legacy,
    Invalid,
    Ambiguous,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ModelConfigMigrationField {
    pub location: String,
    pub status: ModelConfigMigrationStatus,
    pub current: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub struct ModelConfigMigrationStoreReport {
    pub changed: bool,
    pub fields: Vec<ModelConfigMigrationField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ModelConfigMigrationReport {
    pub ok: bool,
    pub write: bool,
    pub changed: bool,
    pub config_file_path: PathBuf,
    pub runtime_db_path: PathBuf,
    pub config: ModelConfigMigrationStoreReport,
    pub agent_state: ModelConfigMigrationStoreReport,
}

#[derive(Debug)]
struct ConfigInspection {
    config: HolonConfigFile,
    report: ModelConfigMigrationStoreReport,
}

#[derive(Debug)]
struct AgentStateMutation {
    agent_id: String,
    payload_json: String,
}

#[derive(Debug)]
struct AgentStateInspection {
    mutations: Vec<AgentStateMutation>,
    report: ModelConfigMigrationStoreReport,
}

pub fn migrate_model_config_routes(
    config_file_path: &Path,
    runtime_db: &RuntimeDb,
    write: bool,
) -> Result<ModelConfigMigrationReport> {
    let mut config = inspect_config(config_file_path)?;
    let agent_state = inspect_agent_state(runtime_db)?;
    let ok = all_fields_valid(&config.report) && all_fields_valid(&agent_state.report);

    if write && ok {
        if config.report.changed {
            let backup_path = backup_config_once(config_file_path)?;
            write_config_atomically(config_file_path, &config.config)?;
            config.report.backup_path = backup_path;
        }
        if agent_state.report.changed {
            write_agent_state_mutations(runtime_db, &agent_state.mutations)?;
        }
    }

    let changed = config.report.changed || agent_state.report.changed;
    Ok(ModelConfigMigrationReport {
        ok,
        write,
        changed,
        config_file_path: config_file_path.to_path_buf(),
        runtime_db_path: runtime_db.path().to_path_buf(),
        config: config.report,
        agent_state: agent_state.report,
    })
}

fn inspect_config(path: &Path) -> Result<ConfigInspection> {
    let mut config = load_persisted_config_at(path)?;
    let mut report = ModelConfigMigrationStoreReport::default();

    inspect_optional_route(
        "model.default",
        &mut config.model.default,
        false,
        &mut report,
    );
    for (index, value) in config.model.fallbacks.iter_mut().enumerate() {
        inspect_route_value(
            format!("model.fallbacks[{index}]"),
            value,
            false,
            &mut report,
        );
    }
    inspect_optional_route(
        "vision.default",
        &mut config.vision.default,
        false,
        &mut report,
    );
    inspect_optional_route(
        "image_generation.default",
        &mut config.image_generation.default,
        true,
        &mut report,
    );

    Ok(ConfigInspection { config, report })
}

fn inspect_optional_route(
    location: &str,
    value: &mut Option<String>,
    allow_auto: bool,
    report: &mut ModelConfigMigrationStoreReport,
) {
    if allow_auto
        && value
            .as_deref()
            .is_some_and(|current| current.trim().eq_ignore_ascii_case("auto"))
    {
        let current = value.take().unwrap_or_default();
        report.changed = true;
        report.fields.push(ModelConfigMigrationField {
            location: location.to_string(),
            status: ModelConfigMigrationStatus::Legacy,
            current,
            proposed: Some("<unset>".into()),
            error: None,
        });
        return;
    }

    if let Some(value) = value {
        inspect_route_value(location.to_string(), value, allow_auto, report);
    }
}

fn inspect_route_value(
    location: String,
    value: &mut String,
    allow_auto: bool,
    report: &mut ModelConfigMigrationStoreReport,
) {
    let current = value.clone();
    if allow_auto && current.trim().eq_ignore_ascii_case("auto") {
        report.fields.push(ModelConfigMigrationField {
            location,
            status: ModelConfigMigrationStatus::Canonical,
            current,
            proposed: None,
            error: None,
        });
        return;
    }

    match ModelRouteRef::parse_compatible(&current) {
        Ok(route_ref) => {
            let canonical = route_ref.as_string();
            let is_canonical = current == canonical;
            if !is_canonical {
                *value = canonical.clone();
                report.changed = true;
            }
            report.fields.push(ModelConfigMigrationField {
                location,
                status: if is_canonical {
                    ModelConfigMigrationStatus::Canonical
                } else {
                    ModelConfigMigrationStatus::Legacy
                },
                current,
                proposed: (!is_canonical).then_some(canonical),
                error: None,
            });
        }
        Err(error) => report.fields.push(ModelConfigMigrationField {
            location,
            status: ModelConfigMigrationStatus::Invalid,
            current,
            proposed: None,
            error: Some(error.to_string()),
        }),
    }
}

fn inspect_agent_state(runtime_db: &RuntimeDb) -> Result<AgentStateInspection> {
    let connection = runtime_db.connection()?;
    let mut statement =
        connection.prepare("SELECT agent_id, payload_json FROM agent_states ORDER BY agent_id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut mutations = Vec::new();
    let mut report = ModelConfigMigrationStoreReport::default();

    for row in rows {
        let (agent_id, payload_json) = row?;
        let mut payload: Value = serde_json::from_str(&payload_json)
            .with_context(|| format!("failed to parse agent state payload for {agent_id}"))?;
        let object = payload
            .as_object_mut()
            .ok_or_else(|| anyhow!("agent state payload for {agent_id} must be a JSON object"))?;
        let mut row_changed = false;

        for field in AGENT_MODEL_ROUTE_FIELDS {
            let Some(value) = object.get_mut(field) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            let Some(raw) = value.as_str() else {
                report.fields.push(ModelConfigMigrationField {
                    location: format!("agent_states[{agent_id}].{field}"),
                    status: ModelConfigMigrationStatus::Invalid,
                    current: value.to_string(),
                    proposed: None,
                    error: Some("model route value must be a string or null".into()),
                });
                continue;
            };
            let current = raw.to_string();
            match ModelRouteRef::parse_compatible(&current) {
                Ok(route_ref) => {
                    let canonical = route_ref.as_string();
                    let is_canonical = current == canonical;
                    if !is_canonical {
                        *value = Value::String(canonical.clone());
                        row_changed = true;
                        report.changed = true;
                    }
                    report.fields.push(ModelConfigMigrationField {
                        location: format!("agent_states[{agent_id}].{field}"),
                        status: if is_canonical {
                            ModelConfigMigrationStatus::Canonical
                        } else {
                            ModelConfigMigrationStatus::Legacy
                        },
                        current,
                        proposed: (!is_canonical).then_some(canonical),
                        error: None,
                    });
                }
                Err(error) => report.fields.push(ModelConfigMigrationField {
                    location: format!("agent_states[{agent_id}].{field}"),
                    status: ModelConfigMigrationStatus::Invalid,
                    current,
                    proposed: None,
                    error: Some(error.to_string()),
                }),
            }
        }

        if row_changed {
            mutations.push(AgentStateMutation {
                agent_id,
                payload_json: serde_json::to_string(&payload)?,
            });
        }
    }

    Ok(AgentStateInspection { mutations, report })
}

fn all_fields_valid(report: &ModelConfigMigrationStoreReport) -> bool {
    report.fields.iter().all(|field| {
        matches!(
            field.status,
            ModelConfigMigrationStatus::Canonical | ModelConfigMigrationStatus::Legacy
        )
    })
}

fn backup_config_once(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let backup_path = path.with_extension("json.model-route-migration.bak");
    if !backup_path.exists() {
        fs::copy(path, &backup_path).with_context(|| {
            format!(
                "failed to back up {} to {}",
                path.display(),
                backup_path.display()
            )
        })?;
        OpenOptions::new()
            .read(true)
            .open(&backup_path)?
            .sync_all()
            .with_context(|| format!("failed to sync {}", backup_path.display()))?;
    }
    Ok(Some(backup_path))
}

fn write_config_atomically(path: &Path, config: &HolonConfigFile) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp_path = path.with_extension(format!("json.migrate-{}", std::process::id()));
    let content = serde_json::to_vec_pretty(config).context("failed to serialize config")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&temp_path)
            .with_context(|| format!("failed to open {}", temp_path.display()))?;
        file.write_all(&content)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "failed to replace {} with {}",
                path.display(),
                temp_path.display()
            )
        })?;
        OpenOptions::new()
            .read(true)
            .open(parent)?
            .sync_all()
            .with_context(|| format!("failed to sync {}", parent.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn write_agent_state_mutations(
    runtime_db: &RuntimeDb,
    mutations: &[AgentStateMutation],
) -> Result<()> {
    runtime_db.transaction(|transaction| {
        for mutation in mutations {
            transaction.execute(
                "UPDATE agent_states SET payload_json = ?1 WHERE agent_id = ?2",
                params![mutation.payload_json, mutation.agent_id],
            )?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::save_persisted_config_at, types::AgentState};
    use serde_json::json;
    use tempfile::TempDir;

    struct Fixture {
        _temp_dir: TempDir,
        config_path: PathBuf,
        runtime_db: RuntimeDb,
    }

    impl Fixture {
        fn new(config: &HolonConfigFile) -> Result<Self> {
            let temp_dir = tempfile::tempdir()?;
            let config_path = temp_dir.path().join("config.json");
            save_persisted_config_at(&config_path, config)?;
            let runtime_db = RuntimeDb::open_and_migrate(
                temp_dir.path().join("runtime.sqlite"),
                temp_dir.path().join("runtime.lock"),
            )?;
            Ok(Self {
                _temp_dir: temp_dir,
                config_path,
                runtime_db,
            })
        }

        fn insert_agent_payload(&self, agent_id: &str, model: &str) -> Result<String> {
            self.runtime_db
                .agent_states()
                .upsert(&AgentState::new(agent_id))?;
            let mut payload = serde_json::to_value(AgentState::new(agent_id))?;
            payload["model_override"] = json!(model);
            payload["pending_fallback_model"] = json!(model);
            payload["last_requested_model"] = json!(model);
            payload["last_active_model"] = json!(model);
            let payload_json = serde_json::to_string(&payload)?;
            self.runtime_db.connection()?.execute(
                "UPDATE agent_states SET payload_json = ?1 WHERE agent_id = ?2",
                params![payload_json, agent_id],
            )?;
            Ok(payload_json)
        }

        fn agent_payload(&self, agent_id: &str) -> Result<String> {
            Ok(self.runtime_db.connection()?.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id = ?1",
                [agent_id],
                |row| row.get(0),
            )?)
        }
    }

    fn legacy_config() -> HolonConfigFile {
        let mut config = HolonConfigFile::default();
        config.model.default = Some("openai/gpt-5.4".into());
        config.model.fallbacks = vec![
            "openrouter/anthropic/claude-3.5-sonnet".into(),
            "volcengine/doubao-seedream-5.0-lite".into(),
        ];
        config.vision.default = Some("openai/gpt-5.4".into());
        config.image_generation.default = Some("auto".into());
        config
    }

    fn canonical(value: &str) -> String {
        ModelRouteRef::parse_compatible(value).unwrap().as_string()
    }

    #[test]
    fn dry_run_reports_changes_without_writing_either_store() -> Result<()> {
        let fixture = Fixture::new(&legacy_config())?;
        let original_config = fs::read(&fixture.config_path)?;
        let original_agent = fixture.insert_agent_payload("agent-a", "openai/gpt-5.4")?;

        let report = migrate_model_config_routes(&fixture.config_path, &fixture.runtime_db, false)?;

        assert!(report.ok);
        assert!(!report.write);
        assert!(report.changed);
        assert!(report.config.changed);
        assert!(report.agent_state.changed);
        assert_eq!(fs::read(&fixture.config_path)?, original_config);
        assert_eq!(fixture.agent_payload("agent-a")?, original_agent);
        assert!(!fixture
            .config_path
            .with_extension("json.model-route-migration.bak")
            .exists());
        Ok(())
    }

    #[test]
    fn write_creates_one_backup_and_is_idempotent() -> Result<()> {
        let fixture = Fixture::new(&legacy_config())?;
        let original_config = fs::read(&fixture.config_path)?;
        fixture.insert_agent_payload("agent-a", "volcengine/doubao-seedream-5.0-lite")?;

        let first = migrate_model_config_routes(&fixture.config_path, &fixture.runtime_db, true)?;
        assert!(first.ok);
        assert!(first.changed);
        let backup_path = first.config.backup_path.expect("config backup");
        assert_eq!(fs::read(&backup_path)?, original_config);

        let migrated = load_persisted_config_at(&fixture.config_path)?;
        assert_eq!(
            migrated.model.default.as_deref(),
            Some(canonical("openai/gpt-5.4").as_str())
        );
        assert_eq!(
            migrated.model.fallbacks,
            vec![
                canonical("openrouter/anthropic/claude-3.5-sonnet"),
                canonical("volcengine/doubao-seedream-5.0-lite"),
            ]
        );
        assert!(migrated.image_generation.default.is_none());
        let agent: Value = serde_json::from_str(&fixture.agent_payload("agent-a")?)?;
        let expected_agent = canonical("volcengine/doubao-seedream-5.0-lite");
        for field in AGENT_MODEL_ROUTE_FIELDS {
            assert_eq!(agent[field].as_str(), Some(expected_agent.as_str()));
        }

        let config_after_first = fs::read(&fixture.config_path)?;
        let agent_after_first = fixture.agent_payload("agent-a")?;
        let second = migrate_model_config_routes(&fixture.config_path, &fixture.runtime_db, true)?;
        assert!(second.ok);
        assert!(!second.changed);
        assert!(!second.config.changed);
        assert!(!second.agent_state.changed);
        assert!(second.config.backup_path.is_none());
        assert_eq!(fs::read(&fixture.config_path)?, config_after_first);
        assert_eq!(fixture.agent_payload("agent-a")?, agent_after_first);
        assert_eq!(fs::read(&backup_path)?, original_config);
        Ok(())
    }

    #[test]
    fn invalid_preflight_prevents_partial_writes() -> Result<()> {
        let fixture = Fixture::new(&legacy_config())?;
        let original_config = fs::read(&fixture.config_path)?;
        let original_agent = fixture.insert_agent_payload("agent-a", "not-a-model-ref")?;

        let report = migrate_model_config_routes(&fixture.config_path, &fixture.runtime_db, true)?;

        assert!(!report.ok);
        assert!(report.changed);
        assert!(report.config.changed);
        assert!(!report.agent_state.changed);
        assert!(report
            .agent_state
            .fields
            .iter()
            .all(|field| field.status == ModelConfigMigrationStatus::Invalid));
        assert_eq!(fs::read(&fixture.config_path)?, original_config);
        assert_eq!(fixture.agent_payload("agent-a")?, original_agent);
        assert!(!fixture
            .config_path
            .with_extension("json.model-route-migration.bak")
            .exists());
        Ok(())
    }

    #[test]
    fn agent_state_batch_rolls_back_when_any_update_fails() -> Result<()> {
        let mut config = HolonConfigFile::default();
        config.model.default = Some(canonical("openai/gpt-5.4"));
        let fixture = Fixture::new(&config)?;
        let original_a = fixture.insert_agent_payload("agent-a", "openai/gpt-5.4")?;
        let original_b = fixture.insert_agent_payload("agent-b", "openai/gpt-5.4")?;
        fixture.runtime_db.connection()?.execute_batch(
            "CREATE TRIGGER reject_agent_b_model_route_migration
             BEFORE UPDATE OF payload_json ON agent_states
             WHEN OLD.agent_id = 'agent-b'
             BEGIN
               SELECT RAISE(ABORT, 'reject agent-b migration');
             END;",
        )?;

        let error = migrate_model_config_routes(&fixture.config_path, &fixture.runtime_db, true)
            .expect_err("second update should abort the transaction");

        assert!(error.to_string().contains("reject agent-b migration"));
        assert_eq!(fixture.agent_payload("agent-a")?, original_a);
        assert_eq!(fixture.agent_payload("agent-b")?, original_b);
        Ok(())
    }
}

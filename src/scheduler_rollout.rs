use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
};

use anyhow::{anyhow, bail, Result};

use crate::{
    domain::scheduler_protocol::{
        managed_shadow_rollout_manifest, rollout_class_evidence_is_complete, ProtocolMode,
        RolloutCommand, RolloutManifest, RolloutPreflightState, RolloutState,
        ScenarioHardBlockerRecord, ScenarioMode,
    },
    runtime_db::RuntimeDb,
};

pub(crate) const SCHEDULER_ENV: &str = "HOLON_SCHEDULER";
const LEGACY_PRODUCTION_COMMANDS_ENV: &str = "HOLON_SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerDesiredMode {
    Legacy,
    Shadow,
    Authoritative,
}

impl SchedulerDesiredMode {
    fn token(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Shadow => "shadow",
            Self::Authoritative => "authoritative",
        }
    }
}

fn desired_mode_from_value(value: Option<&OsStr>) -> Result<Option<SchedulerDesiredMode>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .ok_or_else(|| anyhow!("{SCHEDULER_ENV} must be valid UTF-8"))?
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "legacy" => Ok(Some(SchedulerDesiredMode::Legacy)),
        "shadow" => Ok(Some(SchedulerDesiredMode::Shadow)),
        "authoritative" => Ok(Some(SchedulerDesiredMode::Authoritative)),
        _ => Err(anyhow!(
            "{SCHEDULER_ENV} expects legacy, shadow, or authoritative"
        )),
    }
}

pub(crate) fn production_commands_enabled_from_env() -> Result<bool> {
    production_commands_enabled_from_values(
        std::env::var_os(SCHEDULER_ENV).as_deref(),
        std::env::var_os(LEGACY_PRODUCTION_COMMANDS_ENV).as_deref(),
    )
}

pub(crate) fn reconcile_from_env(runtime_db: &RuntimeDb) -> Result<()> {
    let Some(desired) = desired_mode_for_values(
        std::env::var_os(SCHEDULER_ENV).as_deref(),
        std::env::var_os(LEGACY_PRODUCTION_COMMANDS_ENV).as_deref(),
    )?
    else {
        return Ok(());
    };
    let rollout = runtime_db.transitions().load_scheduler_rollout_state()?;
    let commands = reconciliation_commands(runtime_db, desired, &rollout)?;
    if !commands.is_empty() {
        runtime_db.apply_scheduler_rollout_commands(&commands)?;
    }
    Ok(())
}

fn desired_mode_for_values(
    desired: Option<&OsStr>,
    legacy_production_commands: Option<&OsStr>,
) -> Result<Option<SchedulerDesiredMode>> {
    if let Some(desired) = desired_mode_from_value(desired)? {
        return Ok(Some(desired));
    }
    boolean_from_value(LEGACY_PRODUCTION_COMMANDS_ENV, legacy_production_commands)?;
    Ok(None)
}

fn boolean_from_value(name: &str, value: Option<&OsStr>) -> Result<Option<bool>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .ok_or_else(|| anyhow!("{name} must be valid UTF-8"))?
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(anyhow!("{name} expects a boolean")),
    }
}

pub(crate) fn production_commands_enabled_from_values(
    desired: Option<&OsStr>,
    legacy_production_commands: Option<&OsStr>,
) -> Result<bool> {
    if let Some(desired) = desired_mode_from_value(desired)? {
        return Ok(desired == SchedulerDesiredMode::Authoritative);
    }
    Ok(
        boolean_from_value(LEGACY_PRODUCTION_COMMANDS_ENV, legacy_production_commands)?
            .unwrap_or(false),
    )
}

fn reconciliation_commands(
    runtime_db: &RuntimeDb,
    desired: SchedulerDesiredMode,
    rollout: &RolloutState,
) -> Result<Vec<(String, RolloutCommand)>> {
    let schema_revision = u64::try_from(runtime_db.current_schema_version()?)
        .map_err(|_| anyhow!("runtime schema revision is negative"))?;
    reconciliation_commands_for_state(desired, rollout, schema_revision)
}

fn reconciliation_commands_for_state(
    desired: SchedulerDesiredMode,
    rollout: &RolloutState,
    schema_revision: u64,
) -> Result<Vec<(String, RolloutCommand)>> {
    let mut planner = ReconciliationPlanner::new(desired, rollout);
    if desired != SchedulerDesiredMode::Legacy && planner.manifest.is_none() {
        if let Some((revision, _)) = rollout
            .preflights
            .iter()
            .rev()
            .find(|(_, preflight)| preflight.state == RolloutPreflightState::Open)
        {
            bail!(
                "cannot reconcile {SCHEDULER_ENV}={} while rollout preflight {revision} is open",
                desired.token()
            );
        }
        if let Some((revision, preflight)) = rollout
            .preflights
            .iter()
            .rev()
            .find(|(_, preflight)| preflight.state == RolloutPreflightState::Completed)
        {
            let manifest = preflight
                .manifest
                .clone()
                .ok_or_else(|| anyhow!("completed rollout preflight {revision} has no manifest"))?;
            planner.install_completed_manifest(manifest);
        } else {
            planner.install_managed_shadow_manifest(
                rollout.latest_preflight_revision + 1,
                schema_revision,
            );
        }
    }
    match desired {
        SchedulerDesiredMode::Legacy => planner.plan_legacy()?,
        SchedulerDesiredMode::Shadow => planner.plan_shadow()?,
        SchedulerDesiredMode::Authoritative => planner.plan_authoritative()?,
    }
    Ok(planner.finish())
}

struct ReconciliationPlanner {
    desired: SchedulerDesiredMode,
    initial_config_revision: u64,
    config_revision: u64,
    protocol_mode: ProtocolMode,
    manifest: Option<RolloutManifest>,
    scenario_modes: BTreeMap<String, ScenarioMode>,
    hard_blockers: BTreeSet<ScenarioHardBlockerRecord>,
    commands: Vec<RolloutCommand>,
}

impl ReconciliationPlanner {
    fn new(desired: SchedulerDesiredMode, rollout: &RolloutState) -> Self {
        Self {
            desired,
            initial_config_revision: rollout.config_revision,
            config_revision: rollout.config_revision,
            protocol_mode: rollout.protocol_mode,
            manifest: rollout.manifest.clone(),
            scenario_modes: rollout
                .scenarios
                .iter()
                .map(|(scenario, authority)| (scenario.clone(), authority.mode))
                .collect(),
            hard_blockers: rollout.hard_blockers.clone(),
            commands: Vec::new(),
        }
    }

    fn install_managed_shadow_manifest(&mut self, preflight_revision: u64, schema_revision: u64) {
        let manifest_revision = self
            .manifest
            .as_ref()
            .map_or(1, |manifest| manifest.revision + 1);
        let manifest = managed_shadow_rollout_manifest(
            manifest_revision,
            preflight_revision,
            format!("holon-{}", env!("CARGO_PKG_VERSION")),
            format!("runtime-db-schema-{schema_revision}"),
            schema_revision,
        );
        self.commands.push(RolloutCommand::OpenPreflight {
            expected_config_revision: self.config_revision,
            manifest_revision,
        });
        self.commands.push(RolloutCommand::CompletePreflight {
            expected_config_revision: self.config_revision,
            expected_preflight_revision: preflight_revision,
            manifest: manifest.clone(),
        });
        self.commands.push(RolloutCommand::InstallManifest {
            expected_config_revision: self.config_revision,
            manifest: manifest.clone(),
        });
        // Opening and completing preserve the config fence; installation advances
        // it once for the whole bootstrap sequence.
        self.config_revision += 1;
        self.manifest = Some(manifest);
    }

    fn install_completed_manifest(&mut self, manifest: RolloutManifest) {
        self.commands.push(RolloutCommand::InstallManifest {
            expected_config_revision: self.config_revision,
            manifest: manifest.clone(),
        });
        self.config_revision += 1;
        self.manifest = Some(manifest);
    }

    fn plan_legacy(&mut self) -> Result<()> {
        self.lower_authoritative_scenarios()?;
        self.lower_shadow_scenarios()?;
        self.configure_protocol(ProtocolMode::Legacy);
        Ok(())
    }

    fn plan_shadow(&mut self) -> Result<()> {
        self.lower_authoritative_scenarios()?;
        self.configure_protocol(ProtocolMode::Shadow);
        self.converge_manifest_scenarios(false)
    }

    fn plan_authoritative(&mut self) -> Result<()> {
        self.configure_protocol(ProtocolMode::Authoritative);
        self.converge_manifest_scenarios(true)
    }

    fn lower_authoritative_scenarios(&mut self) -> Result<()> {
        for scenario in self.known_scenarios() {
            if self.scenario_mode(&scenario) == ScenarioMode::Authoritative {
                self.change_scenario(&scenario, ScenarioMode::Shadow)?;
            }
        }
        Ok(())
    }

    fn lower_shadow_scenarios(&mut self) -> Result<()> {
        for scenario in self.known_scenarios() {
            if self.scenario_mode(&scenario) == ScenarioMode::Shadow {
                self.change_scenario(&scenario, ScenarioMode::Off)?;
            }
        }
        Ok(())
    }

    fn converge_manifest_scenarios(&mut self, allow_authoritative: bool) -> Result<()> {
        let manifest_classes = self
            .manifest
            .as_ref()
            .map(|manifest| manifest.classes.clone())
            .unwrap_or_default();
        for scenario in self.known_scenarios() {
            let configured = manifest_classes
                .get(&scenario)
                .map(|class| class.configured_mode);
            if configured.is_none() {
                if self.scenario_mode(&scenario) == ScenarioMode::Authoritative {
                    self.change_scenario(&scenario, ScenarioMode::Shadow)?;
                }
                if self.scenario_mode(&scenario) == ScenarioMode::Shadow {
                    self.change_scenario(&scenario, ScenarioMode::Off)?;
                }
                continue;
            }
            if self.scenario_mode(&scenario) == ScenarioMode::Off {
                self.change_scenario(&scenario, ScenarioMode::Shadow)?;
            }
            if allow_authoritative
                && configured == Some(ScenarioMode::Authoritative)
                && self.scenario_mode(&scenario) == ScenarioMode::Shadow
                && manifest_classes
                    .get(&scenario)
                    .is_some_and(|class| rollout_class_evidence_is_complete(&scenario, class))
                && !self.has_hard_blocker(&scenario)
            {
                self.change_scenario(&scenario, ScenarioMode::Authoritative)?;
            } else if !allow_authoritative
                && self.scenario_mode(&scenario) == ScenarioMode::Authoritative
            {
                self.change_scenario(&scenario, ScenarioMode::Shadow)?;
            }
        }
        Ok(())
    }

    fn configure_protocol(&mut self, mode: ProtocolMode) {
        if self.protocol_mode == mode {
            return;
        }
        self.commands.push(RolloutCommand::ConfigureProtocol {
            expected_config_revision: self.config_revision,
            mode,
        });
        self.config_revision += 1;
        self.protocol_mode = mode;
    }

    fn change_scenario(&mut self, scenario: &str, mode: ScenarioMode) -> Result<()> {
        if self.scenario_mode(scenario) == mode {
            return Ok(());
        }
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(|| {
                anyhow!(
                    "cannot reconcile {SCHEDULER_ENV}={} because scenario {scenario} is {:?} without an installed manifest",
                    self.desired.token(),
                    self.scenario_mode(scenario)
                )
            })?;
        self.commands.push(RolloutCommand::ChangeScenarioAuthority {
            scenario_class: scenario.to_string(),
            expected_config_revision: self.config_revision,
            expected_manifest_revision: manifest.revision,
            expected_preflight_revision: manifest.preflight_revision,
            mode,
        });
        self.config_revision += 1;
        self.scenario_modes.insert(scenario.to_string(), mode);
        Ok(())
    }

    fn scenario_mode(&self, scenario: &str) -> ScenarioMode {
        self.scenario_modes
            .get(scenario)
            .copied()
            .unwrap_or(ScenarioMode::Off)
    }

    fn has_hard_blocker(&self, scenario: &str) -> bool {
        let Some(manifest) = &self.manifest else {
            return false;
        };
        self.hard_blockers.iter().any(|blocker| {
            blocker.scenario_class == scenario
                && blocker.manifest_revision == manifest.revision
                && blocker.preflight_revision == manifest.preflight_revision
        })
    }

    fn known_scenarios(&self) -> Vec<String> {
        let mut scenarios = self.scenario_modes.keys().cloned().collect::<Vec<_>>();
        if let Some(manifest) = &self.manifest {
            scenarios.extend(manifest.classes.keys().cloned());
        }
        scenarios.sort();
        scenarios.dedup();
        scenarios
    }

    fn finish(self) -> Vec<(String, RolloutCommand)> {
        self.commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                (
                    format!(
                        "scheduler-env:{}:{}:{}",
                        self.desired.token(),
                        self.initial_config_revision,
                        index
                    ),
                    command,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::domain::scheduler_protocol::{
        RollbackAction, RollbackTrigger, RolloutPreflightRecord, RolloutPreflightState,
        ScenarioAuthority, ScenarioHardBlockerRecord, SchedulerScenarioClass,
    };

    #[test]
    fn desired_mode_parser_is_strict() {
        assert_eq!(desired_mode_from_value(None).unwrap(), None);
        assert_eq!(
            desired_mode_from_value(Some(OsStr::new(" shadow "))).unwrap(),
            Some(SchedulerDesiredMode::Shadow)
        );
        assert!(desired_mode_from_value(Some(OsStr::new("enabled"))).is_err());
    }

    #[test]
    fn unset_desired_mode_preserves_legacy_rollout_control() {
        assert_eq!(desired_mode_for_values(None, None).unwrap(), None);
        assert_eq!(
            desired_mode_for_values(None, Some(OsStr::new("false"))).unwrap(),
            None
        );
        assert_eq!(
            desired_mode_for_values(None, Some(OsStr::new("true"))).unwrap(),
            None
        );
        assert_eq!(
            desired_mode_for_values(Some(OsStr::new("shadow")), Some(OsStr::new("true"))).unwrap(),
            Some(SchedulerDesiredMode::Shadow)
        );
    }

    #[test]
    fn desired_mode_overrides_the_legacy_production_capability() {
        assert!(production_commands_enabled_from_values(
            Some(OsStr::new("authoritative")),
            Some(OsStr::new("false"))
        )
        .unwrap());
        assert!(!production_commands_enabled_from_values(
            Some(OsStr::new("shadow")),
            Some(OsStr::new("true"))
        )
        .unwrap());
        assert!(production_commands_enabled_from_values(None, Some(OsStr::new("true"))).unwrap());
    }

    #[test]
    fn fresh_shadow_bootstraps_only_shadow_authority() {
        let commands = reconciliation_commands_for_state(
            SchedulerDesiredMode::Shadow,
            &RolloutState::default(),
            7,
        )
        .unwrap();

        assert!(commands.iter().any(|(_, command)| matches!(
            command,
            RolloutCommand::InstallManifest { manifest, .. }
                if manifest.classes.values().all(|class| {
                    class.configured_mode == ScenarioMode::Shadow
                        && class.verified_evidence.is_empty()
                })
        )));
        assert!(commands.iter().any(|(_, command)| matches!(
            command,
            RolloutCommand::ConfigureProtocol {
                mode: ProtocolMode::Shadow,
                ..
            }
        )));
        assert_eq!(
            scenario_transitions(&commands, ScenarioMode::Shadow),
            SchedulerScenarioClass::PRODUCTION_AUTHORITY.len()
        );
        assert_eq!(
            scenario_transitions(&commands, ScenarioMode::Authoritative),
            0
        );
    }

    #[test]
    fn fresh_authoritative_stays_shadow_without_approved_evidence() {
        let commands = reconciliation_commands_for_state(
            SchedulerDesiredMode::Authoritative,
            &RolloutState::default(),
            7,
        )
        .unwrap();

        assert!(commands.iter().any(|(_, command)| matches!(
            command,
            RolloutCommand::ConfigureProtocol {
                mode: ProtocolMode::Authoritative,
                ..
            }
        )));
        assert_eq!(
            scenario_transitions(&commands, ScenarioMode::Shadow),
            SchedulerScenarioClass::PRODUCTION_AUTHORITY.len()
        );
        assert_eq!(
            scenario_transitions(&commands, ScenarioMode::Authoritative),
            0
        );
    }

    #[test]
    fn authoritative_promotes_only_manifest_approved_classes() {
        let mut rollout = approved_authoritative_rollout();
        let excluded = SchedulerScenarioClass::Delivery.as_str();
        rollout
            .manifest
            .as_mut()
            .expect("manifest")
            .classes
            .get_mut(excluded)
            .expect("class")
            .configured_mode = ScenarioMode::Shadow;

        let commands =
            reconciliation_commands_for_state(SchedulerDesiredMode::Authoritative, &rollout, 7)
                .unwrap();

        assert_eq!(
            scenario_transitions(&commands, ScenarioMode::Authoritative),
            SchedulerScenarioClass::PRODUCTION_AUTHORITY.len() - 1
        );
        assert!(!commands.iter().any(|(_, command)| matches!(
            command,
            RolloutCommand::ChangeScenarioAuthority {
                scenario_class,
                mode: ScenarioMode::Authoritative,
                ..
            } if scenario_class == excluded
        )));
    }

    #[test]
    fn authoritative_does_not_promote_a_hard_blocked_class() {
        let mut rollout = approved_authoritative_rollout();
        let blocked = SchedulerScenarioClass::ExactWaitResume.as_str();
        rollout.hard_blockers.insert(ScenarioHardBlockerRecord {
            scenario_class: blocked.to_string(),
            blocker_code: "stale_wait_generation_accepted".into(),
            config_revision: 0,
            manifest_revision: 1,
            preflight_revision: 1,
            trigger: RollbackTrigger::AnyHardBlocker,
            action: RollbackAction::StopAdmissionsAndRevert {
                target: ScenarioMode::Shadow,
            },
        });

        let commands =
            reconciliation_commands_for_state(SchedulerDesiredMode::Authoritative, &rollout, 7)
                .unwrap();

        assert!(!commands.iter().any(|(_, command)| matches!(
            command,
            RolloutCommand::ChangeScenarioAuthority {
                scenario_class,
                mode: ScenarioMode::Authoritative,
                ..
            } if scenario_class == blocked
        )));
    }

    #[test]
    fn legacy_downgrade_is_ordered_and_idempotent() {
        let mut rollout = approved_authoritative_rollout();
        rollout.protocol_mode = ProtocolMode::Authoritative;
        let manifest = rollout.manifest.as_ref().expect("manifest");
        for scenario in SchedulerScenarioClass::PRODUCTION_AUTHORITY {
            rollout.scenarios.insert(
                scenario.as_str().to_string(),
                ScenarioAuthority {
                    mode: ScenarioMode::Authoritative,
                    rollback_target: ScenarioMode::Shadow,
                    manifest_revision: Some(manifest.revision),
                    preflight_revision: Some(manifest.preflight_revision),
                },
            );
        }

        let commands =
            reconciliation_commands_for_state(SchedulerDesiredMode::Legacy, &rollout, 7).unwrap();
        let modes = commands
            .iter()
            .filter_map(|(_, command)| match command {
                RolloutCommand::ChangeScenarioAuthority { mode, .. } => Some(*mode),
                RolloutCommand::ConfigureProtocol { mode, .. } => {
                    assert_eq!(*mode, ProtocolMode::Legacy);
                    None
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            modes,
            [ScenarioMode::Shadow, ScenarioMode::Off]
                .into_iter()
                .flat_map(|mode| {
                    std::iter::repeat_n(mode, SchedulerScenarioClass::PRODUCTION_AUTHORITY.len())
                })
                .collect::<Vec<_>>()
        );

        rollout.protocol_mode = ProtocolMode::Legacy;
        for authority in rollout.scenarios.values_mut() {
            authority.mode = ScenarioMode::Off;
            authority.manifest_revision = None;
            authority.preflight_revision = None;
        }
        assert!(
            reconciliation_commands_for_state(SchedulerDesiredMode::Legacy, &rollout, 7)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn legacy_downgrade_reports_missing_manifest_without_panicking() {
        let rollout = RolloutState {
            scenarios: BTreeMap::from([(
                SchedulerScenarioClass::ExactWaitResume.as_str().to_string(),
                ScenarioAuthority {
                    mode: ScenarioMode::Shadow,
                    rollback_target: ScenarioMode::Off,
                    manifest_revision: None,
                    preflight_revision: None,
                },
            )]),
            ..RolloutState::default()
        };

        let error = reconciliation_commands_for_state(SchedulerDesiredMode::Legacy, &rollout, 7)
            .unwrap_err();
        assert!(error.to_string().contains("without an installed manifest"));
    }

    #[test]
    fn open_preflight_fails_with_actionable_diagnostic() {
        let mut rollout = RolloutState {
            latest_preflight_revision: 1,
            ..RolloutState::default()
        };
        rollout.preflights.insert(
            1,
            RolloutPreflightRecord {
                revision: 1,
                manifest_revision: 1,
                state: RolloutPreflightState::Open,
                manifest: None,
            },
        );

        let error = reconciliation_commands_for_state(SchedulerDesiredMode::Shadow, &rollout, 7)
            .unwrap_err();
        assert!(error.to_string().contains("rollout preflight 1 is open"));
    }

    #[test]
    fn completed_preflight_is_installed_without_reopening() {
        let manifest = managed_shadow_rollout_manifest(1, 1, "build".into(), "schema".into(), 7);
        let rollout = RolloutState {
            latest_preflight_revision: 1,
            preflights: BTreeMap::from([(
                1,
                RolloutPreflightRecord {
                    revision: 1,
                    manifest_revision: 1,
                    state: RolloutPreflightState::Completed,
                    manifest: Some(manifest.clone()),
                },
            )]),
            ..RolloutState::default()
        };

        let commands =
            reconciliation_commands_for_state(SchedulerDesiredMode::Shadow, &rollout, 7).unwrap();
        assert!(matches!(
            commands.first(),
            Some((
                _,
                RolloutCommand::InstallManifest {
                    manifest: installed,
                    ..
                }
            )) if installed == &manifest
        ));
        assert!(!commands.iter().any(|(_, command)| matches!(
            command,
            RolloutCommand::OpenPreflight { .. } | RolloutCommand::CompletePreflight { .. }
        )));
    }

    #[test]
    fn runtime_db_reconciliation_is_restart_idempotent() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        let schema_revision = u64::try_from(db.current_schema_version()?)?;

        let rollout = db.transitions().load_scheduler_rollout_state()?;
        let commands = reconciliation_commands_for_state(
            SchedulerDesiredMode::Authoritative,
            &rollout,
            schema_revision,
        )?;
        db.apply_scheduler_rollout_commands(&commands)?;

        let restarted = RuntimeDb::open_and_migrate(
            dir.path().join("state/runtime.sqlite"),
            dir.path().join("state/runtime.lock"),
        )?;
        let rollout = restarted.transitions().load_scheduler_rollout_state()?;
        assert_eq!(rollout.protocol_mode, ProtocolMode::Authoritative);
        assert!(rollout
            .scenarios
            .values()
            .all(|authority| authority.mode == ScenarioMode::Shadow));
        assert!(reconciliation_commands_for_state(
            SchedulerDesiredMode::Authoritative,
            &rollout,
            schema_revision
        )?
        .is_empty());
        Ok(())
    }

    fn scenario_transitions(
        commands: &[(String, RolloutCommand)],
        expected: ScenarioMode,
    ) -> usize {
        commands
            .iter()
            .filter(|(_, command)| {
                matches!(
                    command,
                    RolloutCommand::ChangeScenarioAuthority { mode, .. } if *mode == expected
                )
            })
            .count()
    }

    fn approved_authoritative_rollout() -> RolloutState {
        let mut manifest =
            managed_shadow_rollout_manifest(1, 1, "build".into(), "schema".into(), 7);
        for class in manifest.classes.values_mut() {
            class.configured_mode = ScenarioMode::Authoritative;
            class.observed_shadow_samples = class.minimum_shadow_samples;
            class.observed_shadow_duration_secs = class.minimum_shadow_duration_secs;
            class.verified_evidence = class.required_evidence.clone();
        }
        RolloutState {
            latest_preflight_revision: 1,
            preflights: BTreeMap::from([(
                1,
                RolloutPreflightRecord {
                    revision: 1,
                    manifest_revision: 1,
                    state: RolloutPreflightState::Consumed,
                    manifest: Some(manifest.clone()),
                },
            )]),
            manifest: Some(manifest),
            hard_blockers: BTreeSet::new(),
            ..RolloutState::default()
        }
    }
}

use super::*;
use chrono::{DateTime, Utc};
use std::{collections::BTreeMap, sync::Mutex};
use uuid::Uuid;

const USER_GLOBAL_LIBRARY_LABEL: &str = "user_global";

#[derive(Clone, Default)]
pub struct JobRegistry {
    // TODO: retain terminal jobs for a bounded window before evicting them.
    jobs: Arc<Mutex<BTreeMap<String, JobSnapshot>>>,
}

impl JobRegistry {
    pub(super) fn insert(&self, job: JobSnapshot) {
        self.jobs
            .lock()
            .expect("job registry lock")
            .insert(job.id.clone(), job);
    }

    fn get(&self, id: &str) -> Option<JobSnapshot> {
        self.jobs
            .lock()
            .expect("job registry lock")
            .get(id)
            .cloned()
    }

    pub(super) fn update(&self, id: &str, update: impl FnOnce(&mut JobSnapshot)) {
        let mut jobs = self.jobs.lock().expect("job registry lock");
        if let Some(job) = jobs.get_mut(id) {
            update(job);
            job.updated_at = Utc::now();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JobSnapshot {
    pub id: String,
    pub kind: String,
    pub status: JobStatus,
    pub phase: String,
    pub progress: JobProgress,
    pub summary: String,
    pub items: Vec<JobItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct JobProgress {
    pub current: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobItem {
    pub id: String,
    pub status: JobStatus,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CreateJobRequest {
    #[serde(rename = "skill.install")]
    SkillInstall {
        params: crate::types::AddSkillRequest,
    },
    #[serde(rename = "agent_template.remote_sources.sync")]
    AgentTemplateRemoteSourcesSync {
        params: crate::types::SyncTemplateRemoteSourcesRequest,
    },
}

pub async fn create_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateJobRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    match request {
        CreateJobRequest::SkillInstall { params } => create_skill_install_job(state, params)
            .await
            .map(IntoResponse::into_response),
        CreateJobRequest::AgentTemplateRemoteSourcesSync { params } => {
            create_template_remote_source_sync_job(state, params)
                .await
                .map(IntoResponse::into_response)
        }
    }
}

pub async fn job_status(
    Path(job_id): Path<String>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_remote_access(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    match state.jobs.get(&job_id) {
        Some(job) => Ok(Json(json!({ "ok": true, "job": job }))),
        None => Err(http_error(
            StatusCode::NOT_FOUND,
            HttpErrorEnvelope::new(format!("job {job_id} was not found")).code("job_not_found"),
        )),
    }
}

async fn create_skill_install_job(
    state: Arc<AppState>,
    request: crate::types::AddSkillRequest,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let id = format!("job_{}", Uuid::new_v4().simple());
    let now = Utc::now();
    let job = JobSnapshot {
        id: id.clone(),
        kind: "skill.install".into(),
        status: JobStatus::Queued,
        phase: "queued".into(),
        progress: JobProgress::default(),
        summary: "Queued skill install job".into(),
        items: Vec::new(),
        result: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    state.jobs.insert(job.clone());

    let jobs = state.jobs.clone();
    let skill_install_jobs = state.skill_install_jobs.clone();
    let user_home = crate::agent_template::user_home_dir().map_err(error_response)?;
    let job_id = id.clone();
    tokio::spawn(async move {
        jobs.update(&job_id, |job| {
            job.status = JobStatus::Running;
            job.phase = "installing".into();
            job.summary = "Installing skill into the Global Skill Library".into();
            job.progress.total = 1;
            job.items = vec![JobItem {
                id: "skill.install".into(),
                status: JobStatus::Running,
                summary: skill_install_summary(&request.kind),
                error: None,
            }];
        });
        let permit = match skill_install_jobs.acquire_owned().await {
            Ok(permit) => permit,
            Err(error) => {
                let message = format!("skill install queue closed: {error}");
                jobs.update(&job_id, |job| {
                    job.status = JobStatus::Failed;
                    job.phase = "failed".into();
                    job.progress.current = 1;
                    job.summary = "Skill install queue closed".into();
                    job.error = Some(message.clone());
                    job.items = vec![JobItem {
                        id: "skill.install".into(),
                        status: JobStatus::Failed,
                        summary: "Skill install queue closed".into(),
                        error: Some(message),
                    }];
                });
                return;
            }
        };
        let kind = request.kind.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::skills::add_library_skill(&user_home, &kind)
        })
        .await;
        match result {
            Ok(Ok(skill_name)) => jobs.update(&job_id, |job| {
                job.status = JobStatus::Completed;
                job.phase = "completed".into();
                job.progress.current = 1;
                job.summary = format!("Installed {skill_name} to the Global Skill Library");
                job.items = vec![JobItem {
                    id: skill_name.clone(),
                    status: JobStatus::Completed,
                    summary: format!("Installed {skill_name}"),
                    error: None,
                }];
                job.result = Some(json!({
                    "skill_name": skill_name,
                    "library": USER_GLOBAL_LIBRARY_LABEL,
                }));
            }),
            Ok(Err(error)) => {
                let message = error.to_string();
                jobs.update(&job_id, |job| {
                    job.status = JobStatus::Failed;
                    job.phase = "failed".into();
                    job.progress.current = 1;
                    job.summary = "Skill install failed".into();
                    job.error = Some(message.clone());
                    job.items = vec![JobItem {
                        id: "skill.install".into(),
                        status: JobStatus::Failed,
                        summary: "Skill install failed".into(),
                        error: Some(message),
                    }];
                });
            }
            Err(error) => {
                let message = format!("skill install worker failed: {error}");
                jobs.update(&job_id, |job| {
                    job.status = JobStatus::Failed;
                    job.phase = "failed".into();
                    job.progress.current = 1;
                    job.summary = "Skill install worker failed".into();
                    job.error = Some(message.clone());
                    job.items = vec![JobItem {
                        id: "skill.install".into(),
                        status: JobStatus::Failed,
                        summary: "Skill install worker failed".into(),
                        error: Some(message),
                    }];
                });
            }
        }
    });

    Ok(((
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "job": job,
        })),
    ))
        .into_response())
}

pub(super) async fn create_template_remote_source_sync_job(
    state: Arc<AppState>,
    request: crate::types::SyncTemplateRemoteSourcesRequest,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let id = format!("job_{}", Uuid::new_v4().simple());
    let now = Utc::now();
    let job = JobSnapshot {
        id: id.clone(),
        kind: "agent_template.remote_sources.sync".into(),
        status: JobStatus::Queued,
        phase: "queued".into(),
        progress: JobProgress::default(),
        summary: "Queued agent template remote source sync job".into(),
        items: Vec::new(),
        result: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    state.jobs.insert(job.clone());

    let jobs = state.jobs.clone();
    let sync_jobs = state.template_remote_source_sync_jobs.clone();
    let db = state.host.runtime_db().clone();
    let configured_sources = crate::agent_template::effective_agent_template_remote_sources(
        &state.host.config().stored_config.agent_templates,
    );
    let job_id = id.clone();
    tokio::spawn(async move {
        let selected_sources = configured_sources
            .iter()
            .filter(|(source_id, config)| {
                request
                    .source_id
                    .as_ref()
                    .is_none_or(|requested| requested == *source_id)
                    && config.enabled()
            })
            .map(|(source_id, config)| (source_id.clone(), config.clone()))
            .collect::<Vec<_>>();

        if request.source_id.is_some() && selected_sources.is_empty() {
            let message = format!(
                "agent template remote source {} was not found or is disabled",
                request.source_id.as_deref().unwrap_or_default()
            );
            jobs.update(&job_id, |job| {
                job.status = JobStatus::Failed;
                job.phase = "failed".into();
                job.summary = "Remote source sync failed".into();
                job.error = Some(message.clone());
                job.items = vec![JobItem {
                    id: request.source_id.clone().unwrap_or_default(),
                    status: JobStatus::Failed,
                    summary: "Remote source was not found or is disabled".into(),
                    error: Some(message),
                }];
            });
            return;
        }

        jobs.update(&job_id, |job| {
            job.status = JobStatus::Running;
            job.phase = "syncing".into();
            job.summary = "Synchronizing agent template remote sources".into();
            job.progress.total = selected_sources.len();
            job.items = selected_sources
                .iter()
                .map(|(source_id, _)| JobItem {
                    id: source_id.clone(),
                    status: JobStatus::Queued,
                    summary: format!("Queued sync for {source_id}"),
                    error: None,
                })
                .collect();
        });

        let permit = match sync_jobs.acquire_owned().await {
            Ok(permit) => permit,
            Err(error) => {
                let message = format!("remote source sync queue closed: {error}");
                jobs.update(&job_id, |job| {
                    job.status = JobStatus::Failed;
                    job.phase = "failed".into();
                    job.summary = "Remote source sync queue closed".into();
                    job.error = Some(message.clone());
                    for item in &mut job.items {
                        item.status = JobStatus::Failed;
                        item.error = Some(message.clone());
                    }
                });
                return;
            }
        };
        let _permit = permit;

        let user_home = match crate::agent_template::user_home_dir() {
            Ok(path) => path,
            Err(error) => {
                let message = error.to_string();
                jobs.update(&job_id, |job| {
                    job.status = JobStatus::Failed;
                    job.phase = "failed".into();
                    job.summary = "Remote source sync failed".into();
                    job.error = Some(message.clone());
                    for item in &mut job.items {
                        item.status = JobStatus::Failed;
                        item.error = Some(message.clone());
                    }
                });
                return;
            }
        };

        let mut completed = 0usize;
        let mut failed = 0usize;
        let mut source_results = Vec::new();
        for (source_id, config) in selected_sources {
            jobs.update(&job_id, |job| {
                if let Some(item) = job.items.iter_mut().find(|item| item.id == source_id) {
                    item.status = JobStatus::Running;
                    item.summary = format!("Syncing {source_id}");
                    item.error = None;
                }
            });
            match crate::agent_template::sync_agent_template_remote_source(
                &db, &user_home, &source_id, &config,
            )
            .await
            {
                Ok(status) => {
                    completed += 1;
                    source_results.push(json!(status));
                    jobs.update(&job_id, |job| {
                        job.progress.current = completed + failed;
                        if let Some(item) = job.items.iter_mut().find(|item| item.id == source_id) {
                            item.status = JobStatus::Completed;
                            item.summary = format!("Synced {source_id}");
                            item.error = None;
                        }
                    });
                }
                Err(error) => {
                    failed += 1;
                    let mut message = error.to_string();
                    if let Err(record_error) =
                        crate::agent_template::record_agent_template_remote_source_sync_failure(
                            &db, &source_id, &config, &message,
                        )
                    {
                        message = format!(
                            "{message}; additionally failed to persist sync failure: {record_error}"
                        );
                    }
                    jobs.update(&job_id, |job| {
                        job.progress.current = completed + failed;
                        if let Some(item) = job.items.iter_mut().find(|item| item.id == source_id) {
                            item.status = JobStatus::Failed;
                            item.summary = format!("Failed to sync {source_id}");
                            item.error = Some(message.clone());
                        }
                    });
                }
            }
        }

        jobs.update(&job_id, |job| {
            if failed == 0 {
                job.status = JobStatus::Completed;
                job.phase = "completed".into();
                job.summary = format!("Synced {completed} agent template remote source(s)");
            } else {
                job.status = JobStatus::Failed;
                job.phase = "failed".into();
                job.summary =
                    format!("Synced {completed} agent template remote source(s), {failed} failed");
                job.error = Some(job.summary.clone());
            }
            job.result = Some(json!({
                "sources": source_results,
                "completed": completed,
                "failed": failed,
            }));
        });
    });

    Ok(((
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "job": job,
        })),
    ))
        .into_response())
}

pub(super) fn create_codex_device_login_job(
    state: Arc<AppState>,
    device_code: crate::auth::CodexDeviceCode,
) -> JobSnapshot {
    let id = format!("job_{}", Uuid::new_v4().simple());
    let now = Utc::now();
    let job = JobSnapshot {
        id: id.clone(),
        kind: "auth.codex.device_login".into(),
        status: JobStatus::Queued,
        phase: "waiting_for_user".into(),
        progress: JobProgress {
            current: 1,
            total: 3,
        },
        summary: "Waiting for OpenAI Codex device authorization".into(),
        items: vec![JobItem {
            id: "device_authorization".into(),
            status: JobStatus::Running,
            summary: "Waiting for user to authorize the OpenAI Codex device code".into(),
            error: None,
        }],
        result: None,
        error: None,
        created_at: now,
        updated_at: now,
    };
    state.jobs.insert(job.clone());

    let jobs = state.jobs.clone();
    let host = state.host.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        jobs.update(&job_id, |job| {
            job.status = JobStatus::Running;
        });
        match crate::auth::complete_codex_device_login(device_code).await {
            Ok(login) => {
                jobs.update(&job_id, |job| {
                    job.phase = "persisting_credentials".into();
                    job.progress.current = 2;
                    job.summary = "Persisting OpenAI Codex OAuth credential".into();
                    job.items = vec![JobItem {
                        id: "credential_profile".into(),
                        status: JobStatus::Running,
                        summary: "Writing openai-codex credential profile".into(),
                        error: None,
                    }];
                });
                let config = host.config();
                let profile = crate::config::OPENAI_CODEX_CREDENTIAL_PROFILE;
                let credential_path = credential_store_path(&config.home_dir);
                match set_credential_profile_at(
                    &credential_path,
                    profile,
                    CredentialKind::OAuth,
                    login.material,
                ) {
                    Ok(profile_status) => {
                        if let Err(error) = host.reload_all_agents_config().await {
                            tracing::warn!(
                                error = %error,
                                "OpenAI Codex device credential saved but hot-reload failed; restart needed"
                            );
                        }
                        jobs.update(&job_id, |job| {
                            job.status = JobStatus::Completed;
                            job.phase = "completed".into();
                            job.progress.current = 3;
                            job.summary = "OpenAI Codex OAuth login completed".into();
                            job.items = vec![JobItem {
                                id: "credential_profile".into(),
                                status: JobStatus::Completed,
                                summary: "Configured openai-codex OAuth credential profile".into(),
                                error: None,
                            }];
                            job.result = Some(json!({
                                "provider_id": crate::config::ProviderId::OPENAI_CODEX,
                                "profile": profile_status.profile,
                                "auth_kind": profile_status.kind,
                                "configured": true,
                                "account_id": login.account_id,
                            }));
                        });
                    }
                    Err(error) => fail_codex_device_login_job(
                        &jobs,
                        &job_id,
                        "OpenAI Codex credential persist failed",
                        error.to_string(),
                    ),
                }
            }
            Err(error) => fail_codex_device_login_job(
                &jobs,
                &job_id,
                "OpenAI Codex device login failed",
                error.to_string(),
            ),
        }
    });

    job
}

fn fail_codex_device_login_job(jobs: &JobRegistry, job_id: &str, summary: &str, message: String) {
    jobs.update(job_id, |job| {
        job.status = JobStatus::Failed;
        job.phase = "failed".into();
        job.progress.current = job.progress.total;
        job.summary = summary.into();
        job.error = Some(message.clone());
        job.items = vec![JobItem {
            id: "auth.codex.device_login".into(),
            status: JobStatus::Failed,
            summary: summary.into(),
            error: Some(message),
        }];
    });
}

fn skill_install_summary(kind: &crate::types::SkillInstallKind) -> String {
    match kind {
        crate::types::SkillInstallKind::Named { name, .. } => format!("Install named skill {name}"),
        crate::types::SkillInstallKind::Local { path, .. } => {
            format!("Install local skill {}", path.display())
        }
        crate::types::SkillInstallKind::Remote { package, skill, .. } => match skill {
            Some(skill) => format!("Install remote skill {package}@{skill}"),
            None => format!("Install remote skill package {package}"),
        },
    }
}

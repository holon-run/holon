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
}

pub async fn create_job(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CreateJobRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    authorize_control(&headers, &state).map_err(|err| auth_required(err.to_string()))?;
    match request {
        CreateJobRequest::SkillInstall { params } => create_skill_install_job(state, params).await,
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
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
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

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "job": job,
        })),
    ))
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
        crate::types::SkillInstallKind::Builtin { name } => format!("Install builtin skill {name}"),
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

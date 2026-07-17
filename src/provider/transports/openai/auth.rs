use super::*;

impl OpenAiBearerAuth {
    pub(crate) fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        client: Client,
    ) -> Result<Self> {
        let oauth_profile = matches!(
            (provider_config.auth.source, provider_config.auth.kind),
            (CredentialSource::AuthProfile, CredentialKind::OAuth)
        );
        let api_key = match (provider_config.auth.source, provider_config.auth.kind) {
            (CredentialSource::AuthProfile, CredentialKind::OAuth) => None,
            (CredentialSource::None, CredentialKind::None) => None,
            _ => Some(
                provider_config
                    .credential
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        let credential_name = provider_config
                            .auth
                            .env
                            .as_deref()
                            .or(provider_config.auth.profile.as_deref())
                            .or(provider_config.auth.external.as_deref())
                            .unwrap_or("configured credential");
                        anyhow::anyhow!("missing {credential_name}")
                    })?,
            ),
        };
        Ok(Self {
            client,
            provider_id: provider_config.id.as_str().to_string(),
            api_key,
            credential_profile: oauth_profile.then(|| {
                provider_config
                    .auth
                    .profile
                    .clone()
                    .unwrap_or_else(|| provider_config.id.as_str().to_string())
            }),
            credential_material: oauth_profile
                .then(|| provider_config.credential.clone())
                .flatten(),
            credential_store_path: provider_config.credential_store_path.clone(),
        })
    }

    fn resolve_oauth_credential(&self) -> Result<Option<OAuthCredential>> {
        let Some(profile) = self.credential_profile.as_deref() else {
            return Ok(None);
        };
        let material = self
            .credential_material
            .as_deref()
            .filter(|material| !material.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing OAuth credential profile {profile}"))?;
        load_oauth_profile_credential(material, profile).map(Some)
    }

    pub(crate) async fn resolve_authorization_header(&self) -> Result<Option<String>> {
        if let Some(api_key) = self.api_key.as_ref() {
            return Ok(Some(format!("Bearer {api_key}")));
        }
        let Some(credential) = self.resolve_oauth_credential()? else {
            return Ok(None);
        };
        let credential = if credential
            .expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now() + chrono::Duration::hours(1))
        {
            self.refresh_oauth_profile(false).await?
        } else {
            credential
        };
        Ok(Some(format!("Bearer {}", credential.access_token)))
    }

    pub(super) async fn resolve_auth_headers(&self) -> Result<Vec<(&'static str, String)>> {
        Ok(self
            .resolve_authorization_header()
            .await?
            .into_iter()
            .map(|value| ("authorization", value))
            .collect())
    }

    async fn refresh_oauth_profile(&self, force: bool) -> Result<OAuthCredential> {
        let profile = self
            .credential_profile
            .as_deref()
            .context("provider does not use an OAuth credential profile")?;
        let config = oauth_provider_config(&self.provider_id).ok_or_else(|| {
            anyhow::anyhow!("provider {} has no OAuth configuration", self.provider_id)
        })?;
        let store_path = self.credential_store_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "Holon-managed OAuth profile {profile} cannot refresh because the credential store path is unavailable"
            )
        })?;
        let lock_path = store_path.with_extension("json.lock");
        let _lock = CredentialStoreRefreshLock::acquire(&lock_path)?;
        let mut store = load_credential_store_at(store_path)?;
        let entry = store.profiles.get(profile).cloned().ok_or_else(|| {
            anyhow::anyhow!("Holon credential profile {profile} disappeared before refresh")
        })?;
        if entry.kind != CredentialKind::OAuth {
            anyhow::bail!(
                "Holon credential profile {profile} has kind {}, but OAuth refresh requires oauth",
                entry.kind.as_str()
            );
        }
        let current = load_oauth_profile_credential(&entry.material, profile)?;
        if !force
            && !current
                .expires_at
                .is_some_and(|expires_at| expires_at <= Utc::now() + chrono::Duration::hours(1))
        {
            return Ok(current);
        }
        let refreshed =
            refresh_oauth_profile_material(&self.client, &config, &entry.material, profile)
                .await
                .map_err(|failure| codex_refresh_error(profile, failure))?;
        store.profiles.insert(
            profile.to_string(),
            CredentialProfileFile {
                kind: CredentialKind::OAuth,
                material: refreshed.material,
            },
        );
        save_credential_store_at(store_path, &store)?;
        Ok(refreshed.credential)
    }

    pub(crate) async fn refresh_authorization_header(&self) -> Result<Option<String>> {
        if self.credential_profile.is_none() {
            return Ok(None);
        }
        let credential = self.refresh_oauth_profile(true).await?;
        Ok(Some(format!("Bearer {}", credential.access_token)))
    }

    pub(super) async fn refresh_auth_headers(&self) -> Result<Option<Vec<(&'static str, String)>>> {
        Ok(self
            .refresh_authorization_header()
            .await?
            .map(|value| vec![("authorization", value)]))
    }
}

impl OpenAiCodexProvider {
    pub fn from_config(config: &AppConfig, model: &str) -> Result<Self> {
        let provider_config = config
            .providers
            .get(&ProviderId::openai_codex())
            .ok_or_else(|| anyhow::anyhow!("missing openai-codex provider config"))?;
        let policy = openai_model_policy_from_config(config, ProviderId::openai_codex(), model);
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            &config.home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
            policy.verbosity,
            policy.capabilities.supports_reasoning,
        )
    }

    pub fn from_runtime_config(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        supports_reasoning: bool,
    ) -> Result<Self> {
        let policy =
            openai_model_policy_for_runtime_config(provider_config, model, max_output_tokens);
        Self::from_runtime_config_with_compaction_policy(
            provider_config,
            model,
            policy.runtime_max_output_tokens,
            trace_home_dir,
            OpenAiCompactionPolicy {
                trigger_input_tokens: policy.compaction_trigger_estimated_tokens as u64,
            },
            policy.verbosity,
            supports_reasoning,
        )
    }

    pub(crate) fn from_runtime_config_with_compaction_policy(
        provider_config: &ProviderRuntimeConfig,
        model: &str,
        max_output_tokens: u32,
        trace_home_dir: &Path,
        compaction_policy: OpenAiCompactionPolicy,
        verbosity: Option<ModelVerbosity>,
        supports_reasoning: bool,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let codex_home = provider_config
            .codex_home
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing codex_home for OpenAI Codex provider"))?;
        resolve_openai_codex_credential(provider_config, &codex_home)?;
        Ok(Self {
            client,
            provider_id: provider_config.id.as_str().to_string(),
            base_url: provider_config.base_url.trim_end_matches('/').to_string(),
            credential_profile: provider_config.auth.profile.clone(),
            credential_material: provider_config.credential.clone(),
            credential_external: provider_config.auth.external.clone(),
            credential_store_path: provider_config.credential_store_path.clone(),
            codex_home,
            originator: provider_config
                .originator
                .clone()
                .unwrap_or_else(|| "codex_cli_rs".into()),
            model: model.to_string(),
            max_output_tokens,
            reasoning_effort: provider_config.reasoning_effort.clone(),
            supports_reasoning,
            verbosity,
            builtin_web_search: provider_config.builtin_web_search.clone(),
            compaction_policy,
            trace_home_dir: trace_home_dir.to_path_buf(),
            continuation: Arc::new(Mutex::new(OpenAiContinuationState::default())),
        })
    }

    fn resolve_credentials(
        &self,
    ) -> Result<(Option<CodexCliCredential>, Option<CodexCliCredential>)> {
        let profile = self
            .credential_material
            .as_deref()
            .filter(|material| !material.trim().is_empty())
            .map(|material| {
                load_codex_oauth_profile_credential(
                    material,
                    self.credential_profile.as_deref().unwrap_or("openai-codex"),
                )
            })
            .transpose()?;
        let cli = if self.credential_external.as_deref() == Some("codex_cli") || profile.is_none() {
            load_codex_cli_credential(&self.codex_home).ok()
        } else {
            None
        };
        Ok((profile, cli))
    }

    pub(super) async fn resolve_fresh_credential(&self) -> Result<CodexCliCredential> {
        let (profile, cli) = self.resolve_credentials()?;
        let Some(profile) = profile else {
            return cli.ok_or_else(|| {
                anyhow::anyhow!(
                    "no Holon openai-codex credential profile or usable Codex CLI credentials are available"
                )
            });
        };
        if !credential_needs_refresh(&profile) {
            return Ok(profile);
        }
        match self.refresh_holon_oauth_profile(false).await {
            Ok(refreshed) => Ok(refreshed),
            Err(error) if cli.is_some() => {
                tracing::warn!(
                    %error,
                    credential_profile = self
                        .credential_profile
                        .as_deref()
                        .unwrap_or("openai-codex"),
                    "OpenAI Codex profile refresh failed; falling back to Codex CLI credential"
                );
                Ok(cli.expect("CLI fallback was checked above"))
            }
            Err(error) => Err(error),
        }
    }

    async fn refresh_holon_oauth_profile(&self, force: bool) -> Result<CodexCliCredential> {
        let profile = self.credential_profile.as_deref().unwrap_or("openai-codex");
        let store_path = self.credential_store_path.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "OpenAI Codex Holon-managed OAuth profile {profile} cannot refresh because the credential store path is unavailable"
            )
        })?;
        let lock_path = store_path.with_extension("json.lock");
        let _lock = CredentialStoreRefreshLock::acquire(&lock_path)?;
        let mut store = load_credential_store_at(store_path)?;
        let entry = store.profiles.get(profile).cloned().ok_or_else(|| {
            anyhow::anyhow!("Holon credential profile {profile} disappeared before refresh")
        })?;
        if entry.kind != CredentialKind::OAuth {
            anyhow::bail!(
                "Holon credential profile {profile} has kind {}, but OpenAI Codex refresh requires oauth",
                entry.kind.as_str()
            );
        }
        let current = load_codex_oauth_profile_credential(&entry.material, profile)?;
        if !force && !credential_needs_refresh(&current) {
            return Ok(current);
        }
        let refreshed =
            refresh_codex_oauth_profile_material(&self.client, &entry.material, profile)
                .await
                .map_err(|failure| codex_refresh_error(profile, failure))?;
        store.profiles.insert(
            profile.to_string(),
            CredentialProfileFile {
                kind: CredentialKind::OAuth,
                material: refreshed.material,
            },
        );
        save_credential_store_at(store_path, &store)?;
        Ok(refreshed.credential)
    }

    pub(super) async fn refresh_after_auth_failure(
        &self,
        credential: &CodexCliCredential,
    ) -> Result<Option<CodexCliCredential>> {
        if !credential.source.starts_with("credential_profile:") {
            return Ok(None);
        }
        self.refresh_holon_oauth_profile(true).await.map(Some)
    }
}

pub(super) struct CredentialStoreRefreshLock {
    path: PathBuf,
}

impl CredentialStoreRefreshLock {
    pub(super) fn acquire(path: &Path) -> Result<Self> {
        match Self::try_acquire(path) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create credential refresh lock {}",
                        path.display()
                    )
                });
            }
        }
        if Self::is_stale(path)? {
            std::fs::remove_file(path).with_context(|| {
                format!(
                    "failed to remove stale credential refresh lock {}",
                    path.display()
                )
            })?;
            return Self::try_acquire(path).map_err(|error| Self::acquire_error(path, error));
        }
        Self::try_acquire(path).map_err(|error| Self::acquire_error(path, error))
    }

    fn try_acquire(path: &Path) -> std::io::Result<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).map(|_| Self {
            path: path.to_path_buf(),
        })
    }

    fn acquire_error(path: &Path, error: std::io::Error) -> anyhow::Error {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!(
                "OpenAI Codex OAuth refresh is already in progress for this credential store; retry shortly"
            )
        } else {
            anyhow::Error::new(error).context(format!(
                "failed to create credential refresh lock {}",
                path.display()
            ))
        }
    }

    fn is_stale(path: &Path) -> Result<bool> {
        const STALE_LOCK_AFTER: Duration = Duration::from_secs(10 * 60);
        match std::fs::OpenOptions::new()
            .read(true)
            .open(path)
            .and_then(|file| file.metadata())
        {
            Ok(metadata) => Ok(metadata
                .modified()
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .map(|elapsed| elapsed >= STALE_LOCK_AFTER)
                .unwrap_or(false)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to inspect credential refresh lock {}",
                    path.display()
                )
            }),
        }
    }
}

impl Drop for CredentialStoreRefreshLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn credential_needs_refresh(credential: &CodexCliCredential) -> bool {
    credential
        .expires_at
        .map(|expires_at| expires_at <= Utc::now() + chrono::Duration::minutes(5))
        .unwrap_or(false)
}

pub(super) fn choose_openai_codex_credential(
    profile: Option<CodexCliCredential>,
    cli: Option<CodexCliCredential>,
) -> Option<CodexCliCredential> {
    profile.or(cli)
}

pub(super) fn openai_codex_headers(
    credential: &CodexCliCredential,
    originator: &str,
) -> Vec<(&'static str, String)> {
    vec![
        (
            "authorization",
            format!("Bearer {}", credential.access_token),
        ),
        ("chatgpt-account-id", credential.account_id.clone()),
        ("OpenAI-Beta", "responses=experimental".to_string()),
        ("originator", originator.to_string()),
    ]
}

pub(super) fn is_openai_codex_auth_status_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProviderTransportError>()
        .is_some_and(|error| {
            error.classification.kind == ProviderFailureKind::AuthError
                && matches!(error.status, Some(401 | 403))
        })
}

fn codex_refresh_error(profile: &str, failure: CodexOAuthRefreshFailure) -> anyhow::Error {
    anyhow::anyhow!(
        "OpenAI Codex OAuth refresh failed for Holon credential profile {profile}: {} ({})",
        failure.message,
        failure.kind.as_str()
    )
}

pub(super) fn resolve_openai_codex_credential(
    provider_config: &ProviderRuntimeConfig,
    codex_home: &Path,
) -> Result<CodexCliCredential> {
    let profile = provider_config
        .credential
        .as_deref()
        .filter(|material| !material.trim().is_empty())
        .map(|material| {
            load_codex_oauth_profile_credential(
                material,
                provider_config
                    .auth
                    .profile
                    .as_deref()
                    .unwrap_or("openai-codex"),
            )
        })
        .transpose()?;
    let cli = if provider_config.auth.external.as_deref() == Some("codex_cli") || profile.is_none()
    {
        load_codex_cli_credential(codex_home).ok()
    } else {
        None
    };
    choose_openai_codex_credential(profile, cli).ok_or_else(|| {
        anyhow::anyhow!(
            "no Holon openai-codex credential profile or usable Codex CLI credentials are available"
        )
    })
}

pub(super) fn openai_model_policy_from_config(
    config: &AppConfig,
    provider: ProviderId,
    model: &str,
) -> crate::model_catalog::ResolvedRuntimeModelPolicy {
    let base_context_config = ContextConfig {
        recent_messages: config.context_window_messages,
        recent_briefs: config.context_window_briefs,
        compaction_trigger_messages: config.compaction_trigger_messages,
        compaction_keep_recent_messages: config.compaction_keep_recent_messages,
        prompt_budget_estimated_tokens: config.prompt_budget_estimated_tokens,
        compaction_trigger_estimated_tokens: config.compaction_trigger_estimated_tokens,
        compaction_keep_recent_estimated_tokens: config.compaction_keep_recent_estimated_tokens,
        recent_episode_candidates: config.recent_episode_candidates,
        max_relevant_episodes: config.max_relevant_episodes,
        ..ContextConfig::default()
    };
    let route_ref = config
        .providers
        .get(&provider)
        .map(|provider_config| {
            ModelRouteRef::new(
                provider_config.route_provider.clone(),
                provider_config.route_endpoint.clone(),
                model,
            )
        })
        .unwrap_or_else(|| ModelRouteRef::from_legacy_model_ref(&ModelRef::new(provider, model)));
    RuntimeModelCatalog::from_config(config)
        .resolved_model_policy(&base_context_config, Some(&route_ref))
}

pub(super) fn openai_model_policy_for_runtime_config(
    provider_config: &ProviderRuntimeConfig,
    model: &str,
    max_output_tokens: u32,
) -> crate::model_catalog::ResolvedRuntimeModelPolicy {
    let model_ref = ModelRef::new(provider_config.route_provider.clone(), model);
    let route_ref = ModelRouteRef::new(
        provider_config.route_provider.clone(),
        provider_config.route_endpoint.clone(),
        model,
    );
    let mut model_overrides = HashMap::new();
    model_overrides.insert(
        model_ref,
        ModelRuntimeOverride {
            runtime_max_output_tokens: Some(max_output_tokens),
            ..ModelRuntimeOverride::default()
        },
    );
    RuntimeModelCatalog {
        model_overrides,
        configured_runtime_max_output_tokens: max_output_tokens,
        ..RuntimeModelCatalog::default()
    }
    .resolved_model_policy(&ContextConfig::default(), Some(&route_ref))
}

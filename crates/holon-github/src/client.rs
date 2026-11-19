use std::{fmt, io::Read, process::Stdio, time::Duration};

use reqwest::{
    header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION},
    Url,
};

const BODY_LIMIT: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthSource {
    Anonymous,
    Configured,
    Environment,
    Gh,
}

#[derive(Clone)]
pub struct GitHubAuth {
    token: Option<String>,
    pub source: AuthSource,
}

impl fmt::Debug for GitHubAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubAuth")
            .field("source", &self.source)
            .finish()
    }
}

impl GitHubAuth {
    pub fn anonymous() -> Self {
        Self {
            token: None,
            source: AuthSource::Anonymous,
        }
    }

    pub fn resolve_blocking(configured: Option<String>, env_keys: &[&str]) -> Self {
        if let Some(auth) = Self::from_token(configured, AuthSource::Configured) {
            return auth;
        }
        for key in env_keys {
            if let Some(auth) = Self::from_token(std::env::var(key).ok(), AuthSource::Environment) {
                return auth;
            }
        }
        Self::from_token(gh_token(Duration::from_secs(5)), AuthSource::Gh)
            .unwrap_or_else(Self::anonymous)
    }

    pub async fn resolve(configured: Option<String>, env_keys: &[&str]) -> Self {
        if let Some(auth) = Self::selected_token(configured, env_keys) {
            return auth;
        }
        Self::from_token(gh_token_async(Duration::from_secs(5)).await, AuthSource::Gh)
            .unwrap_or_else(Self::anonymous)
    }

    fn selected_token(configured: Option<String>, env_keys: &[&str]) -> Option<Self> {
        Self::from_token(configured, AuthSource::Configured).or_else(|| {
            env_keys
                .iter()
                .find_map(|key| Self::from_token(std::env::var(key).ok(), AuthSource::Environment))
        })
    }

    pub(crate) fn from_token(token: Option<String>, source: AuthSource) -> Option<Self> {
        let token = token?.trim().to_owned();
        if token.is_empty() {
            return None;
        }
        if HeaderValue::from_str(&format!("Bearer {token}")).is_err() {
            return Some(Self {
                token: None,
                source: AuthSource::Anonymous,
            });
        }
        Some(Self {
            token: Some(token),
            source,
        })
    }

    fn header(&self) -> Option<HeaderValue> {
        let mut value = HeaderValue::from_str(&format!("Bearer {}", self.token.as_ref()?)).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
}

#[cfg(unix)]
fn gh_token(timeout: Duration) -> Option<String> {
    use std::os::fd::AsRawFd;
    let mut child = std::process::Command::new("gh")
        .args(["auth", "token", "-h", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = (|| {
        let mut stdout = child.stdout.take()?;
        let fd = stdout.as_raw_fd();
        // Nonblocking reads keep the deadline effective even if descendants hold stdout.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return None;
        }
        let start = std::time::Instant::now();
        let mut bytes = Vec::new();
        let mut eof = false;
        loop {
            if start.elapsed() >= timeout {
                return None;
            }
            if !eof {
                let mut buffer = [0; 1024];
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        if bytes.len() + n > BODY_LIMIT {
                            return None;
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return None,
                }
            }
            if let Some(status) = child.try_wait().ok()? {
                if !status.success() {
                    return None;
                }
                if eof {
                    return String::from_utf8(bytes).ok();
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    let _ = child.kill();
    let _ = child.wait();
    output
}

#[cfg(not(unix))]
fn gh_token(_timeout: Duration) -> Option<String> {
    // Do not fall back to an unbounded pipe reader on unsupported platforms.
    None
}

async fn gh_token_async(timeout: Duration) -> Option<String> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new("gh")
        .args(["auth", "token", "-h", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let output = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()?
            .take((BODY_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() > BODY_LIMIT || !child.wait().await.ok()?.success() {
            return None;
        }
        String::from_utf8(bytes).ok()
    })
    .await;
    let _ = child.start_kill();
    // Keep cleanup bounded; kill_on_drop also protects cancellation paths.
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    output.ok().flatten()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitKind {
    Primary,
    Secondary,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct RateLimit {
    pub upstream_status: u16,
    pub retry_after_seconds: Option<u64>,
    pub reset_at: Option<String>,
    pub kind: RateLimitKind,
    pub auth_source: AuthSource,
}

impl RateLimit {
    pub fn recovery_hint(&self) -> String {
        let mut hint = if let Some(seconds) = self.retry_after_seconds {
            format!("Retry after {seconds} seconds.")
        } else if let Some(reset) = &self.reset_at {
            format!("Retry after the GitHub API limit resets at {reset} (UTC).")
        } else {
            "Wait for the GitHub API limit to reset before retrying.".to_owned()
        };
        if self.auth_source == AuthSource::Anonymous {
            hint.push_str(" Configure daemon-visible GITHUB_TOKEN/GH_TOKEN or authenticated gh for authenticated API limits.");
        }
        hint
    }
}

impl fmt::Display for RateLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GitHub API rate limit exceeded (HTTP {})",
            self.upstream_status
        )
    }
}
impl std::error::Error for RateLimit {}

#[derive(Debug)]
pub struct ApiFailure {
    pub status: u16,
    pub diagnostic: String,
    pub rate_limit: Option<RateLimit>,
}

impl fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.diagnostic.is_empty() {
            write!(f, "GitHub request failed (HTTP {})", self.status)
        } else {
            write!(
                f,
                "GitHub request failed (HTTP {}): {}",
                self.status, self.diagnostic
            )
        }
    }
}

impl std::error::Error for ApiFailure {}

struct Policy {
    auth: GitHubAuth,
    api_base: Url,
}

impl Policy {
    fn new(auth: GitHubAuth, api_base: &str) -> anyhow::Result<Self> {
        let api_base = Url::parse(api_base)?;
        anyhow::ensure!(
            matches!(api_base.scheme(), "http" | "https") && api_base.host_str().is_some(),
            "invalid GitHub API base"
        );
        Ok(Self { auth, api_base })
    }

    fn is_api(&self, url: &Url) -> bool {
        url.origin() == self.api_base.origin()
    }

    fn headers(&self, url: &Url) -> HeaderMap {
        let api = self.is_api(url);
        let archive = is_archive_url(url);
        let mut headers = HeaderMap::new();
        if api && !archive {
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("application/vnd.github+json"),
            );
        }
        let trusted_archive = archive
            && (self.is_api(url)
                || (url.scheme() == "https" && url.host_str() == Some("codeload.github.com")));
        if api
            || trusted_archive
            || (url.scheme() == "https" && url.host_str() == Some("raw.githubusercontent.com"))
        {
            if let Some(auth) = self.auth.header() {
                headers.insert(AUTHORIZATION, auth);
            }
        }
        headers
    }

    fn failure(&self, status: u16, headers: &HeaderMap, url: &Url, body: &[u8]) -> ApiFailure {
        let text = String::from_utf8_lossy(body);
        let message = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("message").and_then(|v| v.as_str()).map(str::to_owned))
            .unwrap_or_else(|| {
                if text.trim_start().starts_with('{') {
                    String::new()
                } else {
                    text.trim().to_owned()
                }
            });
        let lower = message.to_ascii_lowercase();
        let primary = lower.starts_with("api rate limit exceeded")
            || lower.starts_with("github api rate limit exceeded");
        let secondary = lower.starts_with("you have exceeded a secondary rate limit")
            || lower.starts_with("secondary rate limit");
        let remaining_zero = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            == Some("0");
        let rate_limit = if self.is_api(url)
            && (status == 429 || (status == 403 && (remaining_zero || primary || secondary)))
        {
            Some(RateLimit {
                upstream_status: status,
                retry_after_seconds: headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok()),
                reset_at: headers
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i64>().ok())
                    .and_then(|v| chrono::DateTime::from_timestamp(v, 0))
                    .filter(|v| (0..=9999).contains(&chrono::Datelike::year(v)))
                    .map(|v| v.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
                kind: if secondary {
                    RateLimitKind::Secondary
                } else if remaining_zero || primary {
                    RateLimitKind::Primary
                } else {
                    RateLimitKind::Unknown
                },
                auth_source: self.auth.source,
            })
        } else {
            None
        };
        let text = if let Some(token) = &self.auth.token {
            text.replace(token, "<redacted>")
        } else {
            text.into_owned()
        };
        let diagnostic: String = holon_domain::sanitize_runtime_error_text(&text)
            .chars()
            .take(BODY_LIMIT)
            .collect();
        ApiFailure {
            status,
            diagnostic,
            rate_limit,
        }
    }
}

pub struct BlockingClient {
    client: reqwest::blocking::Client,
    policy: Policy,
}
impl BlockingClient {
    pub fn new(
        client: reqwest::blocking::Client,
        auth: GitHubAuth,
        api_base: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client,
            policy: Policy::new(auth, api_base)?,
        })
    }
    pub fn get(&self, url: &str) -> reqwest::blocking::RequestBuilder {
        let request = self.client.get(url);
        match Url::parse(url) {
            Ok(url) => request.headers(self.policy.headers(&url)),
            Err(_) => request,
        }
    }
    pub fn api_url(&self, url: &str) -> anyhow::Result<String> {
        map_api_url(&self.policy.api_base, url)
    }
    pub fn auth_present(&self) -> bool {
        self.policy.auth.token.is_some()
    }
    pub fn response_error(&self, mut response: reqwest::blocking::Response) -> ApiFailure {
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let url = response.url().clone();
        let mut body = Vec::new();
        if (&mut response)
            .take(BODY_LIMIT as u64)
            .read_to_end(&mut body)
            .is_err()
        {
            body.clear();
        }
        self.policy.failure(status, &headers, &url, &body)
    }
}

pub struct AsyncClient {
    client: reqwest::Client,
    policy: Policy,
}
impl AsyncClient {
    pub fn new(client: reqwest::Client, auth: GitHubAuth, api_base: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client,
            policy: Policy::new(auth, api_base)?,
        })
    }
    pub fn get(&self, url: Url) -> reqwest::RequestBuilder {
        let headers = self.policy.headers(&url);
        self.client.get(url).headers(headers)
    }
    pub fn api_url(&self, url: &str) -> anyhow::Result<String> {
        map_api_url(&self.policy.api_base, url)
    }
    pub fn auth_present(&self) -> bool {
        self.policy.auth.token.is_some()
    }
    pub async fn response_error(&self, mut response: reqwest::Response) -> ApiFailure {
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let url = response.url().clone();
        let mut body = Vec::new();
        while body.len() < BODY_LIMIT {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    body.extend_from_slice(&chunk[..chunk.len().min(BODY_LIMIT - body.len())])
                }
                Ok(None) => break,
                Err(_) => {
                    body.clear();
                    break;
                }
            }
        }
        self.policy.failure(status, &headers, &url, &body)
    }
}

fn is_archive_url(url: &Url) -> bool {
    url.host_str() == Some("codeload.github.com")
        || url
            .path_segments()
            .is_some_and(|mut segments| segments.nth(2) == Some("tar.gz"))
}

fn map_api_url(api_base: &Url, url: &str) -> anyhow::Result<String> {
    let source = Url::parse(url)?;
    let github = Url::parse("https://api.github.com")?;
    anyhow::ensure!(
        source.origin() == github.origin()
            && source.username().is_empty()
            && source.password().is_none()
            && source.fragment().is_none(),
        "expected a GitHub API URL"
    );
    let mut target = api_base.clone();
    // api_base replaces the origin, not the canonical GitHub API path.
    target.set_path(source.path());
    target.set_query(source.query());
    target.set_fragment(None);
    Ok(target.into())
}

#[cfg(test)]
mod tests;

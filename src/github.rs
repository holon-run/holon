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
    pub fn resolve_blocking(configured: Option<String>, env_keys: &[&str]) -> Self {
        if let Some(auth) = Self::from_token(configured, AuthSource::Configured) {
            return auth;
        }
        for key in env_keys {
            if let Some(auth) = Self::from_token(std::env::var(key).ok(), AuthSource::Environment) {
                return auth;
            }
        }
        Self::from_token(gh_token(Duration::from_secs(5)), AuthSource::Gh).unwrap_or(Self {
            token: None,
            source: AuthSource::Anonymous,
        })
    }

    pub async fn resolve(configured: Option<String>, env_keys: &[&str]) -> Self {
        if let Some(auth) = Self::selected_token(configured, env_keys) {
            return auth;
        }
        Self::from_token(gh_token_async(Duration::from_secs(5)).await, AuthSource::Gh).unwrap_or(
            Self {
                token: None,
                source: AuthSource::Anonymous,
            },
        )
    }

    fn selected_token(configured: Option<String>, env_keys: &[&str]) -> Option<Self> {
        Self::from_token(configured, AuthSource::Configured).or_else(|| {
            env_keys
                .iter()
                .find_map(|key| Self::from_token(std::env::var(key).ok(), AuthSource::Environment))
        })
    }

    fn from_token(token: Option<String>, source: AuthSource) -> Option<Self> {
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
        let mut headers = HeaderMap::new();
        if api {
            headers.insert(
                ACCEPT,
                HeaderValue::from_static("application/vnd.github+json"),
            );
        }
        if api || (url.scheme() == "https" && url.host_str() == Some("raw.githubusercontent.com")) {
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
        let diagnostic: String = crate::runtime_error::sanitize_runtime_error_text(&text)
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
        let source = Url::parse(url)?;
        let github = Url::parse("https://api.github.com")?;
        anyhow::ensure!(
            source.origin() == github.origin()
                && source.username().is_empty()
                && source.password().is_none()
                && source.fragment().is_none(),
            "expected a GitHub API URL"
        );
        let mut target = self.policy.api_base.clone();
        target.set_path(source.path());
        target.set_query(source.query());
        target.set_fragment(None);
        Ok(target.into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_api_url_only_maps_github_api_origin() {
        let client = BlockingClient::new(
            reqwest::blocking::Client::new(),
            GitHubAuth::from_token(Some("secret-token".into()), AuthSource::Configured).unwrap(),
            "http://127.0.0.1:1234",
        )
        .unwrap();
        assert_eq!(
            client
                .api_url("https://api.github.com/repos/a/b/contents/a%20b?ref=main")
                .unwrap(),
            "http://127.0.0.1:1234/repos/a/b/contents/a%20b?ref=main"
        );
        for url in [
            "https://example.com/repos/a/b",
            "https://api.github.com.example.com/repos/a/b",
            "http://api.github.com/repos/a/b",
            "https://api.github.com:444/repos/a/b",
            "https://user:secret@api.github.com/repos/a/b",
            "https://api.github.com/repos/a/b#fragment",
            "/repos/a/b",
            "https://raw.githubusercontent.com/a/b/main/file",
        ] {
            assert!(client.api_url(url).is_err(), "{url}");
        }
    }

    fn policy() -> Policy {
        Policy::new(
            GitHubAuth::from_token(Some("secret-token".into()), AuthSource::Configured).unwrap(),
            "https://api.github.com",
        )
        .unwrap()
    }

    #[test]
    fn classification_is_api_only_and_status_specific() {
        let policy = policy();
        let api = Url::parse("https://api.github.com/repos/a/b").unwrap();
        let raw = Url::parse("https://raw.githubusercontent.com/a/b").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("12"));
        for status in [401, 403, 404, 500] {
            assert!(policy
                .failure(status, &headers, &api, b"Forbidden")
                .rate_limit
                .is_none());
        }
        assert_eq!(
            policy
                .failure(429, &headers, &api, b"Forbidden")
                .rate_limit
                .unwrap()
                .kind,
            RateLimitKind::Unknown
        );
        assert!(policy
            .failure(429, &headers, &raw, b"API rate limit exceeded")
            .rate_limit
            .is_none());
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        assert_eq!(
            policy
                .failure(403, &headers, &api, b"Forbidden")
                .rate_limit
                .unwrap()
                .kind,
            RateLimitKind::Primary
        );
        assert!(policy
            .failure(500, &headers, &api, b"API rate limit exceeded")
            .rate_limit
            .is_none());
        headers.remove("x-ratelimit-remaining");
        for body in [
            br#"{"message":"API rate limit exceeded for x"}"#.as_slice(),
            b"API rate limit exceeded for x",
        ] {
            assert!(policy
                .failure(403, &headers, &api, body)
                .rate_limit
                .is_some());
        }
        assert!(policy
            .failure(
                403,
                &headers,
                &api,
                br#"{"message":"You have exceeded a secondary rate limit."}"#
            )
            .rate_limit
            .is_some());
        assert!(policy
            .failure(403, &headers, &api, b"not an API rate limit exceeded")
            .rate_limit
            .is_none());
    }

    #[test]
    fn scoped_sensitive_headers_and_redacted_errors() {
        let policy = policy();
        for (url, auth, accept) in [
            ("https://api.github.com/a", true, true),
            ("https://raw.githubusercontent.com/a", true, false),
            ("http://raw.githubusercontent.com/a", false, false),
            ("https://example.com/a", false, false),
            ("https://api.github.com:444/a", false, false),
        ] {
            let url = Url::parse(url).unwrap();
            let headers = policy.headers(&url);
            assert_eq!(headers.contains_key(AUTHORIZATION), auth);
            assert_eq!(headers.contains_key(ACCEPT), accept);
            if auth {
                assert!(headers[AUTHORIZATION].is_sensitive());
            }
        }
        let api = Url::parse("https://api.github.com").unwrap();
        assert!(!policy
            .failure(500, &HeaderMap::new(), &api, b"failed secret-token")
            .diagnostic
            .contains("secret-token"));
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-ratelimit-reset",
            HeaderValue::from_static("18446744073709551615"),
        );
        assert!(policy
            .failure(429, &headers, &api, b"")
            .rate_limit
            .unwrap()
            .reset_at
            .is_none());
        headers.insert("x-ratelimit-reset", HeaderValue::from_static("0"));
        assert_eq!(
            policy
                .failure(429, &headers, &api, b"")
                .rate_limit
                .unwrap()
                .reset_at
                .as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
    }

    #[test]
    fn configured_environment_priority_and_debug() {
        let _lock = crate::test_env::lock_env();
        let key = "HOLON_GITHUB_UNIT_TEST_TOKEN";
        let old = std::env::var_os(key);
        std::env::set_var(key, "env-secret");
        let configured = GitHubAuth::resolve_blocking(Some(" config-secret ".into()), &[key]);
        assert_eq!(configured.source, AuthSource::Configured);
        assert!(!format!("{configured:?}").contains("config-secret"));
        let env =
            GitHubAuth::resolve_blocking(Some(" ".into()), &["HOLON_GITHUB_UNIT_TEST_UNSET", key]);
        assert_eq!(env.source, AuthSource::Environment);
        assert_eq!(env.token.as_deref(), Some("env-secret"));
        let invalid = GitHubAuth::resolve_blocking(Some("bad\nheader".into()), &[key]);
        assert_eq!(invalid.source, AuthSource::Anonymous);
        assert!(invalid.token.is_none());
        std::env::set_var(key, "bad\nheader");
        assert_eq!(
            GitHubAuth::resolve_blocking(None, &[key]).source,
            AuthSource::Anonymous
        );
        match old {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    #[cfg(unix)]
    #[test]
    fn fake_gh_fixed_arguments_failures_and_timeout_reaping() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::test_env::lock_env();
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("gh");
        let old_path = std::env::var_os("PATH");
        struct RestorePath(Option<std::ffi::OsString>);
        impl Drop for RestorePath {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(path) => std::env::set_var("PATH", path),
                    None => std::env::remove_var("PATH"),
                }
            }
        }
        let _restore_path = RestorePath(old_path);
        std::env::set_var("PATH", directory.path());
        let write = |body: &str| {
            std::fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        };
        write("test \"$*\" = 'auth token -h github.com' || exit 9\ntest \"$GH_PROMPT_DISABLED\" = 1 || exit 8\nread line && exit 7\nprintf 'cli-secret\\n'");
        let auth = GitHubAuth::resolve_blocking(None, &[]);
        assert_eq!(auth.source, AuthSource::Gh);
        assert_eq!(auth.token.as_deref(), Some("cli-secret"));
        // Request construction reuses resolved credentials without invoking gh.
        write("exit 1");
        let client = BlockingClient::new(
            reqwest::blocking::Client::new(),
            auth,
            "https://api.github.com",
        )
        .unwrap();
        for _ in 0..2 {
            assert_eq!(
                client
                    .get("https://api.github.com/a")
                    .build()
                    .unwrap()
                    .headers()[AUTHORIZATION],
                "Bearer cli-secret"
            );
        }
        assert_eq!(
            GitHubAuth::resolve_blocking(None, &[]).source,
            AuthSource::Anonymous
        );
        write("printf '\\377'");
        assert_eq!(
            GitHubAuth::resolve_blocking(None, &[]).source,
            AuthSource::Anonymous
        );
        write("printf '   '");
        assert_eq!(
            GitHubAuth::resolve_blocking(None, &[]).source,
            AuthSource::Anonymous
        );
        let pid_file = directory.path().join("pid");
        write(&format!(
            "echo $$ > '{}'\nexec /bin/sleep 30",
            pid_file.display()
        ));
        let start = std::time::Instant::now();
        assert!(gh_token(Duration::from_millis(100)).is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // ESRCH confirms that the child was both terminated and reaped.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        write("printf '%09000d' 0");
        assert!(gh_token(Duration::from_secs(1)).is_none());
        // Descendants retaining stdout must not defeat the deadline.
        write("/bin/sleep 1 &\nexit 0");
        let start = std::time::Instant::now();
        assert!(gh_token(Duration::from_millis(50)).is_none());
        assert!(start.elapsed() < Duration::from_millis(500));
        write("printf 'async-cli-secret'");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let auth = runtime.block_on(GitHubAuth::resolve(None, &[]));
        assert_eq!(auth.token.as_deref(), Some("async-cli-secret"));
        runtime.block_on(async {
            assert_eq!(
                GitHubAuth::resolve_blocking(None, &[]).source,
                AuthSource::Gh
            );
            assert_eq!(
                GitHubAuth::resolve(Some("bad\nheader".into()), &[])
                    .await
                    .source,
                AuthSource::Anonymous
            );
        });
        let marker = directory.path().join("invoked");
        write(&format!("echo invoked > '{}'\nexit 1", marker.display()));
        // PATH is the only environment key consulted here, and contains only
        // this test's temporary directory, never a real credential.
        let configured = GitHubAuth::resolve_blocking(Some("profile-secret".into()), &["PATH"]);
        assert_eq!(configured.source, AuthSource::Configured);
        assert_eq!(configured.token.as_deref(), Some("profile-secret"));
        let environment = GitHubAuth::resolve_blocking(None, &["PATH"]);
        assert_eq!(environment.source, AuthSource::Environment);
        runtime.block_on(async {
            let configured = GitHubAuth::resolve(Some("profile-secret".into()), &["PATH"]).await;
            assert_eq!(configured.source, AuthSource::Configured);
            assert_eq!(configured.token.as_deref(), Some("profile-secret"));
            let environment = GitHubAuth::resolve(None, &["PATH"]).await;
            assert_eq!(environment.source, AuthSource::Environment);
            assert_eq!(environment.token.as_deref(), directory.path().to_str());
        });
        assert!(!marker.exists(), "selected credentials must not launch gh");
        for body in ["exit 1", "printf '   '", "printf '\\377'"] {
            write(body);
            let auth = runtime.block_on(GitHubAuth::resolve(None, &[]));
            assert_eq!(auth.source, AuthSource::Anonymous);
            assert!(auth.token.is_none());
            let client =
                AsyncClient::new(reqwest::Client::new(), auth, "https://api.github.com").unwrap();
            assert!(!client
                .get(Url::parse("https://api.github.com/a").unwrap())
                .build()
                .unwrap()
                .headers()
                .contains_key(AUTHORIZATION));
        }
        write(&format!(
            "echo $$ > '{}'\nexec /bin/sleep 30",
            pid_file.display()
        ));
        runtime.block_on(async {
            let start = std::time::Instant::now();
            let (token, executor_progress) =
                tokio::join!(gh_token_async(Duration::from_millis(300)), async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    start.elapsed()
                });
            assert!(token.is_none());
            assert!(executor_progress < Duration::from_millis(200));
            assert!(start.elapsed() < Duration::from_secs(2));
        });
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        std::fs::remove_file(&pid_file).unwrap();
        runtime.block_on(async {
            let lookup = tokio::spawn(gh_token_async(Duration::from_secs(30)));
            let pid: libc::pid_t = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(pid) = std::fs::read_to_string(&pid_file)
                        .ok()
                        .and_then(|text| text.trim().parse().ok())
                    {
                        break pid;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            lookup.abort();
            assert!(lookup.await.unwrap_err().is_cancelled());
            tokio::time::timeout(Duration::from_secs(2), async {
                while unsafe { libc::kill(pid, 0) } != -1 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
            })
            .await
            .expect("cancelled gh lookup must terminate and reap its child");
        });
        std::fs::remove_file(&executable).unwrap();
        let auth = runtime.block_on(GitHubAuth::resolve(None, &[]));
        assert_eq!(auth.source, AuthSource::Anonymous);
        assert!(auth.header().is_none());
        assert_eq!(
            GitHubAuth::resolve_blocking(None, &[]).source,
            AuthSource::Anonymous
        );
    }

    #[test]
    fn recovery_hint_precedence_and_daemon_auth() {
        let mut limit = RateLimit {
            upstream_status: 429,
            retry_after_seconds: Some(7),
            reset_at: Some("2030-01-01T00:00:00Z".into()),
            kind: RateLimitKind::Unknown,
            auth_source: AuthSource::Anonymous,
        };
        let hint = limit.recovery_hint();
        assert!(hint.contains("7 seconds"));
        assert!(!hint.contains("2030"));
        assert!(hint.contains("daemon-visible GITHUB_TOKEN/GH_TOKEN"));
        assert!(hint.contains("authenticated gh"));
        limit.retry_after_seconds = None;
        assert!(limit.recovery_hint().contains("2030-01-01T00:00:00Z (UTC)"));
        limit.reset_at = None;
        assert!(limit.recovery_hint().contains("Wait"));
        limit.auth_source = AuthSource::Configured;
        assert!(!limit.recovery_hint().contains("Configure"));
    }

    #[test]
    fn blocking_redirect_strips_authorization_across_hosts() {
        use std::io::Write;
        use std::net::TcpListener;
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        let source = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", source.local_addr().unwrap());
        let location = format!(
            "http://localhost:{}/final",
            target.local_addr().unwrap().port()
        );
        let source_thread = std::thread::spawn(move || {
            let (mut stream, _) = source.accept().unwrap();
            let mut bytes = [0; 4096];
            let n = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]).to_ascii_lowercase();
            assert!(request.contains("authorization: bearer secret-token"));
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let target_thread = std::thread::spawn(move || {
            let (mut stream, _) = target.accept().unwrap();
            let mut bytes = [0; 4096];
            let n = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]).to_ascii_lowercase();
            assert!(!request.contains("authorization:"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let client = BlockingClient::new(
            reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            policy().auth,
            &base,
        )
        .unwrap();
        assert!(client.get(&base).send().unwrap().status().is_success());
        source_thread.join().unwrap();
        target_thread.join().unwrap();
    }

    #[tokio::test]
    async fn async_auth_headers_and_bounded_response() {
        use std::io::Write;
        use std::net::TcpListener;
        let auth = GitHubAuth::resolve(Some("async-secret".into()), &[]).await;
        assert_eq!(auth.source, AuthSource::Configured);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 4096];
            let mut received = 0;
            while !request[..received].ends_with(b"\r\n\r\n") {
                assert!(received < request.len(), "request headers exceed buffer");
                let read = stream.read(&mut request[received..]).unwrap();
                assert!(read > 0, "request closed before complete headers");
                received += read;
            }
            let body = "x".repeat(BODY_LIMIT * 2);
            write!(stream, "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let client = AsyncClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            auth,
            &base,
        )
        .unwrap();
        let request = client.get(Url::parse(&base).unwrap()).build().unwrap();
        assert!(request.headers()[AUTHORIZATION].is_sensitive());
        let response = client.get(Url::parse(&base).unwrap()).send().await.unwrap();
        let failure = client.response_error(response).await;
        assert_eq!(failure.status, 429);
        assert!(failure.diagnostic.len() <= BODY_LIMIT);
        let rate = failure.rate_limit.unwrap();
        assert_eq!(rate.retry_after_seconds, Some(7));
        assert_eq!(rate.kind, RateLimitKind::Unknown);
        assert!(rate.to_string().contains("GitHub API rate limit"));
        assert!(!rate.to_string().contains("xxx"));
        thread.join().unwrap();
    }
}

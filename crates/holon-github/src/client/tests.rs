use std::sync::{Mutex, MutexGuard};

use super::*;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    let _lock = lock_env();
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
    let _lock = lock_env();
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
    write(
            "test \"$*\" = 'auth token -h github.com' || exit 9\ntest \"$GH_PROMPT_DISABLED\" = 1 || exit 8\nread line && exit 7\nprintf 'cli-secret\\n'",
        );
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

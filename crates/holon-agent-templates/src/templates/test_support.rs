use flate2::{write::GzEncoder, Compression};
use std::{
    env,
    io::{Cursor, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tar::{Builder, EntryType, Header};

pub struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    pub fn set(key: &'static str, value: String) -> Self {
        let old = env::var(key).ok();
        env::set_var(key, value);
        Self { key, old }
    }

    pub fn remove(key: &'static str) -> Self {
        let old = env::var(key).ok();
        env::remove_var(key);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}

pub struct MockGithubServer {
    pub addr: std::net::SocketAddr,
    shutdown: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<MockGithubRequest>>>,
    _handle: thread::JoinHandle<()>,
}

#[derive(Debug, Clone)]
pub struct MockGithubRequest {
    pub path: String,
    pub authorization: Option<String>,
}

impl MockGithubServer {
    /// Start a server that answers any number of requests; unknown paths get
    /// 404. Responses advertise `Connection: close` and the socket is closed
    /// after each response so HTTP clients open a fresh connection per request
    /// instead of racing pooled keep-alive reuse against this server's close.
    pub fn start(responses: Vec<(&'static str, u16, String)>) -> Self {
        Self::start_bytes(
            responses
                .into_iter()
                .map(|(path, status, body)| (path, status, body.into_bytes()))
                .collect(),
        )
    }

    pub fn start_bytes(responses: Vec<(&'static str, u16, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let requests_clone = requests.clone();
        let shutdown_clone = shutdown.clone();
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if shutdown_clone.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = match stream {
                    Ok(stream) => stream,
                    Err(_) => break,
                };
                let request = match read_http_request_head(&mut stream) {
                    Some(request) => request,
                    None => continue,
                };
                let request_line = request.lines().next().unwrap_or("");
                let req_path = request_line.split_whitespace().nth(1).unwrap_or("");
                let authorization = request
                    .lines()
                    .find_map(|line| line.strip_prefix("authorization: "))
                    .or_else(|| {
                        request
                            .lines()
                            .find_map(|line| line.strip_prefix("Authorization: "))
                    })
                    .map(|value| value.trim().to_string());
                requests_clone.lock().unwrap().push(MockGithubRequest {
                    path: req_path.to_string(),
                    authorization,
                });

                // Strip query for matching.
                let match_path = req_path.split('?').next().unwrap_or("");
                let matched = responses.iter().find(|(p, _, _)| match_path == *p);
                let (status_code, body) = match matched {
                    Some((_, code, body)) => (*code, body.clone()),
                    None => (404, b"{\"message\":\"not found\"}".to_vec()),
                };
                let status_text = format!("{status_code} Mock");
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status_text}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
                let _ = stream.flush();
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Self {
            addr,
            shutdown,
            requests,
            _handle: handle,
        }
    }

    pub fn requests(&self) -> Vec<MockGithubRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockGithubServer {
    fn drop(&mut self) {
        if !self.shutdown.swap(true, Ordering::SeqCst) {
            // Wake the accept loop so the server thread exits instead of leaking.
            let _ = TcpStream::connect(self.addr);
        }
    }
}

/// Read a full HTTP request head (through the blank line) so a split TCP
/// segment cannot truncate the request line or headers.
fn read_http_request_head(stream: &mut TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > 64 * 1024 {
            return None;
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(feature = "test-support")]
pub fn parse_skill_refs(
    environment: super::TemplateEnvironment,
    path: std::path::PathBuf,
) -> anyhow::Result<impl std::fmt::Debug> {
    super::parse_skill_refs(environment, path)
}

#[cfg(feature = "test-support")]
pub fn parse_template_skills_manifest(input: &str) -> Result<(), toml::de::Error> {
    toml::from_str::<super::TemplateSkillsManifest>(input).map(drop)
}

#[cfg(feature = "test-support")]
pub fn builtin_template_catalog_entry(
    environment: super::TemplateEnvironment,
    template: &super::BuiltinTemplate,
) -> holon_domain::AgentTemplateCatalogEntry {
    super::builtin_template_catalog_entry(environment, template)
}

fn archive_bytes(root: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let mut compressed = Vec::new();
    {
        let encoder = GzEncoder::new(&mut compressed, Compression::default());
        let mut builder = Builder::new(encoder);
        // GitHub codeload archives include repository metadata before the content root.
        let metadata = b"52 comment=34b7e2cc93dc428f8c1b4bf696de6dcfe7373436\n";
        let mut header = Header::new_gnu();
        header.set_path("pax_global_header").unwrap();
        header.set_entry_type(EntryType::XGlobalHeader);
        header.set_size(metadata.len() as u64);
        header.set_cksum();
        builder.append(&header, Cursor::new(metadata)).unwrap();
        for (path, contents) in files {
            let full_path = format!("{root}/{path}");
            let mut header = Header::new_gnu();
            header.set_path(full_path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_cksum();
            builder
                .append(&header, Cursor::new(contents.as_bytes()))
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }
    compressed
}

pub fn archive_route(reference: &str, files: &[(&str, &str)]) -> (&'static str, u16, Vec<u8>) {
    let mut url = reqwest::Url::parse("http://archive.invalid").unwrap();
    url.path_segments_mut()
        .unwrap()
        .push("owner")
        .push("repo")
        .push("tar.gz")
        .push(reference);
    let path = Box::leak(url.path().to_owned().into_boxed_str());
    (path, 200, archive_bytes("repo-snapshot", files))
}

pub fn archive_server(reference: &str, files: &[(&str, &str)]) -> MockGithubServer {
    MockGithubServer::start_bytes(vec![archive_route(reference, files)])
}

pub fn archive_base(server: &MockGithubServer) -> EnvGuard {
    EnvGuard::set(
        super::GITHUB_TEMPLATE_API_BASE_ENV,
        format!("http://{}", server.addr),
    )
}

pub fn archive_files<'a>(worker_agents: &'a str) -> Vec<(&'static str, &'a str)> {
    vec![
        ("agent_templates/worker/template.toml", WORKER_MANIFEST),
        ("agent_templates/worker/AGENTS.md", worker_agents),
    ]
}

pub const WORKER_MANIFEST: &str = r#"schema = "holon.agent_template.v1"
id = "worker"
name = "Worker"
summary = "Implementation agent"
"#;
pub const REVIEWER_MANIFEST: &str = r#"schema = "holon.agent_template.v1"
id = "reviewer"
name = "Reviewer"
summary = "Review agent"
"#;

#[cfg(feature = "test-support")]
pub fn installed_template_ids(templates_root: &std::path::Path) -> anyhow::Result<Vec<String>> {
    Ok(super::load_template_registry(templates_root)?
        .installed
        .into_keys()
        .collect())
}

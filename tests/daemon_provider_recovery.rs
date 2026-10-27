//! Compiled-daemon provider cooldown recovery regressions for #3484.
//!
//! These tests spawn the real `holon serve` binary against local mock
//! providers to prove, at process level:
//! - a provider cooldown park is durable across process death (SIGKILL) and
//!   the restarted daemon resumes the parked input on schedule, without any
//!   new message arriving;
//! - while the primary provider cools down, a healthy configured fallback is
//!   used and nothing is parked;
//! - a task-result continuation that is parked during cooldown recovers the
//!   same way, covering an isolated equivalent of "tool result arrives while
//!   the chain cools", not a replay of a production database.
//!
//! Every invocation has its own home, socket, environment, process group,
//! and mock provider ports. No real provider or network endpoint is used.

#![cfg(target_os = "linux")]

use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

const TOKEN: &str = "provider-recovery-test-secret-not-an-operator-token";
const AGENT_ID: &str = "main";
const DEADLINE: Duration = Duration::from_secs(120);
const PARK_WINDOW: Duration = Duration::from_secs(6);

fn holon_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_holon")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_holon").map(PathBuf::from))
        .expect("Cargo must provide the compiled holon binary")
}

// Instrumented daemon startup can be slow on CI. Serialize only this test
// executable; isolation never depends on shared mutable process state.
fn server_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn free_address() -> String {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MockMode {
    /// Agent bootstrap completes before the measured recovery scenario.
    Initialize,
    /// Every model request receives HTTP 429 without a Retry-After hint.
    RateLimited,
    /// Every model request receives a successful completion.
    Success,
}

struct MockState {
    mode: MockMode,
    /// The task-result scenario emits exactly one tool call before cooling.
    tool_call_pending: bool,
    wait_call_pending: bool,
    probe_command: Option<String>,
    task_id: Option<String>,
}

struct MockStats {
    rate_limited: AtomicUsize,
    success: AtomicUsize,
    last_success_at: Mutex<Option<DateTime<Utc>>>,
}

impl MockStats {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rate_limited: AtomicUsize::new(0),
            success: AtomicUsize::new(0),
            last_success_at: Mutex::new(None),
        })
    }
}

struct MockProvider {
    address: String,
    state: Arc<Mutex<MockState>>,
    stats: Arc<MockStats>,
}

impl MockProvider {
    fn start(initial_mode: MockMode, final_text: &'static str, tool_call_first: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock provider listener");
        let address = listener.local_addr().unwrap().to_string();
        let state = Arc::new(Mutex::new(MockState {
            mode: initial_mode,
            tool_call_pending: tool_call_first,
            wait_call_pending: false,
            probe_command: None,
            task_id: None,
        }));
        let stats = MockStats::new();
        let thread_state = state.clone();
        let thread_stats = stats.clone();
        thread::spawn(move || {
            for connection in listener.incoming() {
                let Ok(mut stream) = connection else { continue };
                let state = thread_state.clone();
                let stats = thread_stats.clone();
                thread::spawn(move || {
                    handle_mock_connection(&mut stream, &state, &stats, final_text);
                });
            }
        });
        Self {
            address,
            state,
            stats,
        }
    }

    fn set_mode(&self, mode: MockMode) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .mode = mode;
    }

    fn rate_limited_hits(&self) -> usize {
        self.stats.rate_limited.load(Ordering::SeqCst)
    }

    fn reset_hits(&self) {
        self.stats.rate_limited.store(0, Ordering::SeqCst);
        self.stats.success.store(0, Ordering::SeqCst);
        *self.stats.last_success_at.lock().unwrap() = None;
    }

    fn arm_tool_call(&self, counter: PathBuf) {
        let mut state = self.state.lock().unwrap();
        state.tool_call_pending = true;
        state.probe_command = Some(format!(
            "set -e; printf 'ran\\n' >> '{}'; sleep 2; printf task-result-recovery-tool-ran",
            counter.display()
        ));
    }

    fn success_hits(&self) -> usize {
        self.stats.success.load(Ordering::SeqCst)
    }

    fn task_id(&self) -> String {
        self.state
            .lock()
            .unwrap()
            .task_id
            .clone()
            .expect("mock must observe the real promoted task id")
    }

    fn last_success_at(&self) -> Option<DateTime<Utc>> {
        *self
            .stats
            .last_success_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn openai_text_body(text: &str) -> String {
    json!({
        "status": "completed",
        "usage": { "input_tokens": 3, "output_tokens": 1 },
        "output": [
            { "type": "message", "content": [{ "type": "output_text", "text": text }] }
        ]
    })
    .to_string()
}

fn openai_tool_call_body(command: &str) -> String {
    json!({
        "status": "completed",
        "usage": { "input_tokens": 5, "output_tokens": 2 },
        "output": [
            { "type": "message", "content": [{ "type": "output_text", "text": "running the probe" }] },
            {
                "type": "function_call",
                "call_id": "call_recovery_probe",
                "name": "ExecCommand",
                "arguments": json!({
                    "cmd": command,
                    "yield_time_ms": 0
                }).to_string()
            }
        ]
    })
    .to_string()
}

fn find_task_id(value: &Value) -> Option<String> {
    match value {
        Value::Object(fields) => fields
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| id.starts_with("task_"))
            .map(str::to_owned)
            .or_else(|| fields.values().find_map(find_task_id)),
        Value::Array(items) => items.iter().rev().find_map(find_task_id),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .as_ref()
            .and_then(find_task_id)
            .or_else(|| {
                // The model-visible command receipt is rendered prose, not JSON.
                text.split("task_").skip(1).find_map(|suffix| {
                    let id: String = suffix.chars().take_while(char::is_ascii_hexdigit).collect();
                    (id.len() >= 12).then(|| format!("task_{id}"))
                })
            }),
        _ => None,
    }
}

fn openai_wait_body(task_id: &str) -> String {
    json!({
        "status": "completed",
        "output": [{
            "type": "function_call",
            "call_id": "wait_recovery_probe",
            "name": "WaitFor",
            "arguments": json!({
                "wake": "task_result",
                "resource": task_id,
                "delivery": "silent",
                "reason": "wait for the isolated recovery probe"
            }).to_string()
        }]
    })
    .to_string()
}

fn anthropic_text_body(text: &str) -> String {
    json!({
        "content": [{ "type": "text", "text": text }],
        "stop_reason": "end_turn",
        "usage": { "input_tokens": 4, "output_tokens": 2 }
    })
    .to_string()
}

fn rate_limited_body() -> &'static str {
    "{\"error\":{\"message\":\"mock rate limit\",\"type\":\"rate_limit_error\"}}"
}

fn handle_mock_connection(
    stream: &mut TcpStream,
    state: &Mutex<MockState>,
    stats: &MockStats,
    final_text: &str,
) {
    let Some((path, request)) = read_http_request(stream) else {
        return;
    };
    let openai = path.ends_with("/responses");
    let anthropic = path.ends_with("/messages");
    if !openai && !anthropic {
        write_mock_response(stream, 404, "Not Found", "{\"error\":\"not found\"}");
        return;
    }
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.tool_call_pending {
        state.tool_call_pending = false;
        state.wait_call_pending = true;
        stats.success.fetch_add(1, Ordering::SeqCst);
        *stats
            .last_success_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Utc::now());
        write_mock_response(
            stream,
            200,
            "OK",
            &openai_tool_call_body(state.probe_command.as_deref().unwrap()),
        );
        return;
    }
    if state.wait_call_pending {
        let output = request["input"]
            .as_array()
            .and_then(|input| {
                input.iter().rev().find(|item| {
                    item["type"] == "function_call_output"
                        && item["call_id"] == "call_recovery_probe"
                })
            })
            .expect("request must contain the probe command receipt");
        let task_id =
            find_task_id(&output["output"]).expect("background command must return a real task id");
        state.task_id = Some(task_id.clone());
        state.wait_call_pending = false;
        stats.success.fetch_add(1, Ordering::SeqCst);
        *stats.last_success_at.lock().unwrap() = Some(Utc::now());
        write_mock_response(stream, 200, "OK", &openai_wait_body(&task_id));
        return;
    }
    match state.mode {
        MockMode::RateLimited => {
            stats.rate_limited.fetch_add(1, Ordering::SeqCst);
            write_mock_response(stream, 429, "Too Many Requests", rate_limited_body());
        }
        MockMode::Initialize | MockMode::Success => {
            stats.success.fetch_add(1, Ordering::SeqCst);
            *stats
                .last_success_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Utc::now());
            let text = if state.mode == MockMode::Initialize {
                "provider-recovery-initialized"
            } else {
                final_text
            };
            let body = if openai {
                openai_text_body(text)
            } else {
                anthropic_text_body(text)
            };
            write_mock_response(stream, 200, "OK", &body);
        }
    }
}

/// Reads the path and JSON body without logging provider request contents.
fn read_http_request(stream: &mut TcpStream) -> Option<(String, Value)> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?.to_string();
    let path = request_line.split_whitespace().nth(1)?.to_string();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let body_start = header_end + 4;
    if content_length > 4 * 1024 * 1024 {
        return None;
    }
    while buf.len() < body_start + content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
    }
    let body = serde_json::from_slice(&buf[body_start..body_start + content_length]).ok()?;
    Some((path, body))
}

fn write_mock_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

struct ProcessGroup {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl ProcessGroup {
    fn kill(&mut self) {
        // SAFETY: Command::process_group(0) created this private group. Signal
        // the group, not a PID recovered from untrusted runtime metadata.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }

    fn diagnostics(&self) -> String {
        format!(
            "{}\n{}",
            fs::read_to_string(&self.stdout).unwrap_or_default(),
            fs::read_to_string(&self.stderr).unwrap_or_default()
        )
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Fixture {
    processes: Vec<ProcessGroup>,
    home: tempfile::TempDir,
    provider_env: Vec<(String, String)>,
    client: reqwest::blocking::Client,
}

struct ParkedInput {
    message_id: String,
    not_before: DateTime<Utc>,
    source: Value,
}

impl Fixture {
    fn new(provider_env: Vec<(String, String)>) -> Self {
        let home = tempfile::tempdir().expect("isolated HOLON_HOME");
        fs::create_dir_all(home.path().join("ui")).unwrap();
        fs::write(home.path().join("ui/index.html"), "provider-recovery-ui").unwrap();
        fs::write(home.path().join("control.token"), TOKEN).unwrap();
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("blocking HTTP client");
        Self {
            processes: Vec::new(),
            home,
            provider_env,
            client,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    fn database(&self) -> Connection {
        let db = Connection::open_with_flags(
            self.path("state/runtime.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("open isolated runtime database read-only");
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db
    }

    fn parked_inputs(&self) -> Vec<ParkedInput> {
        let db = self.database();
        let mut query = db
            .prepare(
                "SELECT p.message_id, p.not_before,
                        (SELECT m.payload_json FROM messages m
                         WHERE m.message_id = p.message_id ORDER BY m.rowid DESC LIMIT 1)
                 FROM provider_queue_parking p WHERE p.agent_id = ?1",
            )
            .unwrap();
        query
            .query_map([AGENT_ID], |row| {
                let deadline: String = row.get(1)?;
                let source: String = row.get(2)?;
                Ok(ParkedInput {
                    message_id: row.get(0)?,
                    not_before: DateTime::parse_from_rfc3339(&deadline)
                        .unwrap()
                        .with_timezone(&Utc),
                    source: serde_json::from_str(&source).unwrap(),
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn assert_park_retained(&self, expected: &ParkedInput) {
        let inputs = self.parked_inputs();
        let actual = inputs
            .iter()
            .find(|input| input.message_id == expected.message_id)
            .expect("restart must retain the same parked source");
        assert_eq!(
            actual.not_before, expected.not_before,
            "restart must not extend the deadline"
        );
        // Sources may carry private context; never print their complete payload.
        assert!(
            actual.source == expected.source,
            "restart must not replace source evidence"
        );
    }

    fn assert_source_processed(&self, message_id: &str) {
        let status: String = self
            .database()
            .query_row(
                "SELECT status FROM queue_entries WHERE agent_id = ?1 AND message_id = ?2",
                [AGENT_ID, message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "processed",
            "parked input must reach a queue terminal"
        );
    }

    fn assert_task_source(&self, parked: &ParkedInput, task_id: &str) -> String {
        let db = self.database();
        let mut source = parked.source.clone();
        for _ in 0..16 {
            if source["task_id"].as_str() == Some(task_id) {
                assert_eq!(source["kind"], "task_result");
                return source["id"].as_str().unwrap().to_owned();
            }
            let Some(parent) = source["source_refs"]["source_message_id"].as_str() else {
                panic!(
                    "recovery lineage lacks the exact task result: kind={}, task_id={}",
                    source["kind"], source["task_id"]
                );
            };
            let payload: String = db
                .query_row(
                    "SELECT payload_json FROM messages WHERE message_id = ?1 ORDER BY rowid DESC LIMIT 1",
                    [parent],
                    |row| row.get(0),
                )
                .expect("recovery predecessor must be durable");
            source = serde_json::from_str(&payload).unwrap();
        }
        panic!("recovery lineage exceeds the test's bound");
    }

    fn task_settlement(&self, task_id: &str) -> Value {
        let db = self.database();
        let task: String = db
            .query_row(
                "SELECT payload_json FROM tasks WHERE task_id = ?1",
                [task_id],
                |row| row.get(0),
            )
            .unwrap();
        let task: Value = serde_json::from_str(&task).unwrap();
        assert_eq!(task["agent_id"], AGENT_ID);
        assert_eq!(task["status"], "completed");
        let mut query = db
            .prepare("SELECT payload_json FROM task_result_settlements WHERE task_id = ?1")
            .unwrap();
        let settlements = query
            .query_map([task_id], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            settlements.len(),
            1,
            "one task must have one result identity"
        );
        let settlement: Value = serde_json::from_str(&settlements[0]).unwrap();
        assert_eq!(settlement["agent_id"], task["agent_id"]);
        assert_eq!(settlement["work_item_id"], task["work_item_id"]);
        settlement
    }

    fn wait_input_completed(&self, message_id: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let completed: i64 = self
                .database()
                .query_row(
                    "SELECT COUNT(*) FROM turn_records
                     WHERE agent_id = ?1 AND trigger_message_id = ?2
                       AND terminal_kind = 'completed'",
                    [AGENT_ID, message_id],
                    |row| row.get(0),
                )
                .unwrap();
            if completed > 0 {
                assert_eq!(completed, 1, "input must complete only once");
                self.assert_source_processed(message_id);
                return;
            }
            assert!(Instant::now() < deadline, "input did not complete");
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn assert_admitted_once(&self, message_id: &str) {
        let count: i64 = self
            .database()
            .query_row(
                "SELECT COUNT(*) FROM execution_protocol_attempts
             WHERE agent_id = ?1 AND json_extract(payload_json, '$.source_message_id') = ?2",
                [AGENT_ID, message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "source must have one canonical execution admission"
        );
    }

    fn spawn_serve(&mut self) -> (usize, String) {
        let address = free_address();
        let index = self.processes.len();
        let stdout = self.path(&format!("serve-{index}.stdout"));
        let stderr = self.path(&format!("serve-{index}.stderr"));
        let mut command = Command::new(holon_bin());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("HOLON_HOME", self.home.path())
            .env("HOLON_WORKSPACE_DIR", self.home.path())
            .env("HOLON_SOCKET_PATH", self.path("run/control.sock"))
            .env("HOLON_AGENT_ID", AGENT_ID)
            .env("HOLON_DAEMON_START_TIMEOUT_SECS", "60")
            .env("RUST_LOG", "info")
            .envs(self.provider_env.iter().cloned())
            .current_dir(self.home.path())
            .stdin(Stdio::null())
            .process_group(0)
            .args([
                "serve",
                "--listen",
                &address,
                "--token-file",
                "control.token",
                "--web-dist",
                "ui",
                "--desktop-integration=false",
            ])
            .stdout(Stdio::from(File::create(&stdout).unwrap()))
            .stderr(Stdio::from(File::create(&stderr).unwrap()));
        let child = command
            .spawn()
            .expect("spawn isolated compiled holon serve");
        self.processes.push(ProcessGroup {
            child,
            stdout,
            stderr,
        });
        self.wait_healthy(index, &address, DEADLINE);
        (index, address)
    }

    fn wait_healthy(&mut self, index: usize, address: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            assert!(
                self.processes[index]
                    .child
                    .try_wait()
                    .expect("poll serve")
                    .is_none(),
                "serve exited before becoming healthy:\n{}",
                self.processes[index].diagnostics()
            );
            if let Some(status) = self.try_get(address, "/api/control/runtime/status") {
                if status.is_object() {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "serve did not become healthy:\n{}",
                self.processes[index].diagnostics()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn kill(&mut self, index: usize) {
        self.processes[index].kill();
    }

    fn diagnostics(&self, index: usize) -> String {
        self.processes[index].diagnostics()
    }

    fn try_get(&self, address: &str, path: &str) -> Option<Value> {
        let response = self
            .client
            .get(format!("http://{address}{path}"))
            .bearer_auth(TOKEN)
            .send()
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().ok()
    }

    fn post_prompt(&self, address: &str, text: &str) -> String {
        let response = self
            .client
            .post(format!(
                "http://{address}/api/control/agents/{AGENT_ID}/prompt"
            ))
            .bearer_auth(TOKEN)
            .json(&json!({ "text": text }))
            .send()
            .expect("submit operator prompt");
        assert!(
            response.status().is_success(),
            "prompt rejected: {:?}",
            response.text()
        );
        let receipt: Value = response.json().expect("prompt receipt JSON");
        receipt["message_id"]
            .as_str()
            .expect("prompt receipt message id")
            .to_string()
    }

    fn initialize_agent(&self, address: &str, primary: &MockProvider) {
        let response = self
            .client
            .post(format!(
                "http://{address}/api/control/agents/{AGENT_ID}/create"
            ))
            .bearer_auth(TOKEN)
            .json(&json!({}))
            .send()
            .expect("create isolated test agent");
        assert!(
            response.status().is_success(),
            "agent creation rejected: {:?}",
            response.text()
        );
        wait_for_brief_marker(self, address, "provider-recovery-initialized", DEADLINE);
        primary.reset_hits();
        primary.set_mode(MockMode::RateLimited);
    }

    fn events(&self, address: &str) -> Vec<Value> {
        let page = self
            .try_get(
                address,
                &format!("/api/agents/{AGENT_ID}/events?limit=256&order=desc"),
            )
            .expect("HTTP event page must be available");
        let events = page["events"]
            .as_array()
            .expect("HTTP event page must contain events");
        for event in events {
            assert!(event["type"].is_string(), "HTTP event type is required");
            assert!(
                event["payload"].is_object(),
                "HTTP event payload is required"
            );
        }
        events.clone()
    }

    fn paused_events(&self, address: &str) -> Vec<Value> {
        self.events(address)
            .into_iter()
            .filter(|event| {
                matches!(
                    event["type"].as_str(),
                    Some("provider_dispatch_paused" | "provider_queue_parked")
                )
            })
            .collect()
    }
}

fn json_contains(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains(needle),
        Value::Array(items) => items.iter().any(|item| json_contains(item, needle)),
        Value::Object(fields) => fields.values().any(|item| json_contains(item, needle)),
        _ => false,
    }
}

/// Observe the committed scheduler or dispatch park, not just one event path.
fn wait_for_parked_window(
    fixture: &Fixture,
    address: &str,
    window: Duration,
    timeout: Duration,
) -> ParkedInput {
    let deadline = Instant::now() + timeout;
    loop {
        let paused = fixture.paused_events(address);
        for parked in fixture.parked_inputs() {
            if parked.not_before.signed_duration_since(Utc::now())
                >= chrono::Duration::from_std(window).unwrap()
                && paused.iter().any(|event| {
                    event["payload"]["message_id"] == parked.message_id
                        && event["payload"]["not_before"].as_str().is_some_and(|at| {
                            DateTime::parse_from_rfc3339(at).is_ok_and(|at| at == parked.not_before)
                        })
                })
            {
                return parked;
            }
        }
        assert!(
            Instant::now() < deadline,
            "no committed park window of at least {window:?} appeared; paused events: {paused:#?}\n{}",
            fixture
                .processes
                .iter()
                .map(|process| process.diagnostics())
                .collect::<Vec<_>>()
                .join("\n")
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_brief_marker(fixture: &Fixture, address: &str, marker: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(briefs) =
            fixture.try_get(address, &format!("/api/agents/{AGENT_ID}/briefs?limit=64"))
        {
            if json_contains(&briefs, marker) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "final brief containing {marker:?} never appeared\n{}",
            fixture
                .processes
                .iter()
                .map(|process| process.diagnostics())
                .collect::<Vec<_>>()
                .join("\n")
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn primary_env(primary: &MockProvider) -> Vec<(String, String)> {
    vec![
        ("HOLON_MODEL".to_string(), "openai/gpt-5.4".to_string()),
        (
            "OPENAI_API_KEY".to_string(),
            "provider-recovery-test-key".to_string(),
        ),
        (
            "OPENAI_BASE_URL".to_string(),
            format!("http://{}", primary.address),
        ),
    ]
}

fn fallback_env(fallback: &MockProvider) -> Vec<(String, String)> {
    vec![
        (
            "HOLON_MODEL_FALLBACKS".to_string(),
            "anthropic/claude-sonnet-4-6".to_string(),
        ),
        (
            "ANTHROPIC_AUTH_TOKEN".to_string(),
            "provider-recovery-test-key".to_string(),
        ),
        (
            "ANTHROPIC_BASE_URL".to_string(),
            format!("http://{}", fallback.address),
        ),
    ]
}

#[test]
fn cooldown_park_survives_sigkill_and_resumes_on_schedule_without_new_input() {
    let _lock = server_lock();
    let primary = MockProvider::start(MockMode::Initialize, "primary-recovered-final", false);
    let mut fixture = Fixture::new(primary_env(&primary));
    let (serve, address) = fixture.spawn_serve();
    fixture.initialize_agent(&address, &primary);
    fixture.post_prompt(&address, "recover after the restart");

    let parked = wait_for_parked_window(&fixture, &address, PARK_WINDOW, DEADLINE);
    let parked_until = parked.not_before;
    assert!(primary.rate_limited_hits() >= 1);

    // Crash the whole process group while the input is parked, then let every
    // future model request succeed before the replacement process starts.
    fixture.kill(serve);
    primary.set_mode(MockMode::Success);
    let (_resumed, address) = fixture.spawn_serve();
    let restarted_at = Utc::now();
    assert!(
        restarted_at < parked_until,
        "replacement daemon became healthy at {restarted_at} only after the parked deadline {parked_until}; \
         scheduled-recovery proof is void"
    );

    fixture.assert_park_retained(&parked);
    // No new message is submitted: recovery must be driven by the persisted
    // park deadline alone.
    wait_for_brief_marker(&fixture, &address, "primary-recovered-final", DEADLINE);
    let final_dispatch_at = primary
        .last_success_at()
        .expect("recovered dispatch must reach the provider");
    assert!(
        final_dispatch_at + chrono::Duration::milliseconds(300) >= parked_until,
        "recovered dispatch happened before the persisted park deadline"
    );
    assert_eq!(
        primary.success_hits(),
        1,
        "recovery must dispatch exactly one successful model request"
    );
    fixture.assert_source_processed(&parked.message_id);
    let next = fixture.post_prompt(&address, "confirm the ordinary input queue remains live");
    fixture.wait_input_completed(&next);
    assert_eq!(primary.success_hits(), 2);
}

#[test]
fn cooling_primary_uses_healthy_fallback_and_never_parks() {
    let _lock = server_lock();
    let primary = MockProvider::start(MockMode::Initialize, "primary-recovered-final", false);
    let fallback = MockProvider::start(MockMode::Success, "fallback-recovered-final", false);
    let mut env = primary_env(&primary);
    env.extend(fallback_env(&fallback));
    let mut fixture = Fixture::new(env);
    let (_serve, address) = fixture.spawn_serve();
    fixture.initialize_agent(&address, &primary);
    fixture.post_prompt(&address, "use the fallback while the primary cools");

    wait_for_brief_marker(&fixture, &address, "fallback-recovered-final", DEADLINE);
    assert!(
        primary.rate_limited_hits() >= 1,
        "the primary must have been attempted and cooled down"
    );
    assert_eq!(
        primary.success_hits(),
        0,
        "the cooling primary must not serve the turn"
    );
    assert_eq!(
        fallback.success_hits(),
        1,
        "the healthy fallback must serve exactly one dispatch"
    );
    let paused = fixture.paused_events(&address);
    assert!(
        paused.is_empty(),
        "a healthy fallback must prevent parking: {paused:#?}"
    );
}

#[test]
fn task_result_continuation_parked_in_cooldown_survives_restart_and_finishes_once() {
    let _lock = server_lock();
    let primary = MockProvider::start(MockMode::Initialize, "task-result-recovered-final", false);
    let mut fixture = Fixture::new(primary_env(&primary));
    let (serve, address) = fixture.spawn_serve();
    fixture.initialize_agent(&address, &primary);
    primary.arm_tool_call(fixture.path("completed-probe-count"));
    fixture.post_prompt(&address, "run the probe tool and then answer");

    // The first turn must complete with the one-shot tool call before any
    // cooldown park exists; the ExecCommand task result then re-enters the
    // model while the mock only offers 429.
    let tool_deadline = Instant::now() + DEADLINE;
    while primary.success_hits() < 1 {
        assert!(
            Instant::now() < tool_deadline,
            "tool-call turn never dispatched:\n{}",
            fixture.diagnostics(serve)
        );
        thread::sleep(Duration::from_millis(100));
    }

    let parked = wait_for_parked_window(&fixture, &address, PARK_WINDOW, DEADLINE);
    let parked_until = parked.not_before;
    let task_id = primary.task_id();
    let result_message_id = fixture.assert_task_source(&parked, &task_id);
    let original_settlement = fixture.task_settlement(&task_id);
    assert_eq!(original_settlement["message_id"], result_message_id);
    assert_eq!(
        fs::read_to_string(fixture.path("completed-probe-count")).unwrap(),
        "ran\n"
    );

    fixture.kill(serve);
    primary.set_mode(MockMode::Success);
    let (_resumed, address) = fixture.spawn_serve();
    let restarted_at = Utc::now();
    assert!(restarted_at < parked_until);
    fixture.assert_park_retained(&parked);
    assert_eq!(
        fixture.assert_task_source(&parked, &task_id),
        result_message_id
    );

    wait_for_brief_marker(&fixture, &address, "task-result-recovered-final", DEADLINE);
    fixture.wait_input_completed(&parked.message_id);
    let final_dispatch_at = primary
        .last_success_at()
        .expect("recovered continuation must reach the provider");
    assert!(final_dispatch_at + chrono::Duration::milliseconds(300) >= parked_until);
    assert_eq!(
        primary.success_hits(),
        3,
        "exactly the tool call, exact wait and one recovered continuation may succeed"
    );
    assert!(primary.rate_limited_hits() >= 1);
    fixture.assert_source_processed(&parked.message_id);
    fixture.assert_admitted_once(&result_message_id);
    fixture.assert_admitted_once(&parked.message_id);
    let settlement = fixture.task_settlement(&task_id);
    for key in [
        "result_identity",
        "message_id",
        "work_item_id",
        "rejoin_generation",
        "parent_turn_id",
    ] {
        assert_eq!(
            settlement[key], original_settlement[key],
            "task responsibility changed: {key}"
        );
    }
    assert_eq!(settlement["state"], "settled");
    assert_eq!(settlement["disposition"], "model_delivered");
    let next = fixture.post_prompt(&address, "confirm task recovery leaves the queue live");
    fixture.wait_input_completed(&next);
    assert_eq!(primary.success_hits(), 4);
    assert_eq!(
        fs::read_to_string(fixture.path("completed-probe-count")).unwrap(),
        "ran\n"
    );
}

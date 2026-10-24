//! Exercise terminal acquisition and cleanup against an isolated read-only control plane.
#![cfg(unix)]

use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Read, Write},
    net::TcpListener,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{net::UnixListener, process::CommandExt},
    },
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

trait Connection: Read + Write + Send {}
impl<T: Read + Write + Send> Connection for T {}

enum Listener {
    Local(UnixListener),
    Remote(TcpListener),
}

impl Listener {
    fn accept(&self) -> Box<dyn Connection> {
        match self {
            Self::Local(listener) => {
                let stream = listener.accept().unwrap().0;
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                Box::new(stream)
            }
            Self::Remote(listener) => {
                let stream = listener.accept().unwrap().0;
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                Box::new(stream)
            }
        }
    }
}

fn request_path(stream: &mut dyn Connection) -> String {
    let mut bytes = vec![];
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        assert_eq!(stream.read(&mut byte).unwrap(), 1);
        bytes.push(byte[0]);
        assert!(bytes.len() < 16_384);
    }
    let request = String::from_utf8(bytes).unwrap();
    let mut first_line = request.lines().next().unwrap().split_whitespace();
    assert_eq!(first_line.next(), Some("GET"), "no mutation is allowed");
    first_line.next().unwrap().to_string()
}

fn respond(stream: &mut dyn Connection, status: u16, body: Value) {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}

struct PtyChild {
    child: Child,
    master: File,
    slave: File,
    original: libc::termios,
}

impl PtyChild {
    fn spawn(command: &mut Command) -> Self {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes both descriptors; the size pointer is valid.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            },
            0
        );
        // SAFETY: each descriptor is owned exactly once by its File.
        let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        let original = termios(&slave);
        assert_ne!(original.c_lflag & libc::ICANON, 0);
        let slave_fd = slave.as_raw_fd();
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        // SAFETY: only async-signal-safe syscalls are invoked before exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0
                    || libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
            assert_ne!(
                libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK),
                -1
            );
        }
        Self {
            child: command.spawn().unwrap(),
            master,
            slave,
            original,
        }
    }

    fn output(&mut self) -> String {
        let mut bytes = vec![];
        let mut buffer = [0; 8192];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) if err.raw_os_error() == Some(libc::EIO) => break,
                Err(err) => panic!("read PTY: {err}"),
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn wait(&mut self) -> (ExitStatus, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = String::new();
        loop {
            output.push_str(&self.output());
            if let Some(status) = self.child.try_wait().unwrap() {
                output.push_str(&self.output());
                return (status, output);
            }
            assert!(Instant::now() < deadline, "TUI failed to exit: {output}");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn termios(file: &File) -> libc::termios {
    let mut value = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr initializes value on success.
    assert_eq!(
        unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) },
        0
    );
    unsafe { value.assume_init() }
}

fn assert_cooked_unchanged(child: &PtyChild) {
    let current = termios(&child.slave);
    assert_eq!(current.c_iflag, child.original.c_iflag);
    assert_eq!(current.c_oflag, child.original.c_oflag);
    assert_eq!(current.c_cflag, child.original.c_cflag);
    assert_eq!(current.c_lflag, child.original.c_lflag);
    assert_eq!(current.c_cc, child.original.c_cc);
}

fn assert_terminal_boundary(remote: bool, bootstrap_race: bool) {
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("control.sock");
    let mut command = Command::new(env!("CARGO_BIN_EXE_holon"));
    command
        .env_clear()
        .env("HOME", home.path())
        .env("HOLON_HOME", home.path())
        .env("HOLON_SOCKET_PATH", &socket)
        .env("HOLON_MODEL", "openai/gpt-5.4")
        .env("TERM", "xterm-256color")
        .args(["tui", "--agent", "beta"]);
    let listener = if remote {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        command.args([
            "--connect",
            &format!("http://{}", listener.local_addr().unwrap()),
            "--token",
            "private-test-token",
        ]);
        Listener::Remote(listener)
    } else {
        Listener::Local(UnixListener::bind(&socket).unwrap())
    };
    let (reached_tx, reached_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut paths = vec![];
        if remote {
            let mut stream = listener.accept();
            paths.push(request_path(&mut *stream));
            assert_eq!(paths.last().unwrap(), "/api/handshake");
            respond(&mut *stream, 200, json!({"ok": true}));
        }
        let mut stream = listener.accept();
        paths.push(request_path(&mut *stream));
        assert_eq!(paths.last().unwrap(), "/api/agents/snapshot");
        let mut entries: Value =
            serde_json::from_str(include_str!("fixtures/client-wire/agent-list-v1.json")).unwrap();
        entries[0]["identity"]["agent_id"] = json!("beta");
        entries[0]["status"] = json!("stopped");
        let agent = entries[0].clone();
        respond(
            &mut *stream,
            200,
            json!({
                "contract_version": 1,
                "runtime_id": "isolated",
                "event_log_epoch": "epoch",
                "visibility_scope_id": "public",
                "agents": [{
                    "agent": agent,
                    "event_window": {"event_head_seq": 0, "oldest_retained_seq": 0},
                    "latest_brief": null
                }]
            }),
        );
        // The Unix transport completes its response at EOF.
        drop(stream);
        let mut stream = listener.accept();
        paths.push(request_path(&mut *stream));
        assert_eq!(
            paths.last().unwrap(),
            "/api/agents/beta/status?detail=compact"
        );
        let mut stream = if bootstrap_race {
            respond(
                &mut *stream,
                200,
                json!({
                    "identity": agent["identity"],
                    "agent": {"id": "beta", "status": "stopped", "pending": 0},
                    "model": agent["model"],
                    "closure": {"outcome": "completed", "runtime_posture": "sleeping"}
                }),
            );
            drop(stream);
            let mut stream = listener.accept();
            paths.push(request_path(&mut *stream));
            assert_eq!(paths.last().unwrap(), "/api/agents/beta/state");
            stream
        } else {
            stream
        };
        reached_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        respond(
            &mut *stream,
            410,
            json!({
                "error": "agent beta has been deleted",
                "code": "agent_deleted",
                "hint": "choose another public agent"
            }),
        );
        paths
    });
    let mut child = PtyChild::spawn(&mut command);
    reached_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|err| {
            panic!(
                "control-plane gate not reached: {err}; child={:?}; output={}",
                child.child.try_wait().unwrap(),
                child.output()
            )
        });
    let mut output = child.output();
    if bootstrap_race {
        let deadline = Instant::now() + Duration::from_secs(5);
        while termios(&child.slave).c_lflag & libc::ICANON != 0 {
            assert!(
                Instant::now() < deadline,
                "TUI never entered raw mode: {output}"
            );
            thread::sleep(Duration::from_millis(10));
            output.push_str(&child.output());
        }
    } else {
        // The targeted response is still pending: terminal acquisition must not have begun.
        assert_cooked_unchanged(&child);
        assert!(!output.contains("\x1b[?1049h"));
    }
    release_tx.send(()).unwrap();
    let (status, remaining) = child.wait();
    output.push_str(&remaining);
    assert_eq!(status.code(), Some(1), "{output}");
    assert!(output.contains("agent_deleted"), "{output}");
    assert!(!output.contains("private-test-token"), "{output}");
    assert_cooked_unchanged(&child);
    if bootstrap_race {
        assert!(output.contains("\x1b[?1049h"), "{output}");
        assert!(output.contains("\x1b[?1049l"), "{output}");
        assert!(
            output.contains("failed to bootstrap startup agent beta"),
            "{output}"
        );
    } else {
        assert!(!output.contains("\x1b[?1049h"), "{output}");
        assert!(output.contains("agent beta has been deleted"), "{output}");
    }
    let paths = server.join().unwrap();
    assert_eq!(
        paths.len(),
        2 + usize::from(remote) + usize::from(bootstrap_race)
    );
    assert!(!home.path().join("state/tui").exists());
}

#[test]
fn local_explicit_target_is_preflighted_before_raw_mode() {
    assert_terminal_boundary(false, false);
}

#[test]
fn remote_explicit_target_is_preflighted_after_handshake_before_raw_mode() {
    assert_terminal_boundary(true, false);
}

#[test]
fn local_post_preflight_deletion_restores_terminal_without_fallback() {
    assert_terminal_boundary(false, true);
}

#[test]
fn remote_post_preflight_deletion_restores_terminal_without_fallback() {
    assert_terminal_boundary(true, true);
}

use super::*;

fn retained(path: &Path, segments: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for index in (0..segments).rev() {
        if let Ok(part) = fs::read(segment_path(path, index)) {
            bytes.extend(part);
        }
    }
    bytes
}

#[test]
fn rolling_log_waits_for_previous_owner_before_mutating_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let (mut first, _) = RollingLog::open(&path, 64, 4).unwrap();
    first.write_all(b"first owner\n").unwrap();
    let previous = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        first.write_all(b"tail").unwrap();
    });
    let next = RollingLog::open(&path, 4, 4);
    previous.join().unwrap();
    let (_next, discarded) = next.expect("new owner must wait for the previous collector");
    assert_eq!(discarded, 12);
    assert_eq!(fs::read(&path).unwrap(), b"tail");
}

#[test]
fn rolling_log_rejects_a_second_owner_before_mutating_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let (mut first, _) = RollingLog::open(&path, 64, 4).unwrap();
    first.write_all(b"first owner\n").unwrap();
    let started = Instant::now();
    let error = RollingLog::open_with_owner_timeout(&path, 4, 4, Duration::from_millis(20))
        .err()
        .expect("a live owner must exclude competing writers");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() >= Duration::from_millis(20));
    assert_eq!(fs::read(&path).unwrap(), b"first owner\n");
}

fn assert_bounds(path: &Path, limit: u64, segments: usize) {
    let mut total = 0;
    for index in 0..segments {
        if let Ok(metadata) = fs::metadata(segment_path(path, index)) {
            assert!(metadata.len() <= limit);
            total += metadata.len();
        }
    }
    assert!(total <= limit * segments as u64);
    assert!(!segment_path(path, segments).exists());
}

#[test]
fn rolling_log_bounds_large_writes_and_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let unrelated = directory.path().join("unrelated.log");
    fs::write(&unrelated, b"preserve").unwrap();
    let input = (0..4096).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let (mut log, discarded) = RollingLog::open(&path, 128, 4).unwrap();
    assert_eq!(discarded, 0);
    log.write_all(&input).unwrap();
    assert_bounds(&path, 128, 4);
    assert_eq!(retained(&path, 4), input[input.len() - 512..]);
    drop(log);
    let (mut log, _) = RollingLog::open(&path, 128, 4).unwrap();
    log.write_all(b"restart").unwrap();
    assert_bounds(&path, 128, 4);
    assert!(retained(&path, 4).ends_with(b"restart"));
    assert_eq!(fs::read(unrelated).unwrap(), b"preserve");
}

#[test]
fn rolling_log_normalizes_oversized_history_without_unbounded_backup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    for index in 0..4 {
        let mut file = File::create(segment_path(&path, index)).unwrap();
        file.set_len(1024 * 1024 * 1024).unwrap(); // sparse historical log
        file.seek(SeekFrom::End(-8)).unwrap();
        file.write_all(b"recent!\n").unwrap();
    }
    let (log, discarded) = RollingLog::open(&path, 64, 4).unwrap();
    assert_eq!(discarded, 4 * (1024 * 1024 * 1024 - 64));
    assert_bounds(&path, 64, 4);
    assert!(retained(&path, 4).ends_with(b"recent!\n"));
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 5); // segments + owner lock
    drop(log);
}

#[test]
fn rolling_log_initialization_errors_are_not_an_append_fallback() {
    let directory = tempfile::tempdir().unwrap();
    assert!(RollingLog::open(&directory.path().join("missing/daemon.log"), 64, 4).is_err());
    let path = directory.path().join("daemon.log");
    fs::create_dir(&path).unwrap();
    assert!(RollingLog::open(&path, 64, 4).is_err());
}

struct FailingWriter {
    bytes: Vec<u8>,
    fail: bool,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail {
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        }
        // Exercise partial writes, including recovery of a buffered suffix.
        let count = bytes.len().min(7);
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn collector_disk_full_is_bounded_visible_and_recovers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let writer = FailingWriter {
        bytes: Vec::new(),
        fail: true,
    };
    let mut collector = LogCollector::new(writer, &path, 0).unwrap();
    collector.ingest(b"first\n");
    let health = read_log_health(&path).unwrap().unwrap();
    assert!(health.degraded);
    assert_eq!(health.buffered_bytes, 6);
    let input = vec![b'x'; BUFFER_BYTES * 3];
    collector.ingest(&input);
    assert_eq!(collector.pending.len(), BUFFER_BYTES);
    assert_eq!(
        collector.health.dropped_bytes,
        (6 + BUFFER_BYTES * 2) as u64
    );
    collector.writer.fail = false;
    collector.retry_at = None;
    collector.ingest(b"recovered\n");
    let health = read_log_health(&path).unwrap().unwrap();
    assert!(!health.degraded);
    assert_eq!(health.buffered_bytes, 0);
    assert!(health.last_error.is_some());
    assert_eq!(collector.writer.bytes.len(), BUFFER_BYTES + 10);
    assert!(collector.writer.bytes.ends_with(b"recovered\n"));
    assert_eq!(
        fs::metadata(status_path(&path)).unwrap().len(),
        STATUS_BYTES as u64
    );
}

#[test]
fn collector_publishes_burst_loss_without_new_output() {
    struct BurstThenQuiet {
        input: io::Cursor<Vec<u8>>,
        path: PathBuf,
        timed_out: bool,
    }

    impl Read for BurstThenQuiet {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = self.input.read(bytes)?;
            if count > 0 {
                return Ok(count);
            }
            if !self.timed_out {
                self.timed_out = true;
                std::thread::sleep(RETRY_INTERVAL);
                return Err(io::ErrorKind::TimedOut.into());
            }
            // No new output or shutdown has triggered this publication.
            let health = read_log_health(&self.path).unwrap().unwrap();
            assert!(health.degraded);
            assert_eq!(health.buffered_bytes, BUFFER_BYTES);
            assert_eq!(health.dropped_bytes, (BUFFER_BYTES * 2) as u64);
            Ok(0)
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let writer = FailingWriter {
        bytes: Vec::new(),
        fail: true,
    };
    let mut collector = LogCollector::new(writer, &path, 0).unwrap();
    let mut reader = BurstThenQuiet {
        input: io::Cursor::new(vec![b'x'; BUFFER_BYTES * 3]),
        path,
        timed_out: false,
    };
    collector.drain(&mut reader);
}

#[test]
fn collector_rotation_failure_never_appends_past_limit_and_accounts_shutdown_loss() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let (writer, _) = RollingLog::open(&path, 32, 4).unwrap();
    let mut collector = LogCollector::new(writer, &path, 0).unwrap();
    collector.ingest(&[b'a'; 32]);
    fs::create_dir(segment_path(&path, 3)).unwrap();
    collector.ingest(b"not-written");
    assert_eq!(fs::metadata(&path).unwrap().len(), 32);
    assert!(read_log_health(&path).unwrap().unwrap().degraded);
    collector.finish();
    let health = read_log_health(&path).unwrap().unwrap();
    assert_eq!(health.dropped_bytes, 11);
    assert_eq!(health.buffered_bytes, 0);
    fs::remove_dir(segment_path(&path, 3)).unwrap();
    // Simulate the next retry deadline without sleeping in the test.
    collector.retry_at = None;
    collector.ingest(b"recovered\n");
    assert_bounds(&path, 32, 4);
    assert!(retained(&path, 4).ends_with(b"recovered\n"));
}

#[cfg(unix)]
#[test]
fn managed_stdio_child() {
    let Some(path) = std::env::var_os("HOLON_TEST_LOG_CAPTURE") else {
        return;
    };
    let guard = super::stdio::ManagedDaemonLog::install(Path::new(&path), 256, 4).unwrap();
    // A descendant retaining stdout cannot hold shutdown/drain hostage.
    let mut descendant = std::process::Command::new("sleep")
        .arg("2")
        .spawn()
        .unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..100 {
                io::stdout().write_all(b"out\n").unwrap();
            }
        });
        scope.spawn(|| {
            for _ in 0..100 {
                io::stderr().write_all(b"err\n").unwrap();
            }
        });
    });
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(io::stderr)
        .finish();
    tracing::subscriber::with_default(subscriber, || tracing::warn!("tracing captured"));
    drop(guard);
    let _ = descendant.kill();
    let _ = descendant.wait();
}

#[cfg(unix)]
#[test]
fn managed_stdio_captures_native_output_and_drains_without_helper_process() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("daemon.log");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "daemon::logging::tests::managed_stdio_child",
            "--nocapture",
        ])
        .env("HOLON_TEST_LOG_CAPTURE", &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(child.status.success(), "{:?}", child);
    assert_bounds(&path, 256, 4);
    let text = String::from_utf8(retained(&path, 4)).unwrap();
    assert_eq!(text.matches("out\n").count(), 100);
    assert_eq!(text.matches("err\n").count(), 100);
    assert!(text.contains("tracing captured"));
    assert!(!text.contains('\u{1b}'));
    assert!(!read_log_health(&path).unwrap().unwrap().degraded);
}

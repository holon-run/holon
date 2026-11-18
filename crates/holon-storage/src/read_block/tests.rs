use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use super::*;

static THRESHOLD_TEST_LOCK: Mutex<()> = Mutex::new(());

struct ThresholdGuard {
    previous: u64,
}

impl Drop for ThresholdGuard {
    fn drop(&mut self) {
        SLOW_READ_MS.store(self.previous, Ordering::Relaxed);
    }
}

fn override_threshold(ms: u64) -> (MutexGuard<'static, ()>, ThresholdGuard) {
    let lock = THRESHOLD_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let previous = SLOW_READ_MS.load(Ordering::Relaxed);
    SLOW_READ_MS.store(ms, Ordering::Relaxed);
    (lock, ThresholdGuard { previous })
}

fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone)]
    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let output = Arc::new(Mutex::new(Vec::new()));
    let writer_output = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || LogWriter(writer_output.clone()))
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = output.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn slow_read_threshold_parsing_defaults_and_disables() {
    assert_eq!(parse_slow_read_ms(None), DEFAULT_SLOW_READ_MS);
    assert_eq!(parse_slow_read_ms(Some("")), DEFAULT_SLOW_READ_MS);
    assert_eq!(
        parse_slow_read_ms(Some("not-a-number")),
        DEFAULT_SLOW_READ_MS
    );
    assert_eq!(parse_slow_read_ms(Some(" 250 ")), 250);
    assert_eq!(parse_slow_read_ms(Some("0")), 0);
}

#[test]
fn sql_summary_collapses_whitespace_and_truncates() {
    assert_eq!(
        summarize_sql("SELECT   data_json\n  FROM audit_events\tLIMIT ?1"),
        "SELECT data_json FROM audit_events LIMIT ?1"
    );
    let summary = summarize_sql(&"x".repeat(MAX_LOGGED_SQL_CHARS + 32));
    assert_eq!(summary.chars().count(), MAX_LOGGED_SQL_CHARS + 1);
    assert!(summary.ends_with('…'));
}

#[test]
fn slow_read_block_reports_call_site_and_split() {
    let (_lock, _threshold) = override_threshold(100);
    let logs = capture_logs(|| {
        record_read_block(
            ReadOrigin {
                file: "src/runtime_db/repositories.rs",
                line: 5085,
            },
            Duration::from_millis(20),
            Duration::from_millis(1_500),
        );
    });
    assert!(logs.contains("slow runtime db read"), "{logs}");
    assert!(
        logs.contains(r#"origin_file="src/runtime_db/repositories.rs""#),
        "{logs}"
    );
    assert!(logs.contains("origin_line=5085"), "{logs}");
    assert!(logs.contains("checkout_ms=20"), "{logs}");
    assert!(logs.contains("operation_ms=1500"), "{logs}");
}

#[test]
fn slow_sql_statement_reports_read_label_sql_and_call_site() {
    let (_lock, _threshold) = override_threshold(100);
    let logs = capture_logs(|| {
        let _label = ReadContextGuard::enter_label("http.events.page");
        let _origin = ReadContextGuard::enter_origin(ReadOrigin {
            file: "src/runtime_db/repositories.rs",
            line: 5085,
        });
        profile_statement(
            "SELECT   data_json\nFROM audit_events",
            Duration::from_millis(1_500),
        );
    });
    assert!(logs.contains("slow sqlite statement"), "{logs}");
    assert!(
        logs.contains("sql=SELECT data_json FROM audit_events"),
        "{logs}"
    );
    assert!(logs.contains(r#"read="http.events.page""#), "{logs}");
    assert!(logs.contains("origin_line=5085"), "{logs}");
    assert!(logs.contains("elapsed_ms=1500"), "{logs}");
}

#[test]
fn reads_below_the_threshold_stay_silent() {
    let (_lock, _threshold) = override_threshold(1_000);
    let logs = capture_logs(|| {
        profile_statement("SELECT 1", Duration::from_millis(10));
        record_read_block(
            ReadOrigin::UNKNOWN,
            Duration::from_millis(1),
            Duration::from_millis(10),
        );
    });
    assert!(!logs.contains("slow sqlite statement"), "{logs}");
    assert!(!logs.contains("slow runtime db read"), "{logs}");
}

#[test]
fn read_context_nests_label_and_origin() {
    let _label = ReadContextGuard::enter_label("http.tasks.list");
    assert_eq!(current_label(), Some("http.tasks.list"));
    {
        let _origin = ReadContextGuard::enter_origin(ReadOrigin {
            file: "src/a.rs",
            line: 7,
        });
        let context = current_context().expect("context");
        assert_eq!(context.label, Some("http.tasks.list"));
        assert_eq!(context.origin.map(|origin| origin.line), Some(7));
    }
    let context = current_context().expect("context");
    assert_eq!(context.label, Some("http.tasks.list"));
    assert!(context.origin.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_read_leaves_the_async_worker_free() {
    let started = Instant::now();
    let read = run_blocking("test.off_worker", Duration::from_secs(30), || {
        std::thread::sleep(Duration::from_millis(150));
        Ok(())
    });
    let mut read = std::pin::pin!(read);
    let timer = tokio::time::sleep(Duration::from_millis(20));
    tokio::pin!(timer);
    // On a single-threaded runtime this timer can only fire while the read
    // sleeps on the blocking pool.
    tokio::select! {
        () = &mut timer => {}
        result = &mut read => panic!("blocking read completed inline: {result:?}"),
    }
    read.await.expect("blocking read");
    assert!(started.elapsed() >= Duration::from_millis(150));
}

#[tokio::test]
async fn blocking_read_scopes_only_its_own_thread() {
    let observed = run_blocking("test.scope", Duration::from_secs(30), || {
        Ok(current_scope().map(|scope| scope.deadline()))
    })
    .await
    .expect("blocking read")
    .expect("read scope");
    assert!(observed > Instant::now());
    assert!(current_scope().is_none(), "scope leaked to the caller");
}

//! Startup timing only: these observations never gate or schedule recovery.
use std::{future::Future, time::Instant};

#[cfg(test)]
use std::{pin::Pin, sync::Arc};

use anyhow::Result;

#[cfg(test)]
pub(crate) type StartupTestHook = dyn Fn(StartupObservation, &'static str) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
    + Send
    + Sync;

#[derive(Clone)]
pub struct StartupObservation {
    pub startup_id: String,
    pub agent_id: Option<String>,
    pub generation: Option<u64>,
    #[cfg(test)]
    pub(crate) test_hook: Option<Arc<StartupTestHook>>,
}

impl std::fmt::Debug for StartupObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartupObservation")
            .field("startup_id", &self.startup_id)
            .field("agent_id", &self.agent_id)
            .field("generation", &self.generation)
            .finish()
    }
}

impl Default for StartupObservation {
    fn default() -> Self {
        Self::new()
    }
}

impl StartupObservation {
    pub fn new() -> Self {
        Self::with_id(crate::ids::runtime_id("startup"))
    }

    pub fn with_id(startup_id: String) -> Self {
        Self {
            startup_id,
            agent_id: None,
            generation: None,
            #[cfg(test)]
            test_hook: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_hook(mut self, hook: Arc<StartupTestHook>) -> Self {
        self.test_hook = Some(hook);
        self
    }

    pub fn for_agent(&self, agent_id: &str, generation: Option<u64>) -> Self {
        Self {
            startup_id: self.startup_id.clone(),
            agent_id: Some(agent_id.to_string()),
            generation,
            #[cfg(test)]
            test_hook: self.test_hook.clone(),
        }
    }

    pub fn phase(&self, phase: &'static str) -> StartupPhase {
        let observation = StartupPhase {
            observation: self.clone(),
            phase,
            started: Instant::now(),
            finished: false,
        };
        observation.emit("started", "pending");
        observation
    }

    pub fn observe_sync<T>(&self, phase: &'static str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let observation = self.phase(phase);
        let result = f();
        observation.finish(&result);
        result
    }

    pub async fn observe<T>(
        &self,
        phase: &'static str,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let observation = self.phase(phase);
        #[cfg(test)]
        if let Some(hook) = &self.test_hook {
            if let Err(error) = hook(self.clone(), phase).await {
                let result = Err(error);
                observation.finish(&result);
                return result;
            }
        }
        let result = future.await;
        observation.finish(&result);
        result
    }
}

pub struct StartupPhase {
    observation: StartupObservation,
    phase: &'static str,
    started: Instant,
    finished: bool,
}

impl StartupPhase {
    pub fn finish<T>(mut self, result: &Result<T>) {
        self.emit("finished", if result.is_ok() { "ok" } else { "error" });
        self.finished = true;
    }

    fn emit(&self, state: &str, outcome: &str) {
        tracing::info!(
            startup_id = %self.observation.startup_id,
            agent_id = self.observation.agent_id.as_deref(),
            generation = self.observation.generation,
            phase = self.phase,
            state,
            outcome,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            error_class = if outcome == "error" { self.phase } else { "" },
            "startup phase"
        );
    }
}

impl Drop for StartupPhase {
    fn drop(&mut self) {
        if !self.finished {
            self.emit("finished", "incomplete");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::{pending, Future},
        io::{self, Write},
        sync::{Arc, Mutex},
        task::{Context, Poll},
    };

    use futures_util::{task::noop_waker_ref, FutureExt};

    use super::*;

    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture_logs(f: impl FnOnce()) -> String {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer_output = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_writer(move || LogWriter(writer_output.clone()))
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = output.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    fn observation() -> StartupObservation {
        StartupObservation::with_id("startup-test".to_string()).for_agent("agent-test", Some(7))
    }

    fn assert_phase_pair(output: &str, phase: &str, outcome: &str) {
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 2, "{output}");
        for line in &lines {
            assert!(line.contains("startup_id=startup-test"), "{line}");
            assert!(line.contains("agent_id=\"agent-test\""), "{line}");
            assert!(line.contains("generation=7"), "{line}");
            assert!(line.contains(&format!("phase=\"{phase}\"")), "{line}");
            assert!(line.contains("elapsed_ms="), "{line}");
        }
        assert!(lines[0].contains("state=\"started\""), "{output}");
        assert!(lines[0].contains("outcome=\"pending\""), "{output}");
        assert!(lines[1].contains("state=\"finished\""), "{output}");
        assert!(
            lines[1].contains(&format!("outcome=\"{outcome}\"")),
            "{output}"
        );
    }

    #[test]
    fn synchronous_success_records_correlated_start_and_finish() {
        let output = capture_logs(|| {
            let result = observation().observe_sync("sync.success", || Ok(42));
            assert_eq!(result.unwrap(), 42);
        });
        assert_phase_pair(&output, "sync.success", "ok");
    }

    #[test]
    fn asynchronous_success_records_correlated_start_and_finish() {
        let output = capture_logs(|| {
            let observation = observation();
            let result = observation
                .observe("async.success", async { Ok(42) })
                .now_or_never()
                .expect("ready observation");
            assert_eq!(result.unwrap(), 42);
        });
        assert_phase_pair(&output, "async.success", "ok");
    }

    #[test]
    fn inherited_test_hook_gates_real_future_with_agent_and_phase_context() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let hook_gate = gate.clone();
        let hook_events = events.clone();
        let observation = StartupObservation::with_id("startup-test".to_string())
            .with_test_hook(Arc::new(move |observation, phase| {
                let gate = hook_gate.clone();
                let events = hook_events.clone();
                Box::pin(async move {
                    assert_eq!(observation.startup_id, "startup-test");
                    assert_eq!(observation.agent_id.as_deref(), Some("agent-test"));
                    assert_eq!(observation.generation, Some(7));
                    assert_eq!(phase, "runtime.bootstrap.tasks");
                    events.lock().unwrap().push("hook-started");
                    gate.notified().await;
                    events.lock().unwrap().push("hook-finished");
                    Ok(())
                })
            }))
            .for_agent("agent-test", Some(7));

        let output = capture_logs(|| {
            let mut future = Box::pin(observation.observe("runtime.bootstrap.tasks", async {
                events.lock().unwrap().push("future-polled");
                Ok(42)
            }));
            let mut context = Context::from_waker(noop_waker_ref());
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            assert_eq!(*events.lock().unwrap(), vec!["hook-started"]);

            gate.notify_one();
            assert!(matches!(
                future.as_mut().poll(&mut context),
                Poll::Ready(Ok(42))
            ));
            assert_eq!(
                *events.lock().unwrap(),
                vec!["hook-started", "hook-finished", "future-polled"]
            );
        });
        assert_phase_pair(&output, "runtime.bootstrap.tasks", "ok");
    }

    #[test]
    fn test_hook_error_preserves_error_without_polling_future_or_logging_secret() {
        let secret = "token=private-hook-startup-test-secret";
        let real_future_polled = std::cell::Cell::new(false);
        let output = capture_logs(|| {
            let observation = observation().with_test_hook(Arc::new(move |_, _| {
                Box::pin(async move {
                    Err(
                        anyhow::Error::new(io::Error::new(io::ErrorKind::PermissionDenied, secret))
                            .context("private hook context"),
                    )
                })
            }));
            let result: Result<()> = observation
                .observe("runtime.bootstrap.tasks", async {
                    real_future_polled.set(true);
                    Ok(())
                })
                .now_or_never()
                .expect("ready hook error");
            let error = result.unwrap_err();
            assert_eq!(error.to_string(), "private hook context");
            assert_eq!(error.root_cause().to_string(), secret);
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert!(!real_future_polled.get());
        });
        assert_phase_pair(&output, "runtime.bootstrap.tasks", "error");
        assert!(!output.contains(secret), "{output}");
        assert!(!output.contains("private hook context"), "{output}");
    }

    #[test]
    fn cancelled_test_hook_finishes_incomplete_without_polling_real_future() {
        let real_future_polled = std::cell::Cell::new(false);
        let output = capture_logs(|| {
            let observation =
                observation().with_test_hook(Arc::new(|_, _| Box::pin(pending::<Result<()>>())));
            let mut future = Box::pin(observation.observe("runtime.bootstrap.tasks", async {
                real_future_polled.set(true);
                Ok(())
            }));
            let mut context = Context::from_waker(noop_waker_ref());
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            drop(future);
            assert!(!real_future_polled.get());
        });
        assert_phase_pair(&output, "runtime.bootstrap.tasks", "incomplete");
    }

    #[test]
    fn synchronous_error_preserves_error_without_logging_secret_text() {
        let secret = "token=private-startup-test-secret";
        let output = capture_logs(|| {
            let result: Result<()> = observation().observe_sync("sync.error", || {
                Err(anyhow::anyhow!(secret).context("private startup context"))
            });
            assert_eq!(result.unwrap_err().root_cause().to_string(), secret);
        });
        assert_phase_pair(&output, "sync.error", "error");
        assert!(output.contains("error_class=\"sync.error\""), "{output}");
        assert!(!output.contains(secret), "{output}");
        assert!(!output.contains("private startup context"), "{output}");
        assert!(!output.contains("outcome=\"ok\""), "{output}");
    }

    #[test]
    fn asynchronous_error_preserves_error_without_logging_secret_text() {
        let secret = "token=private-async-startup-test-secret";
        let output = capture_logs(|| {
            let observation = observation();
            let result: Result<()> = observation
                .observe("async.error", async { Err(anyhow::anyhow!(secret)) })
                .now_or_never()
                .expect("ready observation");
            assert_eq!(result.unwrap_err().to_string(), secret);
        });
        assert_phase_pair(&output, "async.error", "error");
        assert!(output.contains("error_class=\"async.error\""), "{output}");
        assert!(!output.contains(secret), "{output}");
        assert!(!output.contains("outcome=\"ok\""), "{output}");
    }

    #[test]
    fn cancelled_future_finishes_incomplete_not_ok() {
        let output = capture_logs(|| {
            let observation = observation();
            let mut future =
                Box::pin(observation.observe("async.cancelled", pending::<Result<()>>()));
            let mut context = Context::from_waker(noop_waker_ref());
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            drop(future);
        });
        assert_phase_pair(&output, "async.cancelled", "incomplete");
        assert!(!output.contains("outcome=\"ok\""), "{output}");
    }

    #[test]
    fn unfinished_guard_finishes_incomplete_not_ok() {
        let output = capture_logs(|| {
            drop(observation().phase("guard.dropped"));
        });
        assert_phase_pair(&output, "guard.dropped", "incomplete");
        assert!(!output.contains("outcome=\"ok\""), "{output}");
    }
}

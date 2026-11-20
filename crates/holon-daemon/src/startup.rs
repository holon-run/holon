//! Startup timing only: these observations never gate or schedule recovery.
use std::{future::Future, time::Instant};

#[cfg(any(test, feature = "test-support"))]
use std::{pin::Pin, sync::Arc};

use anyhow::Result;

#[cfg(any(test, feature = "test-support"))]
pub type StartupTestHook = dyn Fn(StartupObservation, &'static str) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
    + Send
    + Sync;

#[derive(Clone)]
pub struct StartupObservation {
    pub startup_id: String,
    started_at: chrono::DateTime<chrono::Utc>,
    pub agent_id: Option<String>,
    pub generation: Option<u64>,
    #[cfg(any(test, feature = "test-support"))]
    test_hook: Option<Arc<StartupTestHook>>,
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
        Self::with_id(holon_domain::ids::runtime_id("startup"))
    }

    pub fn with_id(startup_id: String) -> Self {
        Self {
            startup_id,
            started_at: chrono::Utc::now(),
            agent_id: None,
            generation: None,
            #[cfg(any(test, feature = "test-support"))]
            test_hook: None,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_hook(mut self, hook: Arc<StartupTestHook>) -> Self {
        self.test_hook = Some(hook);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_hook(&self) -> Option<&StartupTestHook> {
        self.test_hook.as_deref()
    }

    pub fn started_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.started_at
    }

    pub fn for_agent(&self, agent_id: &str, generation: Option<u64>) -> Self {
        Self {
            startup_id: self.startup_id.clone(),
            started_at: self.started_at,
            agent_id: Some(agent_id.to_string()),
            generation,
            #[cfg(any(test, feature = "test-support"))]
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
        #[cfg(any(test, feature = "test-support"))]
        if let Some(hook) = self.test_hook() {
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
            target: "holon::startup",
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
mod tests;

//! Startup resource policy for long-lived holon processes.
//!
//! With glibc, each allocating thread can get its own arena, and an arena's
//! RSS is a high-watermark: `free` only trims the arena top, so fragmented
//! pages inside the heap are never returned to the OS. A daemon with many
//! tokio workers and blocking-pool threads therefore grows RSS without bound
//! even when live data is stable (#2931).
//!
//! The policy applied at process start:
//! - the `holon` binary installs jemalloc as the global allocator on Linux,
//!   whose decay-based purging returns freed pages to the OS;
//! - `mallopt(M_ARENA_MAX, ..)` still bounds glibc arenas for libc-side
//!   malloc users (bundled SQLite C code) that a Rust global allocator
//!   cannot intercept;
//! - tokio worker threads are capped because agent-runtime load is
//!   I/O-bound; CPU-heavy work runs on tokio's blocking pool.

use std::sync::OnceLock;

/// Environment variable overriding the tokio worker-thread count.
pub const TOKIO_WORKER_THREADS_ENV: &str = "HOLON_TOKIO_WORKER_THREADS";

/// Default upper bound for tokio worker threads in the `holon` binary.
pub const DEFAULT_TOKIO_WORKER_THREADS_CAP: usize = 8;

/// Upper bound for glibc arenas still serving libc-side malloc users.
pub const GLIBC_ARENA_MAX: i32 = 8;

static STARTUP_REPORT: OnceLock<ResourcePolicyReport> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerThreadsSource {
    DefaultCap,
    EnvOverride,
    InvalidEnvIgnored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlibcArenaCapReport {
    pub max: i32,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePolicyReport {
    /// Allocator backing Rust allocations in this process, e.g. "jemalloc"
    /// or "system". Supplied by the binary crate so it stays in sync with
    /// its `#[global_allocator]` declaration.
    pub allocator: &'static str,
    /// Resolved tokio worker-thread count.
    pub tokio_worker_threads: usize,
    /// Where the worker-thread count came from.
    pub worker_threads_source: WorkerThreadsSource,
    /// Result of the glibc arena cap attempt (glibc platforms only).
    pub glibc_arena_cap: Option<GlibcArenaCapReport>,
}

impl ResourcePolicyReport {
    pub fn startup_summary(&self) -> String {
        let workers = match self.worker_threads_source {
            WorkerThreadsSource::DefaultCap => format!(
                "{} (default cap {DEFAULT_TOKIO_WORKER_THREADS_CAP}; override with {TOKIO_WORKER_THREADS_ENV})",
                self.tokio_worker_threads
            ),
            WorkerThreadsSource::EnvOverride => {
                format!("{} (from {TOKIO_WORKER_THREADS_ENV})", self.tokio_worker_threads)
            }
            WorkerThreadsSource::InvalidEnvIgnored => format!(
                "{} (ignored invalid {TOKIO_WORKER_THREADS_ENV})",
                self.tokio_worker_threads
            ),
        };
        let glibc = match &self.glibc_arena_cap {
            Some(cap) if cap.applied => format!("glibc arena cap {}", cap.max),
            Some(cap) => format!(
                "glibc arena cap not applied (mallopt(M_ARENA_MAX, {}) failed)",
                cap.max
            ),
            None => "glibc arena cap n/a".to_string(),
        };
        format!(
            "runtime resources: allocator {}; tokio workers {workers}; {glibc}",
            self.allocator
        )
    }
}

/// Apply the process-start resource policy and remember the result so
/// `holon serve` can report it later. The allocator label is provided by the
/// binary crate because only it knows which global allocator was linked in.
pub fn apply_startup_policy(allocator: &'static str) -> ResourcePolicyReport {
    let (tokio_worker_threads, worker_threads_source) = resolve_worker_threads();
    let report = ResourcePolicyReport {
        allocator,
        tokio_worker_threads,
        worker_threads_source,
        glibc_arena_cap: cap_glibc_arenas(),
    };
    let _ = STARTUP_REPORT.set(report.clone());
    report
}

/// The report recorded by [`apply_startup_policy`], if this process applied
/// the startup policy.
pub fn startup_report() -> Option<&'static ResourcePolicyReport> {
    STARTUP_REPORT.get()
}

fn resolve_worker_threads() -> (usize, WorkerThreadsSource) {
    match std::env::var_os(TOKIO_WORKER_THREADS_ENV) {
        Some(value) => {
            let parsed = value
                .to_str()
                .and_then(|raw| raw.trim().parse::<usize>().ok())
                .filter(|threads| *threads > 0);
            match parsed {
                Some(threads) => (threads, WorkerThreadsSource::EnvOverride),
                None => (
                    default_worker_threads(),
                    WorkerThreadsSource::InvalidEnvIgnored,
                ),
            }
        }
        None => (default_worker_threads(), WorkerThreadsSource::DefaultCap),
    }
}

fn default_worker_threads() -> usize {
    let parallelism = std::thread::available_parallelism()
        .map(|cpus| cpus.get())
        .unwrap_or(1);
    parallelism.min(DEFAULT_TOKIO_WORKER_THREADS_CAP).max(1)
}

// M_ARENA_MAX only exists on glibc; musl and non-Linux libc malloc has no
// per-thread arenas to cap.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn cap_glibc_arenas() -> Option<GlibcArenaCapReport> {
    // SAFETY: mallopt only flips the arena-limit tunable for this process.
    let applied = unsafe { libc::mallopt(libc::M_ARENA_MAX, GLIBC_ARENA_MAX) } == 1;
    Some(GlibcArenaCapReport {
        max: GLIBC_ARENA_MAX,
        applied,
    })
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn cap_glibc_arenas() -> Option<GlibcArenaCapReport> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::lock_env;

    #[test]
    fn default_worker_threads_is_bounded() {
        let threads = default_worker_threads();
        assert!(threads >= 1);
        assert!(threads <= DEFAULT_TOKIO_WORKER_THREADS_CAP);
    }

    #[test]
    fn worker_threads_env_override_is_parsed() {
        let _guard = lock_env();
        std::env::set_var(TOKIO_WORKER_THREADS_ENV, "3");
        let (threads, source) = resolve_worker_threads();
        std::env::remove_var(TOKIO_WORKER_THREADS_ENV);
        assert_eq!(threads, 3);
        assert_eq!(source, WorkerThreadsSource::EnvOverride);
    }

    #[test]
    fn invalid_worker_threads_env_falls_back_to_default() {
        let _guard = lock_env();
        std::env::set_var(TOKIO_WORKER_THREADS_ENV, "not-a-number");
        let (threads, source) = resolve_worker_threads();
        std::env::remove_var(TOKIO_WORKER_THREADS_ENV);
        assert_eq!(threads, default_worker_threads());
        assert_eq!(source, WorkerThreadsSource::InvalidEnvIgnored);
    }

    #[test]
    fn zero_worker_threads_env_is_rejected() {
        let _guard = lock_env();
        std::env::set_var(TOKIO_WORKER_THREADS_ENV, "0");
        let (threads, source) = resolve_worker_threads();
        std::env::remove_var(TOKIO_WORKER_THREADS_ENV);
        assert_eq!(threads, default_worker_threads());
        assert_eq!(source, WorkerThreadsSource::InvalidEnvIgnored);
    }

    #[test]
    fn startup_summary_mentions_allocator_and_worker_policy() {
        let report = ResourcePolicyReport {
            allocator: "jemalloc",
            tokio_worker_threads: DEFAULT_TOKIO_WORKER_THREADS_CAP,
            worker_threads_source: WorkerThreadsSource::DefaultCap,
            glibc_arena_cap: Some(GlibcArenaCapReport {
                max: GLIBC_ARENA_MAX,
                applied: true,
            }),
        };
        let summary = report.startup_summary();
        assert!(summary.contains("allocator jemalloc"));
        assert!(summary.contains(&format!(
            "tokio workers {DEFAULT_TOKIO_WORKER_THREADS_CAP} (default cap"
        )));
        assert!(summary.contains("glibc arena cap"));
    }

    #[test]
    fn apply_startup_policy_records_report_for_binary_allocator() {
        let _guard = lock_env();
        std::env::remove_var(TOKIO_WORKER_THREADS_ENV);
        let report = apply_startup_policy("allocator-under-test");
        assert_eq!(report.tokio_worker_threads, default_worker_threads());
        assert_eq!(
            startup_report().map(|recorded| recorded.allocator),
            Some("allocator-under-test")
        );
    }
}

# Linux jemalloc and tokio worker cap

## Choice

The `holon` binary installs `tikv-jemallocator` as the global allocator on
Linux only (`#[cfg(target_os = "linux")]` in `src/main.rs`); macOS and Windows
keep the platform allocator. At startup the binary also applies
`mallopt(M_ARENA_MAX, 8)` for remaining libc-side malloc users and caps its
tokio worker threads at `min(available_parallelism, 8)`, overridable with
`HOLON_TOKIO_WORKER_THREADS`. `holon serve` prints the effective policy in
its startup summary next to the fd-limit line.

## Reason

glibc gives each allocating thread its own arena and only trims the arena top
on `free`, so each arena's RSS is a historical high-watermark. A long-lived
daemon with tens of tokio workers therefore grows RSS without bound even
when live data is stable (#2931: ~8.4 GiB `Private_Dirty` after 2.5 h on a
64-core host, dominated by 64 MB-aligned arena mappings). jemalloc's
decay-based purging (default `dirty_decay_ms`/`muzzy_decay_ms` = 10 s, plus
the `background_threads` feature) returns freed pages to the OS, so RSS
tracks live data instead of arena peaks. `mallopt(M_ARENA_MAX, 8)` still
bounds glibc arenas for C code (bundled SQLite) that a Rust global allocator
cannot intercept. The worker cap is orthogonal hygiene: agent-runtime load
is I/O-bound, and CPU-heavy work runs on tokio's blocking pool, which is not
capped.

## Preserved boundary

The allocator declaration lives in the bin crate only, so library consumers,
integration tests, and benchmarks keep the platform allocator and benchmark
comparability is preserved (bench harnesses also build their own uncapped
runtimes). The dependency is target-gated to Linux, so darwin and Windows
builds are unchanged. jemalloc can still be tuned at runtime through
`_RJEM_MALLOC_CONF` (for example `dirty_decay_ms`), and the worker cap can be
raised per deployment without a rebuild. Reverting or replacing the allocator
is a one-line change in `src/main.rs`.

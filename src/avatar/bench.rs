//! Isolated synthetic renderer benchmark: `cargo bench --bench avatar_render`.
//! Three groups by default; every PNG reparses SVG and allocates fresh pixels.
//! No daemon, HTTP, cache, network, fonts, or persisted agent data is involved.
use std::{hint::black_box, time::Instant};

use anyhow::{ensure, Result};
use holon::avatar::{render_png, render_svg, Expression, VERSION};
use serde_json::json;

#[cfg(unix)]
fn resources() -> (u64, u64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied rusage on success. The value is
    // only read after a successful call; no pointers are retained.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return (0, 0);
        }
        usage.assume_init()
    };
    let micros = |tv: libc::timeval| tv.tv_sec as u64 * 1_000_000 + tv.tv_usec as u64;
    let cpu_us = micros(usage.ru_utime) + micros(usage.ru_stime);
    #[cfg(target_os = "macos")]
    let rss_bytes = usage.ru_maxrss as u64;
    #[cfg(not(target_os = "macos"))]
    let rss_bytes = usage.ru_maxrss as u64 * 1024;
    (cpu_us, rss_bytes)
}

#[cfg(not(unix))]
fn resources() -> (u64, u64) {
    (0, 0)
}

fn main() -> Result<()> {
    let samples = std::env::var("HOLON_AVATAR_BENCH_SAMPLES")
        .ok()
        .map(|s| s.parse::<usize>())
        .transpose()?
        .unwrap_or(100);
    ensure!(samples >= 20, "at least 20 samples are required for p95");
    println!(
        "{}",
        json!({
            "suite": "avatar-cold-render",
            "version": VERSION,
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "platform": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "samples_per_group": samples,
            "groups": 3,
            "rss": "process lifetime peak; 0 means unavailable",
            "cpu": "user+system process CPU microseconds; 0 means unavailable"
        })
    );
    for group in 0..3 {
        for size in [0, 32, 128, 512] {
            let before = resources().0;
            let mut timings = Vec::with_capacity(samples);
            let mut lengths = Vec::with_capacity(samples);
            for sample in 0..samples {
                let index = black_box((sample * 37 + group * 127) % 1152);
                let expression = black_box(
                    [
                        Expression::Neutral,
                        Expression::Focused,
                        Expression::Questioning,
                        Expression::Waiting,
                        Expression::Sleeping,
                    ][sample % 5],
                );
                let started = Instant::now();
                let bytes = if size == 0 {
                    render_svg(index, expression).into_bytes()
                } else {
                    render_png(index, expression, size)?
                };
                timings.push(started.elapsed().as_nanos() as u64);
                lengths.push(black_box(bytes).len());
            }
            let (cpu_us, rss_bytes) = resources();
            timings.sort_unstable();
            lengths.sort_unstable();
            println!(
                "{}",
                json!({
                    "group": group + 1,
                    "format": if size == 0 { "svg" } else { "png" },
                    "size": size,
                    "samples": samples,
                    "p50_us": timings[(samples - 1) / 2] as f64 / 1000.0,
                    "p95_us": timings[(samples * 95).div_ceil(100) - 1] as f64 / 1000.0,
                    "process_cpu_us": cpu_us.saturating_sub(before),
                    "process_peak_rss_bytes": rss_bytes,
                    "bytes_min": lengths[0],
                    "bytes_p50": lengths[(samples - 1) / 2],
                    "bytes_max": lengths[samples - 1]
                })
            );
        }
    }
    Ok(())
}

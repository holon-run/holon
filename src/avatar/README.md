# Versioned avatar renderer

`avatar.rs` and `data.rs` implement the existing JavaScript artwork, with
`light-mech-1-vivid-2` output and the unchanged `light-mech-1:` identity seed.
IDs are hashed as Unicode code points using wrapping 32-bit FNV-1a; they never
enter SVG markup. All five expressions, the `holonbot` brand recipe, and empty
IDs retain the JavaScript behavior. PNG rendering only accepts the SVG generated
by this module, with a maximum dimension of 512 pixels.

## Repeatable checks

From the repository root:

```sh
HOLON_LIB_TEST_SHARD=d cargo test --lib avatar::tests -- --skip http::avatar::tests
node web-gui/app/scripts/check-avatar-parity.mjs
cargo bench --bench avatar_render
cargo run --quiet --example avatar_probe -- --png holonbot neutral 128 > /path/to/avatar.png
```

The optional `--png` exporter writes the actual Rust-rendered PNG to stdout for
visual inspection. It uses the same bounded renderer as the HTTP endpoint.

The parity script requires Node 24+, matching CI. It checks the shared
[JavaScript goldens](../../tests/fixtures/agent-avatar-golden.json), the exported
brand SVG, and exact Rust/JavaScript SVG bytes for all 1,152 recipe indices and
all five expressions. Its 5,835 comparisons include Chinese, emoji, combining
characters, integer overflow, hostile markup, brand, and empty identities.
`--update-fixtures` deliberately updates only the JavaScript-derived goldens;
review artwork changes before accepting them.

Rust tests independently rasterize those JavaScript fixture SVGs and compare
every decoded RGBA byte against `render_png` at 32/64/128/256/512 pixels. They
also check dimensions, opacity, palette/contour coverage, strict expression
parsing, and bounded PNG inputs.

The benchmark uses three groups of 100 samples by default; override
`HOLON_AVATAR_BENCH_SAMPLES` to change the sample count (minimum 20).
Each sample creates fresh SVG, and each PNG reparses it and allocates fresh
pixels. No daemon, HTTP server, cache, network, or agent data is involved.
JSON lines include p50/p95 elapsed time, process CPU time, process lifetime peak
RSS, and encoded byte sizes. This is a renderer benchmark, not HTTP capacity or
process-start latency.

## Dependency probe

The pinned lockfile resolves `resvg` 0.45.1 with `default-features = false`.
Text, system/memory-mapped fonts, and embedded raster image decoding are not
enabled; no system font installation is required. The resolved dependency
closure contains 35 package versions including `resvg`; the lockfile adds 19
package versions relative to the pre-avatar revision.

Registry manifests declare `resvg`/`usvg` as `Apache-2.0 OR MIT`,
`tiny-skia`/`tiny-skia-path` as `BSD-3-Clause`, and `arrayref` as `BSD-2-Clause`.
The remaining resolved packages declare MIT/Apache-2.0 or permissive
0BSD/Zlib alternatives; the inspected closure contains no copyleft license.
`resvg` declares Rust 1.67.1 and `usvg` 1.65.0 as their respective minimums;
these are not a claim about the complete repository's minimum Rust version.

On November 8, 2026, the actual repository library/example/release benchmark
compiled on macOS aarch64 with Rust 1.98.0. The release benchmark build took
6m36s, including already-shared runtime dependencies; it is not an isolated
incremental dependency cost. Its standalone executable was 2,070,880 bytes,
not a measurement of the final `holon` executable or its size increase. The
final executable delta was measured on November 8, 2026 with two native
macOS aarch64 `cargo build --release --locked --features local-onnx` builds
sharing one dependency cache and identical embedded web assets: baseline
124,958,496 bytes, candidate 126,270,704 bytes, +1,312,208 bytes (+1.05%).
Build wall times are shared-cache numbers, not clean-dependency costs.
The debug library-test link emitted the platform warning about an oversized
`__eh_frame` compact-unwind section; the renderer tests still passed.

## Observed release measurements

Each entry is `p50 / p95` in microseconds, with 100 uncached renders per group:

| Output | Group 1 | Group 2 | Group 3 | Encoded bytes, all groups |
| --- | ---: | ---: | ---: | ---: |
| SVG | 1.250 / 3.500 | 1.166 / 1.375 | 1.166 / 1.417 | 796–1,155 |
| PNG 32 | 54.292 / 70.167 | 51.417 / 58.042 | 53.583 / 62.333 | 1,297–1,817 |
| PNG 128 | 162.958 / 185.084 | 158.417 / 179.583 | 162.125 / 183.708 | 6,423–9,248 |
| PNG 512 | 680.667 / 744.166 | 674.750 / 724.125 | 668.250 / 721.334 | 30,143–40,052 |

Process CPU time per 100 samples was 119–187 us for SVG, 5,198–5,891 us for
PNG 32, 15,999–16,486 us for PNG 128, and 66,923–68,418 us for PNG 512.
Lifetime peak RSS reached 10,665,984 bytes. RSS is process-wide cumulative
high-water usage, not an incremental per-render allocation measurement.
These synthetic host-local results are evidence, not timing assertions or
cross-platform performance guarantees.

## HTTP and cache measurements

Run the offline probes without starting a daemon:

```sh
HOLON_LIB_TEST_SHARD=c cargo test --release --locked --features local-onnx \
  --lib http::avatar::tests::avatar_ -- --ignored --nocapture --test-threads=1
```

On the same macOS aarch64 host, three groups of authenticated in-process router
requests produced the ranges below. Each cold burst starts with a fresh resource
cache; the hot phase first populates every requested key. These measurements
include target validation and response consumption, but not TCP/TLS or a browser.
Latency percentiles include **only successful 200 responses**. Rejected 503
requests have separate sample counts, percentiles and byte totals in the JSON.

Different-key bursts, with all latency ranges in milliseconds:

| Burst | Output | Cold 200 / 503 counts | Cold p50 | Cold p95 | Hot p50 | Hot p95 |
| ---: | --- | --- | ---: | ---: | ---: | ---: |
| 32 | SVG | 27–32 / 0–5 | 0.209–0.677 | 0.310–10.638 | 0.103–0.133 | 0.179–0.233 |
| 32 | PNG 32 | 20–21 / 11–12 | 0.530–0.906 | 0.799–1.293 | 0.089–0.135 | 0.192–0.235 |
| 32 | PNG 128 | 19 / 13 | 1.091–1.277 | 1.845–2.100 | 0.076–0.128 | 0.156–0.242 |
| 32 | PNG 512 | 18 / 14 | 3.709–4.168 | 6.709–7.140 | 0.097–0.132 | 0.192–0.302 |
| 128 | SVG | 38–60 / 68–90 | 0.502–0.578 | 0.860–0.948 | 0.313–0.482 | 0.677–0.885 |
| 128 | PNG 32 | 22–23 / 105–106 | 0.915–1.016 | 1.206–1.543 | 0.304–0.382 | 0.761–0.909 |
| 128 | PNG 128 | 21–22 / 106–107 | 1.442–1.634 | 2.273–2.590 | 0.263–0.407 | 0.700–0.867 |
| 128 | PNG 512 | 20 / 108 | 4.317–4.539 | 7.125–8.048 | 0.324–0.389 | 0.715–0.841 |

Same-key cold bursts yielded 17–32 successes for 32 requests and 33–127 for 128;
their successful p95 ranges were 0.159–0.812 ms and 0.686–1.055 ms respectively.
All hot bursts, including same-key bursts, had zero 503s. Across both key modes,
503 p50/p95 ranges were 0.046–0.257 / 0.072–0.385 ms for 32 requests and
0.023–0.657 / 0.023–1.191 ms for 128. Cold overload is intentional: these
simultaneous bursts exceed the two-builder/sixteen-waiter budget, unlike the Web
repository's four concurrent requests.

Cold batch wall/CPU ranges were 0.259–11.208 / 1.085–29.941 ms for 32 requests
and 0.948–8.243 / 4.587–21.871 ms for 128. Hot wall/CPU ranges were
0.258–0.363 / 1.079–1.960 ms and 0.920–1.250 / 4.168–6.306 ms respectively.
The HTTP probe's process lifetime peak RSS reached 58,228,736 bytes, not a
per-request allocation delta.

The cache-only probe cycles all 1,152 SVG recipes past the 512-entry cap, repeats
that cycle after eviction, then reads the resident 512 entries. Across three
groups, cold/evicted p50/p95 ranged 6.958–14.708 / 12.625–24.000 microseconds;
hot p50/p95 ranged 1.000–1.583 / 1.125–1.625 microseconds. CPU per full cycle
was 9.946–20.844 ms, versus 0.593–0.934 ms per hot batch. Process lifetime peak
RSS reached 15,564,800 bytes. Allocation counts were not measured.

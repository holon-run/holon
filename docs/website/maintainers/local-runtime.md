---
title: Run Holon locally
summary: A conservative workflow for building, running, and inspecting Holon locally.
order: 10
---

# Run Holon locally

Use this workflow when you want to inspect Holon without assuming that every
interface is stable.

## 1. Build and test

```bash
cargo build
cargo test
```

For focused work, prefer the most relevant Rust test target, then run broader
checks before submitting changes.

## 2. Inspect the command surface

```bash
cargo run -- --help
```

The runtime is evolving. Let the compiled help output define the exact local
CLI behavior for your checkout.

## 3. Keep lifecycle concepts visible

When testing behavior, record which runtime concept you are exercising:

- work item creation or updates
- task lifecycle and output retrieval
- queueing and wake/sleep behavior
- external trigger ingress
- user-facing delivery versus internal trace output

This keeps experiments aligned with Holon's product intent instead of reducing
them to model prompt trials.

## 4. Verify through repository checks

The default Rust checks are:

```bash
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo check --all-targets
cargo test
```

Use narrower checks during iteration, but do not replace real project checks
with throwaway scripts for final validation.

<a id="web-gui-development"></a>

## Web GUI development

Release binaries embed the GUI; the development server is for changing its
source. Use Node.js 24 or newer. From the repository root, install and build
the shared conversation SDK before starting the GUI:

```bash
npm ci --prefix packages/conversation-sdk
npm --prefix packages/conversation-sdk run build
cd web-gui/app
npm ci
npm run dev
```

The development server supports hot reload. Its `/api` proxy targets
`http://127.0.0.1:7878` by default; set `HOLON_API_PROXY_TARGET` when the test
daemon uses a different address. Use a development daemon without production
credentials or data.

Verify a UI change against the running daemon with the
[Web GUI workflow](../guides/use-web-gui.md). Do not treat fixture output as
evidence that a live request succeeded.

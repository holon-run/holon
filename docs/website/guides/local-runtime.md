---
title: Local runtime
summary: A conservative workflow for running and inspecting Holon locally.
order: 10
---

# Local runtime

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
cargo fmt --check
cargo test
```

Use narrower checks during iteration, but do not replace real project checks
with throwaway scripts for final validation.

# Agent Bundle Specification (Design Draft)

This document defines the **agent bundle** packaging format. 

The **agent contract** (filesystem layout, inputs, outputs) is defined in
`rfc/0002-agent-scheme.md`. This document only specifies the bundle packaging
and how the runner consumes it.

## Goals
- Make runtime image composition independent of agent implementation details.
- Allow a bundle to be copied into a composed image without installing OS packages.
- Support both prebuilt bundles and locally built bundles inside the Holon repo.
- Provide a stable manifest so the runner can validate compatibility.

## Non-goals
- Defining new agent behavior or runtime protocol.
- Replacing the current container isolation model.

## Bundle format
- **Container**: `tar.gz` or `tar.zst` archive.
- **Root layout**:
  - `manifest.json`
  - `bin/agent` (entrypoint executable or script)
  - `dist/` (agent code)
  - `node_modules/` (production dependencies, if applicable)
  - `assets/` (optional)

## Manifest schema (v1)
`manifest.json` is required. Suggested fields:

```json
{
  "bundleVersion": "1",
  "name": "agent-claude",
  "version": "0.1.0",
  "entry": "bin/agent",
  "platform": "linux",
  "arch": "amd64",
  "libc": "glibc",
  "engine": {
    "name": "claude-code",
    "sdk": "@anthropic-ai/claude-agent-sdk",
    "sdkVersion": "0.1.75"
  },
  "runtime": {
    "type": "node",
    "version": "20.15.1"
  },
  "env": {
    "NODE_ENV": "production"
  },
  "capabilities": {
    "needsNetwork": true,
    "needsGit": true
  }
}
```

**Field notes**
- `bundleVersion`: schema version used by the runner.
- `entry`: relative path to the executable that will be invoked as the container
  entrypoint.
- `engine`: underlying agent runtime metadata (implementation-specific).
- `runtime`: runtime metadata describing the required runtime provided by the base image.
- `libc`: used to select compatible bundles (`glibc` vs `musl`).

## Runner contract
The runner treats the bundle as a black box:
1. Copy the bundle archive into the build context.
2. Extract into a fixed path (e.g. `/holon/agent`).
3. Set entrypoint to `<extract_root>/<manifest.entry>`.
4. Mount `/holon/input`, `/holon/workspace`, `/holon/output` per the agent contract.

The runner should not assume language, package manager, or build system.

## Bundle naming (recommended)
`agent-bundle-{name}-{version}-{platform}-{arch}-{libc}.tar.gz`

Example:
`agent-bundle-agent-claude-0.1.0-linux-amd64-glibc.tar.gz`

## Local build inside the Holon repo
If the agent source tree is available (e.g. `agents/claude/`), the runner may build a bundle locally instead of
fetching a prebuilt artifact. Recommended behavior:
- Compute a cache key from source + lockfile + runtime version.
- If cached bundle exists, reuse it.
- Otherwise run a local build script that produces a bundle matching this spec.

## Compatibility rules
- The runner must ensure the base image provides the declared runtime and version.
- `platform`, `arch`, and `libc` must match the composed image environment.

## Open questions
- Whether to sign bundles and verify checksums.
- Whether to support additional resolver prefixes (e.g. `file:`, `gh:`).

# State Mount

## Overview

The state mount feature allows skills and runs to persist and reuse data across executions. This is particularly useful for:

- Caching synced issues/PRs/comments from GitHub API (e.g., for `project-pulse` skill)
- Storing expensive computation results
- Maintaining incremental state between runs
- Reducing API usage and speeding up analysis

## How It Works

When you provide a `--state-dir` flag, Holon bind-mounts the specified directory from the host into the container at `${HOLON_STATE_DIR}`. This directory:

- **Persists across runs**: Data written to `${HOLON_STATE_DIR}` remains on the host and is available in subsequent runs
- **Is opt-in**: No state persistence occurs if the flag is omitted
- **Is separate from output**: Keeps deterministic artifacts (in `${HOLON_OUTPUT_DIR}`) separate from mutable caches (in `${HOLON_STATE_DIR}`)

## Usage

### CLI Usage

```bash
# Local development with workspace-relative state
holon run --goal "Analyze project structure" --state-dir .holon/state

# Use a global state location for cross-branch caching
holon run --goal "Sync issues" --state-dir ~/.holon/state/myproject

# Solve command with state persistence
holon solve holon-run/holon#123 --state-dir .holon/state
```

### GitHub Action Usage

```yaml
- name: Run Holon with state cache
  uses: holon-run/holon@main
  with:
    ref: ${{ github.event.issue.html_url }}
    state_dir: .holon/state  # Workspace-local state

# For persistent caching across runs, combine with actions/cache
- name: Cache Holon state
  uses: actions/cache@v4
  with:
    path: .holon/state
    key: holon-state-${{ github.repository }}-${{ github.ref }}-${{ github.sha }}
    restore-keys: |
      holon-state-${{ github.repository }}-${{ github.ref }}-
      holon-state-${{ github.repository }}-
```

## Directory Layout Conventions

### Inside the Container

```
/root/
├── workspace/    # Repository snapshot (writable)
├── input/        # Context and prompts (read-only)
├── output/       # Deterministic artifacts (writable)
└── state/        # Cross-run caches (writable, if mounted)
    └── <skill-name>/  # Skill-specific cache namespace
        └── cache files...
```

### Skill Contract for State Usage

Skills should follow these conventions:

1. **Namespace**: Use `${HOLON_STATE_DIR}/<skill-name>/...` for cache files
   - Example: `${HOLON_STATE_DIR}/project-pulse/issues-cache.json`

2. **Handle first run**: Skills must tolerate missing/empty state directory
   - Check if state exists before reading
   - Create fresh cache if state is missing

3. **Separate concerns**:
   - Put **non-deterministic caches** in `${HOLON_STATE_DIR}` (safe to delete)
   - Put **deterministic outputs** in `${HOLON_OUTPUT_DIR}` (required artifacts)

4. **Handle migrations**: Skills should handle cache format changes gracefully
   - Version cache files
   - Validate and migrate on read

### Example Skill Usage

```go
// In a skill that caches GitHub issues
cachePath := "${HOLON_STATE_DIR}/my-skill/issues.json"

// Try to load from cache
if data, err := os.ReadFile(cachePath); err == nil {
    var cache IssuesCache
    if err := json.Unmarshal(data, &cache); err == nil {
        if time.Since(cache.Timestamp) < 24*time.Hour {
            return cache.Issues, nil  // Use cached data
        }
    }
}

// Cache miss or expired - fetch fresh data
issues := fetchFromGitHub()

// Save to cache for next run
cache := IssuesCache{
    Timestamp: time.Now(),
    Issues:    issues,
}
if data, err := json.Marshal(cache); err == nil {
    os.MkdirAll(filepath.Dir(cachePath), 0755)
    os.WriteFile(cachePath, data, 0644)
}
```

## Recommended Locations

### Local Development

**Workspace-relative** (branch-specific):
```bash
--state-dir .holon/state
```
- ✅ Keeps caches with the codebase
- ✅ Automatically gitignored (if `.holon/` is ignored)
- ❌ Caches don't persist across branches

**Global location** (cross-branch):
```bash
--state-dir ~/.holon/state/${owner}/${repo}
```
- ✅ Caches persist across branches
- ✅ Doesn't pollute workspace
- ❌ Requires manual setup

### CI/Actions

**Workspace-local** (ephemeral runners):
```yaml
state_dir: .holon/state
```
- Use with `actions/cache` for persistence
- Key by repo + branch + skill version

**Self-hosted runners** (persistent):
```yaml
state_dir: /var/lib${HOLON_STATE_DIR}/${{ github.repository }}
```
- No need for actions/cache
- Better performance (no cache restoration)

## Security Considerations

1. **Opt-in**: State mount is always opt-in via explicit flag
2. **Workspace isolation**: Prefer workspace-relative `--state-dir` to avoid mounting arbitrary host paths (no automatic validation yet)
3. **Read-write mount**: State directory is mounted read-write (skills can modify)
4. **Future read-only mode**: May add `--state-dir:ro` for deterministic replay

## Troubleshooting

### State directory doesn't exist

**Problem**: First run with new state directory

**Solution**: Holon automatically creates the directory if it doesn't exist
```
mkdir -p .holon/state
```

### Permission errors

**Problem**: Container can't write to state directory

**Solution**: Ensure proper permissions on host
```bash
chmod 755 .holon/state
```

### Stale cache

**Problem**: State from previous run is outdated

**Solutions**:
- Clear state manually: `rm -rf .holon/state/*`
- Use cache keys in CI to invalidate
- Implement cache TTL in skills

### State appears in git diff

**Problem**: State directory is tracked by git

**Solution**: Add to `.gitignore`
```
# .gitignore
.holon/state/
```

## Comparison with Alternatives

| Feature | State Mount (`${HOLON_STATE_DIR}`) | Output (`${HOLON_OUTPUT_DIR}`) | Input (`${HOLON_INPUT_DIR}`) |
|---------|------------------------------|--------------------------|----------------------|
| **Purpose** | Cross-run caches | Deterministic artifacts | Context/prompts |
| **Persistence** | Persists across runs | Ephemeral (cleaned) | Read-only |
| **Mutability** | Read-write | Read-write | Read-only |
| **Typical Content** | API caches, incremental state | patches, summaries, manifests | spec, context |
| **Safety** | Safe to delete | Required for publish | Not modified |
| **Example** | `issues-cache.json` | `diff.patch`, `summary.md` | `spec.yaml` |

## See Also

- [Skills Documentation](skills.md) - Skill development guide
- [Modes Documentation](modes.md) - Execution modes
- [Workspace Manifest](workspace-manifest-format.md) - Output artifact format

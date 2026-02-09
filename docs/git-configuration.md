# Git User Configuration

When Holon performs git operations (e.g., creating PRs, publishing changes), it needs git user identity information for authoring commits. This document explains how git identity is configured and how to customize it.

## Default Behavior

If no git identity is configured, Holon uses default fallback values:

- **Name**: `holonbot[bot]`
- **Email**: `250454749+holonbot[bot]@users.noreply.github.com`

## Configuration Priority

Holon determines git identity using a **centralized resolver** (`git.ResolveConfig()`) with the following priority (highest to lowest):

1. **Host git config** (local > global > system) - Your system's git configuration
2. **ProjectConfig** - Project-level configuration in `.holon/config.yaml`
3. **Environment variables** - `GIT_AUTHOR_NAME` and `GIT_AUTHOR_EMAIL`
4. **Default values** - `holonbot[bot] <250454749+holonbot[bot]@users.noreply.github.com>`

**Important**: The centralized resolver ensures consistent behavior across all commands (`run`, `publish`, `solve`). Host git config always has highest priority, respecting your personal git identity.

## Configuration Methods

### Method 1: Host Git Config (Recommended)

Configure git globally on your system. This is the highest priority and will be used by Holon for all projects:

```bash
git config --global user.name "Your Name"
git config --global user.email "your.email@example.com"
```

**Benefits:**
- Works across all Holon projects
- Respects your personal git identity
- No per-project configuration needed

### Method 2: Project Configuration

Create or edit `.holon/config.yaml` in your project root:

```yaml
git:
  author_name: "Project Bot"
  author_email: "bot@example.com"
```

**Use cases:**
- Project-specific bot identity
- CI/CD environments where host config is unavailable
- Different identity for different projects

### Method 3: Environment Variables

For CI/CD or one-off operations, set environment variables:

```bash
export GIT_AUTHOR_NAME="CI Bot"
export GIT_AUTHOR_EMAIL="ci@example.com"
export GIT_COMMITTER_NAME="CI Bot"
export GIT_COMMITTER_EMAIL="ci@example.com"

holon run --goal "Fix the bug"
```

**Note**: Environment variables are only respected by the `holon run` command. For `holon solve`, use host git config or ProjectConfig.

## Scenarios and Recommendations

### Local Development

**Recommended**: Use host git config

```bash
git config --global user.name "Your Name"
git config --global user.email "your.email@example.com"
```

This ensures your commits reflect your actual identity.

### CI/CD (GitHub Actions, GitLab CI, etc.)

**Recommended**: Use ProjectConfig or environment variables

**Option A - ProjectConfig** (`.holon/config.yaml`):
```yaml
git:
  author_name: "CI Bot"
  author_email: "ci-bot@example.com"
```

**Option B - Environment Variables** (for `holon run`):
```yaml
# .github/workflows/holon.yml
- name: Run Holon
  env:
    GIT_AUTHOR_NAME: "CI Bot"
    GIT_AUTHOR_EMAIL: "ci-bot@example.com"
  run: |
    holon run --goal "Fix the bug"
```

### Team Projects with Shared Bot Identity

**Recommended**: Use ProjectConfig

```yaml
# .holon/config.yaml
git:
  author_name: "Team Bot"
  author_email: "team-bot@example.com"
```

Commit this file to your repository so all team members use the same bot identity.

### Bot Accounts

**Recommended**: Use host git config on the bot machine

```bash
sudo -u holon-bot git config --global user.name "holonbot[bot]"
sudo -u holon-bot git config --global user.email "250454749+holonbot[bot]@users.noreply.github.com"
```

## Troubleshooting

### Problem: "Committer identity unknown" error

**Cause**: The workspace doesn't have git user identity configured, and no fallback is available.

**Solutions**:
1. Configure host git config globally (recommended)
2. Add git configuration to `.holon/config.yaml`
3. For `holon run`, set `GIT_AUTHOR_NAME` and `GIT_AUTHOR_EMAIL` environment variables

### Problem: Holon uses wrong identity

**Cause**: Host git config takes priority over ProjectConfig.

**Solutions**:
1. Check your host git config: `git config --global user.name` and `git config --global user.email`
2. If you want ProjectConfig to take priority, temporarily remove or override host config:
   ```bash
   git config --global --unset user.name
   git config --global --unset user.email
   ```
3. Use ProjectConfig for the desired identity

### Problem: Different identity for different projects

**Solution**: Use ProjectConfig for each project. Host git config is used as fallback when ProjectConfig doesn't specify git identity.

## Technical Details

### Centralized Resolver

Holon uses a centralized git configuration resolver (`git.ResolveConfig()`) that ensures consistent behavior across all commands:

- **`holon run`**: Resolves git config once using the centralized resolver, passes to runtime
- **`holon solve`**: Uses the centralized resolver for consistency

This eliminates the previous confusion where `runner.go` would set ProjectConfig values, then `runtime.go` would override with host git config. Now the priority is explicit and enforced in one place.

### Priority Enforcement

The centralized resolver enforces priority as:

1. **Host git config** (local > global > system) - highest priority
2. **ProjectConfig** (`.holon/config.yaml`)
3. **Environment variables** (`GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`)
4. **Defaults** (`holonbot[bot] <250454749+holonbot[bot]@users.noreply.github.com>`)

This means:
- Your personal git identity (host config) is always respected
- ProjectConfig is used as fallback when host config is unavailable
- Environment variables provide explicit override capability
- Defaults ensure the system never fails completely

### Git Commit Identity

Git requires two identities for each commit:

- **Author**: The person who wrote the code (set via `--author` flag)
- **Committer**: The person who applied the commit (from git config)

Holon configures both to be the same value, ensuring consistent attribution.

## Related Documentation

- `.holon/config.yaml` examples: see this document and `docs/skills.md`
- Repo conventions and contributor workflow: `AGENTS.md` and `CONTRIBUTING.md`

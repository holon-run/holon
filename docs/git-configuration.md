# Git User Configuration

When Holon performs git operations (e.g., creating PRs, publishing changes), it needs git user identity information for authoring commits. This document explains how git identity is configured and how to customize it.

## Default Behavior

If no git identity is configured, Holon uses default fallback values:

- **Name**: `Holon Bot`
- **Email**: `bot@holon.run`

## Configuration Priority

Holon determines git identity using the following priority (highest to lowest):

1. **Host git config** - Your system's global git configuration (`git config --global user.name` and `user.email`)
2. **ProjectConfig** - Project-level configuration in `.holon/config.yaml`
3. **Default values** - `Holon Bot <bot@holon.run>`

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

**Note**: Environment variables are only respected by the `holon run` command. For `holon solve` and `holon publish`, use host git config or ProjectConfig.

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
sudo -u holon-bot git config --global user.name "Holon Bot"
sudo -u holon-bot git config --global user.email "bot@holon.run"
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

### How Holon Resolves Git Identity

1. **`holon run` command**:
   - Reads ProjectConfig from `.holon/config.yaml`
   - Injects git config as environment variables into the container
   - Host git config overrides ProjectConfig values at runtime

2. **`holon publish` command**:
   - Reads host git config directly
   - Injects into manifest metadata as `git_author_name` and `git_author_email`
   - Publishers (github-pr, git) use these values for commits

3. **`holon solve` command**:
   - Combines both approaches for consistency

### Git Commit Identity

Git requires two identities for each commit:

- **Author**: The person who wrote the code (set via `--author` flag)
- **Committer**: The person who applied the commit (from git config)

Holon configures both to be the same value, ensuring consistent attribution.

## Related Documentation

- [Project Configuration](../CLAUDE.md#project-configuration-file) - Details on `.holon/config.yaml`
- [Publisher System](../CLAUDE.md#publisher-system) - How git identity is used during publishing
- [Environment Variables](../CLAUDE.md#required-environment-variables) - All supported environment variables

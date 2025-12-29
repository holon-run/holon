# GitHub Token and Commit Identity

When Holon creates pull requests or commits changes, it uses a GitHub token for authentication. This document explains how Holon determines which token to use and how that affects the commit identity (the "author" shown in Git history and on PRs).

## Why This Matters

The commit identity determines:
- **Who appears as the author** of commits in Git history
- **Who appears as the PR creator** on GitHub
- **What permissions are available** (e.g., ability to modify workflows)

Understanding token selection helps you control the commit identity and ensure proper permissions.

## Token Selection Priority

Holon uses a layered approach to select the GitHub token:

### Holon Internal Priority (pkg/github/client.go)

1. **`HOLON_GITHUB_TOKEN`** environment variable (highest priority)
2. **`GITHUB_TOKEN`** environment variable
3. **`gh auth token`** (fallback to GitHub CLI token)

### GitHub Action Priority (action.yml)

When running in GitHub Actions CI, the action layer adds additional logic:

1. **Explicit `github_token` input** (user-provided via workflow)
2. **Holonbot App token** (via OIDC broker exchange)
3. **`github.token`** (GitHub Actions auto-generated token)

## Usage Scenarios

### Scenario 1: Local Development (Your Identity)

**Goal**: Use your personal GitHub identity for commits.

**Setup**:
```bash
# Install GitHub CLI
# macOS
brew install gh

# Linux
# See https://github.com/cli/cli#installation

# Login to GitHub (stores token in gh CLI)
gh auth login
```

**How it works**:
- You don't need to set any environment variables
- Holon automatically uses `gh auth token` as fallback
- Commits are authored with your GitHub login identity

**Token flow**:
```
No HOLON_GITHUB_TOKEN
    ↓
No GITHUB_TOKEN
    ↓
Use gh auth token ✅
```

**Commit identity**: `Your Name <your.email@example.com>` (your GitHub identity)

**Use cases**:
- Local development and testing
- Creating PRs from your machine
- Manual holon runs

---

### Scenario 2: CI with Holonbot App (Holonbot Identity)

**Goal**: Use holonbot app identity with full permissions in CI.

**Setup**:

1. Install Holonbot App in your repository
2. Configure workflow with `id-token: write` permission
3. Do NOT set `secrets.HOLON_GITHUB_TOKEN` (let it auto-detect)

**Workflow configuration** (`.github/workflows/holon-solve.yml`):
```yaml
jobs:
  holon:
    runs-on: ubuntu-latest
    permissions:
      contents: write
      issues: write
      pull-requests: write
      id-token: write  # Required for Holonbot OIDC
    steps:
      - uses: holon-run/holon@main
        with:
          # Note: github_token is optional
          # If omitted, action will auto-detect holonbot via OIDC
          ref: "${{ github.repository }}#${{ github.event.issue.number }}"
          anthropic_auth_token: ${{ secrets.ANTHROPIC_AUTH_TOKEN }}
```

**How it works**:
- Action detects OIDC token availability
- Exchanges OIDC token for holonbot app token via broker
- Sets `GITHUB_TOKEN` environment variable to holonbot token
- Holon uses `GITHUB_TOKEN` for all operations

**Token flow**:
```
No explicit github_token input
    ↓
OIDC token available?
    ↓ YES
Exchange for holonbot token ✅
    ↓
Set GITHUB_TOKEN = holonbot token
    ↓
Holon uses GITHUB_TOKEN
```

**Commit identity**: `holonbot[bot] <noreply@holon.run>` or similar

**Capabilities**:
- ✅ Create and modify PRs
- ✅ Modify workflow files
- ✅ Full repository access
- ✅ Push to protected branches

**Use cases**:
- Automated issue resolution
- CI/CD workflows
- Bot-driven development

---

### Scenario 3: CI with Custom Token (Override Identity)

**Goal**: Use a specific token (e.g., your personal token or service account) in CI.

**Setup**:

1. Create GitHub personal access token or use bot token
2. Add as repository secret: `HOLON_GITHUB_TOKEN`
3. Configure workflow to pass this secret

**Workflow configuration**:
```yaml
jobs:
  holon:
    runs-on: ubuntu-latest
    permissions:
      contents: write
      issues: write
      pull-requests: write
    steps:
      - uses: holon-run/holon@main
        with:
          ref: "${{ github.repository }}#${{ github.event.issue.number }}"
          anthropic_auth_token: ${{ secrets.ANTHROPIC_AUTH_TOKEN }}
          github_token: ${{ secrets.HOLON_GITHUB_TOKEN }}  # Explicit token
```

**How it works**:
- Workflow explicitly passes `HOLON_GITHUB_TOKEN` secret
- Action uses this token (highest priority in action layer)
- Action sets `GITHUB_TOKEN` environment variable
- Holon uses `GITHUB_TOKEN` for all operations

**Token flow**:
```
secrets.HOLON_GITHUB_TOKEN set
    ↓
Passed as github_token input
    ↓
Action uses explicit token ✅
    ↓
Set GITHUB_TOKEN = HOLON_GITHUB_TOKEN
    ↓
Holon uses GITHUB_TOKEN
```

**Commit identity**: Identity of the token owner (you or your service account)

**Capabilities**:
- Depends on token permissions
- Personal tokens have your full access
- Service tokens have configured permissions

**Use cases**:
- Using your identity for CI-created PRs
- Service account with specific permissions
- Testing with custom tokens

---

### Scenario 4: CI with GitHub Actions Token (Limited Permissions)

**Goal**: Let Holon use the default GitHub Actions token.

**Setup**:

**Do nothing** - this is the default behavior if:
- No `HOLON_GITHUB_TOKEN` secret is set
- Holonbot app is NOT installed
- No `github_token` input is provided

**How it works**:
- Action uses `github.token` as fallback
- This is the auto-generated token for the workflow run
- Limited to the workflow's permissions

**Token flow**:
```
No HOLON_GITHUB_TOKEN
No Holonbot app
No explicit github_token
    ↓
Use github.token (auto-generated) ✅
    ↓
Set GITHUB_TOKEN = github.token
    ↓
Holon uses GITHUB_TOKEN
```

**Commit identity**: `github-actions[bot] <noreply@github.com>`

**Capabilities**:
- ⚠️ **Limited permissions**
- ✅ Create PRs and commits
- ❌ **Cannot modify workflow files**
- ❌ Cannot push to protected branches (unless explicitly allowed)

**Limitations**:
The GitHub Actions token (`github.token`) has security restrictions:
- Cannot modify `.github/workflows/` files
- Cannot push to branches protected by rules
- Has repository-scoped permissions only

**Use cases**:
- Simple CI workflows that don't modify workflows
- Testing Holon without full bot setup
- Repositories without holonbot app

## Token Priority Summary

| Scenario | HOLON_GITHUB_TOKEN | GITHUB_TOKEN | gh auth | Final Identity |
|----------|---------------------|--------------|---------|----------------|
| Local (gh login) | - | - | ✅ | Your GitHub identity |
| Local (set env) | ✅ | - | - | Token owner identity |
| CI + Holonbot | - | ✅ (auto) | - | holonbot[bot] |
| CI + Custom token | ✅ | ✅ | - | Token owner |
| CI + Default | - | ✅ (auto) | - | github-actions[bot] |

## Common Issues and Solutions

### Issue: "Committer identity unknown"

**Error**: `fatal: empty ident name (for <>) not allowed`

**Cause**: Git committer identity is not configured.

**Solution**: Holon now automatically configures git identity (as of PR #385). If you still see this error:
1. Ensure your token has `repo` scope
2. Check that Holon is correctly reading the token
3. Verify git is being configured: check logs for "git credentials configured"

### Issue: "Failed to push" (Permission Denied)

**Error**: `fatal: could not read Username for 'https://github.com'`

**Cause**: Token is empty or invalid.

**Solutions**:
1. **Verify token is set**:
   ```bash
   # Local
   echo $HOLON_GITHUB_TOKEN
   echo $GITHUB_TOKEN

   # In CI
   # Check workflow secrets
   ```

2. **Check token has correct permissions**:
   - `repo` scope for general repository access
   - `workflow` scope if modifying workflows
   - `repo:status` for CI status checks

3. **For Holonbot**: Ensure app is installed and has permissions

### Issue: PR created by wrong identity

**Symptom**: PR shows `github-actions[bot]` instead of `holonbot[bot]` or your identity.

**Solution**:
- **To use Holonbot**: Install app, ensure `id-token: write` permission
- **To use your identity**: Set `HOLON_GITHUB_TOKEN` secret
- **Local**: Ensure `gh auth login` is configured

### Issue: Cannot modify workflow files

**Symptom**: PR fails when trying to change `.github/workflows/*.yml`

**Cause**: Using `github.token` which doesn't allow workflow modifications.

**Solutions**:
1. Install Holonbot app (recommended)
2. Set `HOLON_GITHUB_TOKEN` with `workflow` scope
3. Use `GITHUB_TOKEN` with appropriate permissions

## Best Practices

### For Local Development

✅ **Do**: Use `gh auth login` for easy authentication
```bash
gh auth login
holon solve holon-run/repo#123
```

✅ **Do**: Override with `HOLON_GITHUB_TOKEN` if needed
```bash
export HOLON_GITHUB_TOKEN=ghp_xxxxxxxxxxxx
holon solve holon-run/repo#123
```

❌ **Don't**: Use `GITHUB_TOKEN` unless specifically needed (less clear that it's custom)

### For CI/CD

✅ **Do**: Install Holonbot app for full permissions
✅ **Do**: Use `id-token: write` permission for OIDC
✅ **Do**: Let action auto-detect holonbot when possible

❌ **Don't**: Use `github.token` if you need to modify workflows
❌ **Don't**: Set `HOLON_GITHUB_TOKEN` unless you need custom identity

### For Production

✅ **Do**: Use Holonbot app for automated workflows
✅ **Do**: Configure proper permissions in workflow
✅ **Do**: Test token permissions in a draft PR first

❌ **Don't**: Use personal tokens in production (use service accounts)

## Configuration Reference

### Environment Variables

| Variable | Priority | Description |
|----------|----------|-------------|
| `HOLON_GITHUB_TOKEN` | 1 (highest) | Holon-specific token, overrides all |
| `GITHUB_TOKEN` | 2 | Standard GitHub token |
| (gh auth) | 3 | GitHub CLI token (fallback) |

### Workflow Permissions

```yaml
permissions:
  contents: write      # Required for commits and PRs
  issues: write        # Required for issue operations
  pull-requests: write # Required for PR operations
  id-token: write      # Required for Holonbot OIDC
```

### Secret Names

| Secret | Description |
|--------|-------------|
| `HOLON_GITHUB_TOKEN` | Custom GitHub token (user or service account) |
| `ANTHROPIC_AUTH_TOKEN` | Anthropic API key for Claude |
| `GITHUB_TOKEN` | Auto-generated by GitHub Actions (don't set manually) |

## Related Documentation

- [Git Configuration Guide](git-configuration.md) - Configuring git user identity
- [Project Configuration](../CLAUDE.md#project-configuration-file) - `.holon/config.yaml` setup
- [Holonbot App](https://github.com/apps/holonbot) - GitHub App for automated workflows
- [GitHub Actions Permissions](https://docs.github.com/en/actions/security-guides/automatic-token-authentication) - Official GitHub docs

## Technical Details

### Implementation Reference

**Token resolution** (`pkg/github/client.go:67-88`):
```go
func GetTokenFromEnv() (string, bool) {
    // 1. Check HOLON_GITHUB_TOKEN first (highest priority)
    token := os.Getenv(HolonTokenEnv)
    if token != "" {
        return token, false
    }

    // 2. Check standard GITHUB_TOKEN
    token = os.Getenv(TokenEnv)
    if token != "" {
        return token, false
    }

    // 3. Fallback to gh CLI
    token = ghAuthToken()
    if token != "" {
        return token, true
    }

    return "", false
}
```

**Action layer** (`action.yml:78-127`):
```bash
# Priority:
# 1) Explicit user-provided token (inputs.github_token)
# 2) Holonbot App token via broker exchange
# 3) GitHub Actions runtime token (github.token)
```

### Security Considerations

- **HOLON_GITHUB_TOKEN**: Stored as repository secret, use for service accounts
- **GITHUB_TOKEN**: Auto-generated by GitHub Actions, scoped to workflow
- **gh auth token**: Stored locally, used by GitHub CLI
- **OIDC tokens**: Temporary, exchanged for holonbot token, auto-expire

### Troubleshooting Commands

```bash
# Check which token Holon would use
# (requires Holon to be installed)
holon solve --help | grep -A5 "github"

# Test gh CLI authentication
gh auth status

# Validate token permissions
# Replace YOUR_TOKEN with actual token
curl -H "Authorization: Bearer YOUR_TOKEN" \
  https://api.github.com/user

# Check git configuration
git config --list | grep -E "user\.(name|email)"
```

## Summary

- **Local**: Use `gh auth login` → your identity
- **CI + Holonbot**: Auto-detect → holonbot identity
- **CI + Custom token**: Set `HOLON_GITHUB_TOKEN` → custom identity
- **CI + Default**: Uses `github.token` → limited permissions

Choose the approach that matches your use case and security requirements.

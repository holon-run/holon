# Claude Agent Bundle Releases

This document describes how Claude agent bundles are published and consumed.

## Release Process

Claude agent bundles are automatically published to GitHub Releases when the version in `package.json` is updated on the `main` branch.

### How it works

1. **Trigger**: A push to `main` that modifies `agents/claude/package.json`
2. **Version detection**: Extract version from `package.json`
3. **Idempotency check**: Skip if tag `agent-claude-v<version>` already exists
4. **Build**: Create the agent bundle using existing tooling
5. **Asset preparation**: Create standardized release assets:
   - `holon-agent-claude-<version>.tar.gz` - The agent bundle
   - `holon-agent-claude-<version>.tar.gz.sha256` - SHA256 checksum
6. **Release**: Create GitHub tag `agent-claude-v<version>` and release with assets

## Asset URLs

Release assets have stable, predictable URLs:

```bash
# Bundle download URL
https://github.com/holoscript/holon/releases/download/agent-claude-v<version>/holon-agent-claude-<version>.tar.gz

# Checksum download URL
https://github.com/holoscript/holon/releases/download/agent-claude-v<version>/holon-agent-claude-<version>.tar.gz.sha256
```

Replace:
- `<version>` with the semantic version (e.g., `0.1.0`)
- `holoscript/holon` with your repository path if using a fork

## Usage Examples

### Basic Download and Verification

```bash
VERSION="0.1.0"
REPO="holoscript/holon"

# Download assets
curl -L -o "holon-agent-claude-${VERSION}.tar.gz" \
  "https://github.com/${REPO}/releases/download/agent-claude-v${VERSION}/holon-agent-claude-${VERSION}.tar.gz"

curl -L -o "holon-agent-claude-${VERSION}.tar.gz.sha256" \
  "https://github.com/${REPO}/releases/download/agent-claude-v${VERSION}/holon-agent-claude-${VERSION}.tar.gz.sha256"

# Verify integrity
sha256sum -c "holon-agent-claude-${VERSION}.tar.gz.sha256"
```

### Using with Holon CLI

```bash
# Using a local bundle file
holon run --agent ./holon-agent-claude-0.1.0.tar.gz --spec your-spec.yaml

# Using a remote URL (requires download first)
holon run --agent https://github.com/holoscript/holon/releases/download/agent-claude-v0.1.0/holon-agent-claude-0.1.0.tar.gz --spec your-spec.yaml
```

### CI/CD Integration

```yaml
# GitHub Actions example
- name: Download Claude Agent
  run: |
    VERSION="0.1.0"
    curl -L -o "holon-agent-claude-${VERSION}.tar.gz" \
      "https://github.com/holoscript/holon/releases/download/agent-claude-v${VERSION}/holon-agent-claude-${VERSION}.tar.gz"

- name: Run Holon with Claude Agent
  run: |
    holon run --agent "holon-agent-claude-${VERSION}.tar.gz" --spec ci-spec.yaml
```

## Version Management

### Bumping Version

1. Edit `agents/claude/package.json` and increment the version
2. Commit and push to `main`:
   ```bash
   git commit -am "Bump Claude agent to v0.2.0"
   git push origin main
   ```
3. The workflow will automatically create a new release

### Finding Latest Version

```bash
# Get latest version from GitHub API
curl -s "https://api.github.com/repos/holoscript/holon/releases" | \
  jq -r '.[0].tag_name' | sed 's/agent-claude-v//'

# List all available versions
curl -s "https://api.github.com/repos/holoscript/holon/releases" | \
  jq -r '.[].tag_name' | grep '^agent-claude-v' | sed 's/agent-claude-v//'
```

## Manual Release (Advanced)

If you need to create a release manually:

```bash
cd agents/claude

# Build bundle
npm ci
npm run bundle

# Prepare release assets
./scripts/prepare-release-assets.sh <version>

# Create GitHub release
gh release create "agent-claude-v<version>" \
  --title "Claude Agent v<version>" \
  --notes "Claude Agent Bundle v<version>" \
  "dist/agent-bundles/release/holon-agent-claude-<version>.tar.gz" \
  "dist/agent-bundles/release/holon-agent-claude-<version>.tar.gz.sha256"
```

## Security Considerations

- Always verify the SHA256 checksum before using downloaded bundles
- The workflow uses minimal permissions (`contents: write`)
- Releases are idempotent - re-running won't create duplicates
- Bundle includes production dependencies only (`npm prune --omit=dev`)

## Troubleshooting

### Release not created
- Check that `package.json` version was actually modified
- Verify the tag doesn't already exist
- Check workflow logs for build failures

### Bundle download issues
- Verify the version exists in releases
- Check repository URL if using a fork
- Ensure network connectivity for downloads

### Checksum verification fails
- Re-download the bundle file
- Verify you're using the correct version
- Check for file corruption during transfer
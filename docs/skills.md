# Claude Skills in Holon

Claude Skills are reusable capabilities that extend Claude's functionality in Holon. Skills allow you to package custom instructions, tools, and resources that Claude can use during task execution.

## What are Claude Skills?

A **Skill** is a directory containing:
- `SKILL.md`: The skill manifest file with instructions and metadata
- Optional supporting files: scripts, templates, configuration files, etc.

Skills provide a way to:
- Encode domain-specific knowledge and best practices
- Standardize common workflows across your team
- Extend Claude's capabilities with custom tools
- Package complex multi-step procedures
- Share skills across projects via remote URLs

## Skill Discovery

Holon automatically discovers skills from the `.claude/skills/` directory in your workspace. Skills are loaded with the following precedence:

1. **CLI flags** (`--skill` or `--skills`) - highest priority
2. **Project config** (`.holon/config.yaml`)
3. **Spec file** (`metadata.skills` field)
4. **Auto-discovered** from `.claude/skills/` - lowest priority

**Skill Reference Formats:**
Skills can be specified using any of these formats:
- **Catalog references**: `skills:<package>` (skills.sh catalog), `gh:<owner>/<repo>` (GitHub)
- **Direct URLs**: `https://example.com/skill.zip#sha256=<checksum>`
- **Local paths**: `/path/to/skill`, `./relative/skill`
- **Workspace references**: `skill-name` (resolves to `.claude/skills/skill-name`)
- **Built-in**: `github-issue-solve`, `github-pr-fix`, `github-review`, `github-context`, `github-publish` (built-in skills)

Auto-discovered skills are loaded alphabetically by directory name.

### Remote Skills via Zip URLs

Holon supports installing skills directly from remote zip URLs. This allows you to:

- Distribute skills via GitHub releases, CDNs, or any HTTP/S endpoint
- Share skill collections without manual downloads
- Version skills using release tags
- Install multiple skills from a single zip archive

**URL Format:**
```
https://example.com/skills.zip
https://github.com/org/repo/archive/refs/tags/v1.2.3.zip
https://example.com/skills.zip#sha256=<checksum>
```

**Optional Integrity Check:**
Add a SHA256 checksum via URL fragment to verify download integrity:
```bash
# Without checksum (download proceeds without verification)
--skill https://github.com/myorg/skills/archive/v1.0.0.zip

# With checksum (fails if checksum doesn't match)
--skill "https://github.com/myorg/skills/archive/v1.0.0.zip#sha256=abc123..."
```

**Caching:**
Downloaded skills are cached in `~/.holon/cache/skills/` based on URL and checksum (if provided). Subsequent runs use the cache automatically.

**Multiple Skills in One Zip:**
When a zip contains multiple skill directories (each with `SKILL.md`), all skills are installed automatically. No need to specify individual skill paths.

### Holon Builtin Skills Package Format

Holon defines a canonical package format for distributing skill collections. This format is used for official Holon builtin skills and recommended for public skill distributions.

**Package Structure:**

```
holon-skills-v1.0.0.zip
├── skills/                    # Root directory for all skills
│   ├── github-context/        # Individual skill directories
│   │   ├── SKILL.md
│   │   └── scripts/
│   └── github-issue-solve/
│       ├── SKILL.md
│       └── references/
├── package.json               # Package metadata
└── checksums.txt             # Optional per-file checksums
```

**package.json Schema:**

```json
{
  "$schema": "https://schemas.holon.run/skill-package/v1",
  "name": "holon-skills",
  "version": "v1.0.0",
  "description": "Official Holon builtin skills collection",
  "skills": ["github-context", "github-issue-solve", "..."],
  "source": {
    "type": "git",
    "url": "https://github.com/holon-run/holon",
    "ref": "v1.0.0",
    "commit": "abc123def..."
  },
  "generated_at": "2026-02-07T16:00:00Z"
}
```

**Key Requirements:**

- **Filename**: `<package>-<version>.zip` (e.g., `holon-skills-v1.0.0.zip`)
- **Checksum**: Separate `.sha256` file with SHA256 of the zip archive
- **Root Structure**: Must contain `skills/` directory and `package.json`
- **Skill Directories**: Each skill must have `SKILL.md` file
- **Versioning**: SemVer with `v` prefix (e.g., `v1.0.0`, `v1.2.3-beta`)

**Using Official Holon Skills:**

```bash
# Download official skills package (with checksum verification)
holon run --goal "Fix issue" \
  --skill "https://github.com/holon-run/holon/releases/download/v1.0.0/holon-skills-v1.0.0.zip#sha256=<checksum>"

# The package installs all builtin skills:
# - github-context
# - github-issue-solve
# - github-pr-fix
# - github-publish
# - github-review
```

**Building Skill Packages:**

To build your own skill packages following the Holon format:

```bash
# Build from skills/ directory with version detection
make build-skills-package

# Build with explicit version
VERSION=v1.0.0 make build-skills-package

# Output: dist/skills/holon-skills-v1.0.0.zip + .sha256
```

See [Package Format Specification](/holon/output/package-format-spec.md) for complete details on the format, versioning rules, and build process.

**Package Verification:**

When downloading skill packages, you can verify the checksum independently:

```bash
# Download package and checksum
wget https://github.com/holon-run/holon/releases/download/v1.0.0/holon-skills-v1.0.0.zip{,.sha256}

# Verify before using
sha256sum -c holon-skills-v1.0.0.zip.sha256
# Output: holon-skills-v1.0.0.zip: OK
```

## Using Skills

### Method 1: Remote Skills (New!)

Install skills directly from remote URLs:

```bash
# Single skill from a URL
holon run --goal "Add tests" \
  --skill https://github.com/myorg/skills/releases/download/v1.0/testing-go.zip

# Multiple skills from a collection
holon run --goal "Build and test" \
  --skill https://github.com/myorg/skills/archive/refs/tags/v1.2.3.zip

# With integrity verification
holon run --goal "Deploy" \
  --skill "https://github.com/myorg/skills/releases/download/v2.0.0/deploy.zip#sha256=abc123def456..."
```

**Use Cases:**
- Team-maintained skill collections
- Public skill libraries
- Versioned skill distributions via GitHub releases
- CDN-hosted skill repositories

### Method 1.5: Using External Skill Ecosystems (New!)

Holon supports skills from external ecosystems through catalog adapters:

#### Skills from skills.sh Catalog (Vercel-style)

```bash
# Install a skill from the skills.sh catalog
holon run --goal "Add tests" --skill skills:testing-go

# The catalog automatically resolves to the correct download URL
# and includes checksum verification when available
```

The `skills:` prefix resolves packages through the [skills.sh](https://catalog.skills.sh) catalog, which provides:
- Automatic URL resolution to skill package downloads
- Built-in SHA256 checksums for integrity verification
- Community-curated skill packages

#### Skills from GitHub Repositories

```bash
# Install skills from a GitHub repository
holon run --goal "Fix issue" --skill gh:myorg/skills
```

The `gh:` prefix downloads the repository as a zip archive from GitHub and discovers all skill directories within it. Format:
- `gh:<owner>/<repo>` - downloads entire repository and discovers all skills

Note: The archive download uses the entire repository. All skill directories within the repository will be discovered and made available.

**Resolution Order:**
When you use catalog references, Holon resolves them in this order:
1. Direct URL (https://...)
2. Catalog reference (`skills:<package>`, `gh:<owner>/<repo>`)
3. Workspace skill (.claude/skills/{ref})
4. Absolute/relative filesystem path
5. Built-in skills

**Cache Location:**
Catalog-downloaded skills are cached in `~/.holon/cache/skills/` just like direct URLs.

### Method 2: Auto-Discovery (Recommended)

Create a `.claude/skills/` directory in your workspace:

```
my-project/
├── .claude/
│   └── skills/
│       ├── testing/
│       │   └── SKILL.md
│       ├── api-integration/
│       │   └── SKILL.md
│       └── code-review/
│           └── SKILL.md
```

These skills will be automatically available to Holon without additional configuration.

### Method 3: Project Configuration

Add skills to your `.holon/config.yaml`:

```yaml
# .holon/config.yaml
skills:
  - ./shared-skills/testing
  - ./shared-skills/documentation
  - https://github.com/myorg/skills/releases/download/v1.0/ci-cd.zip#sha256=abc123...
```

### Method 4: CLI Flags

Specify skills via command line:

```bash
# Single skill (repeatable flag)
holon run --goal "Add unit tests" --skill ./skills/testing

# Remote skill
holon run --goal "Add unit tests" --skill https://example.com/testing.zip

# Multiple skills
holon run --goal "Add tests and docs" \
  --skill ./skills/testing \
  --skill ./skills/documentation \
  --skill https://github.com/myorg/skills/archive/main.zip

# Comma-separated list
holon run --goal "Add tests" --skills ./skills/testing,https://example.com/linting.zip
```

### Method 5: Spec File

Include skills in your Holon spec:

```yaml
# task.yaml
version: "v1"
kind: Holon
metadata:
  name: "add-tests"
  skills:
    - ./skills/testing
    - ./skills/coverage
    - https://github.com/myorg/skills/archive/refs/tags/v1.0.0.zip
goal:
  description: "Add comprehensive unit tests"
```

## Creating a Skill

### Directory Structure

Each skill must be a directory containing a `SKILL.md` file:

```
my-skill/
├── SKILL.md              # Required: skill manifest
├── templates/            # Optional: code templates
│   └── test-template.ts
├── scripts/              # Optional: helper scripts
│   └── validate.sh
└── examples/             # Optional: usage examples
    └── usage.md
```

### SKILL.md Format

The `SKILL.md` file uses YAML frontmatter with Markdown content:

```markdown
---
name: testing
description: Expert test-writing skills for Go and TypeScript projects. Creates comprehensive unit tests, mocks, and integration tests.
---

# Testing Skill

You are a testing expert specializing in Go and TypeScript projects.

## Guidelines

- Write table-driven tests in Go
- Use testify for assertions
- Mock external dependencies
- Aim for >80% code coverage
- Include edge cases and error scenarios

## Test Structure

For Go packages, follow this structure:
```go
func TestFunctionName(t *testing.T) {
    tests := []struct {
        name    string
        input   InputType
        want    OutputType
        wantErr bool
    }{
        // test cases here
    }
    for _, tt := range tests {
        t.Run(tt.name, func(t *testing.T) {
            // test implementation
        })
    }
}
```

## Common Patterns

### Testing HTTP Handlers
```go
// Example handler test pattern
```

### Testing Database Operations
```go
// Example database test pattern
```
```

### Frontmatter Requirements

The YAML frontmatter must include:

- **`name`** (required): Short identifier for the skill (used in logs and debugging)
- **`description`** (required): One-line description that helps Claude understand when to use the skill

**Constraints:**
- `name` should be lowercase with hyphens (kebab-case)
- `name` should match the directory name
- `description` should be concise but descriptive
- Both fields are validated at skill load time

## Skill Precedence and Deduplication

When skills are specified from multiple sources, Holon applies the following rules:

1. **Precedence**: CLI > config > spec > auto-discovered
2. **Deduplication**: If the same skill path appears in multiple sources, the highest-precedence source wins
3. **Ordering**: Skills are applied in precedence order (CLI first, then auto-discovered alphabetically)

Example:
```bash
# CLI skill overrides auto-discovered skill of same name
holon run --goal "Test" --skill /custom/testing

# Even if .claude/skills/testing/ exists, /custom/testing is used
```

## Example Skills

See the `examples/skills/` directory for complete examples:

- **testing-go**: Go testing best practices
- **typescript-api**: TypeScript/Node.js API development patterns

## How Skills Work in Holon

1. **Resolution**: Skills are collected from all sources (CLI, config, spec, auto-discovered)
2. **Validation**: Each skill directory is validated for `SKILL.md` presence
3. **Staging**: Skills are copied to the workspace snapshot's `.claude/skills/` directory
4. **Execution**: The Claude agent discovers and uses skills as needed during task execution

## Best Practices

1. **Keep skills focused**: Each skill should address one domain or workflow
2. **Use descriptive names**: `testing-go` is better than `test`
3. **Provide examples**: Include usage examples in the SKILL.md content
4. **Version skills**: Use directory names like `testing-go-v1` for breaking changes
5. **Share skills**: Keep common skills in a shared location referenced by multiple projects
6. **Document dependencies**: If a skill requires specific tools, document them in SKILL.md

## Skill Artifacts and Outputs

### Artifact Ownership

Skills **own their artifacts**. This means:
- Skills define what output files they produce (artifact names, formats, schemas)
- Holon does not enforce specific artifact names beyond the required `manifest.json`
- Skills should document their output conventions for users and automation

### Required Artifact: manifest.json

All skill executions MUST produce `/holon/output/manifest.json`. This file is:
- **Runtime-owned**: Generated by the Holon infrastructure, not the skill
- **Stable across skills**: Same format for all skills
- **Machine-readable**: Enables automation and orchestration

The manifest includes an `artifacts` array that lists ALL outputs generated by your skill.

### Declaring Skill Artifacts

Skills SHOULD document their output artifacts in `SKILL.md`. For example:

```markdown
## Output Artifacts

This skill produces the following artifacts in `/holon/output/`:

- `analysis-report.json`: Structured analysis results with findings and recommendations
- `metrics.csv`: Performance metrics in CSV format
- `evidence/`: Directory containing screenshots, logs, and supporting evidence
```

### Recommended Artifacts for Code Skills

For **code workflow skills** (issue-to-PR, PR-fix, code review), these artifacts are RECOMMENDED but not required:

- `diff.patch`: Git-compatible patch of workspace changes
- `summary.md`: Human-readable summary of work performed
- `evidence/`: Supporting evidence (test results, logs, etc.)

### Skill-Defined Artifacts

Your skill may produce ANY artifacts with ANY names:

- **GitHub skill**: `pr-fix.json`, `publish-intent.json`
- **Documentation skill**: `docs-updated.md`, `broken-links.json`
- **Testing skill**: `coverage-report.html`, `test-results.xml`
- **Custom skills**: Any outputs relevant to the skill's purpose

### Example: Artifact Documentation in SKILL.md

```markdown
## Outputs

This skill generates the following artifacts:

### Required by Holon
- `manifest.json` (auto-generated by Holon runtime)

### Generated by this skill
- `test-results.xml`: JUnit-format test results
- `coverage-report.html`: HTML coverage report
- `test-queue.json`: List of tests that were queued and their status
- `evidence/`: Directory containing test logs and screenshots

### Schema

`test-queue.json` format:
```json
{
  "tests": [
    {
      "name": "TestUserLogin",
      "status": "passed",
      "duration": "1.2s"
    }
  ],
  "summary": {
    "total": 42,
    "passed": 40,
    "failed": 2,
    "skipped": 0
  }
}
```
```

### Publishing Side Effects

Skills MAY include scripts/tools to publish results (create PRs, post comments, send messages). This is the **recommended pattern**:

1. Agent writes a structured "intent" file (skill-defined name and schema)
2. Agent invokes a skill-provided script to apply that intent

Example:
```markdown
## Publishing

This skill uses a "plan as JSON, execute via script" pattern:

1. `pr-intent.json`: Agent writes the PR specification
2. `scripts/create-pr.sh`: Skill-provided script that creates/updates the PR

Example `pr-intent.json`:
```json
{
  "title": "Fix authentication bug",
  "body": "Fixes #123",
  "labels": ["bug", "authentication"],
  "branch": "fix/auth-bug"
}
```

The script is invoked automatically by the skill after generating the intent.
```

## Resources

- [Official Anthropic Skills Blog Post](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills)
- [anthropics/skills GitHub Repository](https://github.com/anthropics/skills)
- [Claude Code Skills Complete Guide](https://www.cursor-ide.com/blog/claude-code-skills)
- [Claude Agent Skills: A First Principles Deep Dive](https://leehanchung.github.io/blogs/2025/10/26/claude-skills-deep-dive/)

## Troubleshooting

### Remote Skill Download Failed

If remote skill download fails:
- Check the URL is accessible (try opening it in a browser)
- Verify the URL points to a valid zip file
- Check network connectivity and firewall settings
- Ensure the zip file contains directories with `SKILL.md` files
- Check Holon logs with `--log-level debug` for detailed error information
- Verify SHA256 checksum is correct (if using `#sha256=` fragment)

### Remote Skill Cache Issues

If cached remote skills cause problems:
```bash
# Clear the skills cache
rm -rf ~/.holon/cache/skills/
```

Skills will be re-downloaded on next run.

### Skill Not Found

If you see "skill path does not exist":
- Verify the skill directory path is correct
- Check that the path is relative to the current directory or use an absolute path
- Ensure the directory contains a `SKILL.md` file

### SKILL.md Validation Errors

If you see "skill directory missing required SKILL.md file":
- Ensure the file is named exactly `SKILL.md` (all caps)
- Check that the file is in the root of the skill directory
- Verify the file has valid YAML frontmatter with `name` and `description`

### Skills Not Being Used

If Claude doesn't seem to be using your skills:
- Check the logs to confirm skills were loaded: look for "Loaded skill: <name>"
- Verify the skill `description` is clear and relevant to the task
- Ensure the skill instructions in SKILL.md are specific and actionable
- Try using `--log-level debug` to see detailed skill loading information

### Conflicting Skills

If you have multiple skills with conflicting advice:
- Use more specific skill names (e.g., `testing-go` vs `testing-python`)
- Use CLI flags to explicitly select which skill to use
- Consider merging related skills into one comprehensive skill

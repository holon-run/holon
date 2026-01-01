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

## Skill Discovery

Holon automatically discovers skills from the `.claude/skills/` directory in your workspace. Skills are loaded with the following precedence:

1. **CLI flags** (`--skill` or `--skills`) - highest priority
2. **Project config** (`.holon/config.yaml`)
3. **Spec file** (`metadata.skills` field)
4. **Auto-discovered** from `.claude/skills/` - lowest priority

Auto-discovered skills are loaded alphabetically by directory name.

## Using Skills

### Method 1: Auto-Discovery (Recommended)

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

### Method 2: Project Configuration

Add skills to your `.holon/config.yaml`:

```yaml
# .holon/config.yaml
skills:
  - ./shared-skills/testing
  - ./shared-skills/documentation
```

### Method 3: CLI Flags

Specify skills via command line:

```bash
# Single skill (repeatable flag)
holon run --goal "Add unit tests" --skill ./skills/testing

# Multiple skills
holon run --goal "Add tests and docs" \
  --skill ./skills/testing \
  --skill ./skills/documentation

# Comma-separated list
holon run --goal "Add tests" --skills ./skills/testing,./skills/unit-tests
```

### Method 4: Spec File

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
- **typescript**: TypeScript project patterns
- **api-client**: REST API integration patterns

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

## Resources

- [Official Anthropic Skills Blog Post](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills)
- [anthropics/skills GitHub Repository](https://github.com/anthropics/skills)
- [Claude Code Skills Complete Guide](https://www.cursor-ide.com/blog/claude-code-skills)
- [Claude Agent Skills: A First Principles Deep Dive](https://leehanchung.github.io/blogs/2025/10/26/claude-skills-deep-dive/)

## Troubleshooting

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

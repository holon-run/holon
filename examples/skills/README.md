# Claude Skills Examples

This directory contains example Claude Skills that demonstrate how to package custom instructions and best practices for Holon.

## What are Skills?

Skills are directories containing `SKILL.md` files with:
- **YAML frontmatter**: Skill metadata (name, description)
- **Markdown content**: Instructions, patterns, and examples for Claude

## Using These Examples

Each example skill is a complete, ready-to-use template:

```bash
# Copy an example skill to your project
cp -r examples/skills/testing-go .claude/skills/

# Run Holon - skills are auto-discovered from .claude/skills/
holon run --goal "Add comprehensive unit tests"
```

## Example Skills

### testing-go
**Purpose**: Expert Go testing practices

Creates:
- Table-driven tests with multiple test cases
- Testify assertions for readable test code
- Interface mocks for external dependencies
- HTTP handler tests
- Coverage goals (>80%)

**Best for**: Go backend services, libraries, CLI tools

### typescript-api
**Purpose**: TypeScript/Node.js REST API development

Creates:
- Type-safe API handlers with TypeScript
- Zod validation schemas
- Express/Fastify route patterns
- Service layer architecture
- Proper async/await patterns

**Best for**: Node.js REST APIs, TypeScript backends

## Creating Your Own Skills

1. **Create a directory**:
   ```bash
   mkdir -p .claude/skills/my-skill
   ```

2. **Add SKILL.md** with frontmatter:
   ```markdown
   ---
   name: my-skill
   description: Brief description of when Claude should use this skill
   ---

   # My Skill

   Detailed instructions and examples...
   ```

3. **Use it**:
   ```bash
   # Auto-discovered from .claude/skills/
   holon run --goal "Task that uses my skill"
   ```

## SKILL.md Frontmatter

Required fields:
- `name`: Short identifier (kebab-case, matches directory name)
- `description`: One-line summary of the skill's purpose

See `docs/skills.md` for complete documentation on creating skills.

## Skill Discovery Order

Skills are loaded with the following precedence (highest to lowest):

1. **CLI flags**: `--skill ./path/to/skill`
2. **Project config**: `.holon/config.yaml` `skills` field
3. **Spec file**: `metadata.skills` in YAML specs
4. **Auto-discovery**: `.claude/skills/*/SKILL.md` (alphabetical)

This means auto-discovered skills (like these examples) have the lowest priority and can be overridden by more specific skills via CLI or config.

## Best Practices

1. **Keep skills focused**: One skill per domain or workflow
2. **Be specific**: Clear, actionable instructions
3. **Include examples**: Show, don't just tell
4. **Test skills**: Run Holon with `--log-level debug` to verify skill loading
5. **Version carefully**: Use directory names like `testing-go-v2` for breaking changes

## Further Reading

- Complete guide: `docs/skills.md`
- Official Anthropic documentation: [Blog post](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills)
- Community skills: [github.com/anthropics/skills](https://github.com/anthropics/skills)

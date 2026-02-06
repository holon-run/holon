// Package skills provides skill discovery, validation, and staging for Holon.
// Skills are directories containing SKILL.md files that can be auto-discovered
// from .claude/skills/ or explicitly specified via config/spec/CLI.
// Skills can also be downloaded from remote zip URLs or loaded from builtin.
package skills

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/holon-run/holon/pkg/builtin"
	"github.com/holon-run/holon/pkg/skills/catalog"
	"github.com/holon-run/holon/pkg/skills/remote"
)

const (
	// SkillsDir is the default skills directory relative to workspace root
	SkillsDir = ".claude/skills"
	// SkillManifestFile is the required manifest file in each skill directory
	SkillManifestFile = "SKILL.md"
)

// Skill represents a discovered or specified skill
type Skill struct {
	// Path is the absolute or relative path to the skill directory
	// For builtin skills, this is the reference (e.g., "github-issue-solve")
	Path string
	// Name is the base name of the skill directory
	Name string
	// Source indicates where the skill was specified from (cli, config, spec, discovered, builtin)
	Source string
	// Builtin indicates if this is a builtin skill (embedded in the binary)
	Builtin bool
}

// Resolver handles skill discovery, validation, and resolution
type Resolver struct {
	workspace       string
	cache           *remote.Cache
	catalogRegistry *catalog.Registry
}

// NewResolver creates a new skill resolver for the given workspace
func NewResolver(workspace string) *Resolver {
	// Initialize catalog registry with built-in catalog
	registry := catalog.NewRegistry()

	// Add built-in catalog adapter
	if builtinCatalog, err := catalog.BuiltinCatalog(); err == nil {
		registry.Register(builtinCatalog)
	}
	// If built-in catalog fails to load, log warning but continue
	// The catalog is optional - skills can still be resolved by other means

	// Add skills.sh catalog adapter
	registry.Register(catalog.NewSkillsShAdapter())

	// Add GitHub catalog adapter
	registry.Register(catalog.NewGitHubAdapter())

	return &Resolver{
		workspace:       workspace,
		cache:           remote.NewCache(""),
		catalogRegistry: registry,
	}
}

// Resolve resolves skills from multiple sources with proper precedence.
// Precedence: CLI > config > spec > auto-discovered.
// Returns a deduplicated list of skills in order of precedence.
// Skill refs can be local paths or remote zip URLs.
func (r *Resolver) Resolve(cliSkills []string, configSkills []string, specSkills []string) ([]Skill, error) {
	var skills []Skill

	// Compatibility aliases for renamed skills
	alias := map[string]string{
		"github-solve": "github-issue-solve",
	}

	// Add CLI skills (highest precedence)
	for _, ref := range cliSkills {
		if target, ok := alias[ref]; ok {
			ref = target
		}
		resolved, err := r.resolveSkillRef(ref, "cli")
		if err != nil {
			return nil, fmt.Errorf("invalid skill from CLI: %w", err)
		}
		skills = append(skills, resolved...)
	}

	// Add config skills
	for _, ref := range configSkills {
		if target, ok := alias[ref]; ok {
			ref = target
		}
		resolved, err := r.resolveSkillRef(ref, "config")
		if err != nil {
			return nil, fmt.Errorf("invalid skill from config: %w", err)
		}
		skills = append(skills, resolved...)
	}

	// Add spec skills
	for _, ref := range specSkills {
		if target, ok := alias[ref]; ok {
			ref = target
		}
		resolved, err := r.resolveSkillRef(ref, "spec")
		if err != nil {
			return nil, fmt.Errorf("invalid skill from spec: %w", err)
		}
		skills = append(skills, resolved...)
	}

	// Auto-discover skills from .claude/skills/
	discovered, err := r.discover()
	if err != nil {
		return nil, fmt.Errorf("failed to discover skills: %w", err)
	}

	// Add discovered skills that weren't already specified
	for _, skill := range discovered {
		if !r.containsSkill(skills, skill) {
			skills = append(skills, skill)
		}
	}

	return skills, nil
}

// discover auto-discovers skills from .claude/skills/*/SKILL.md
func (r *Resolver) discover() ([]Skill, error) {
	skillsPath := filepath.Join(r.workspace, SkillsDir)

	// If skills directory doesn't exist, return empty list
	if _, err := os.Stat(skillsPath); os.IsNotExist(err) {
		return []Skill{}, nil
	}

	// Read the skills directory
	entries, err := os.ReadDir(skillsPath)
	if err != nil {
		return nil, fmt.Errorf("failed to read skills directory: %w", err)
	}

	var skills []Skill
	for _, entry := range entries {
		// Skip non-directories
		if !entry.IsDir() {
			continue
		}

		skillName := entry.Name()
		skillPath := filepath.Join(skillsPath, skillName)

		// Validate that SKILL.md exists
		skillManifestPath := filepath.Join(skillPath, SkillManifestFile)
		if _, err := os.Stat(skillManifestPath); os.IsNotExist(err) {
			// Skip directories without SKILL.md
			continue
		} else if err != nil {
			return nil, fmt.Errorf("failed to check skill manifest in %s: %w", skillPath, err)
		}

		skills = append(skills, Skill{
			Path:   skillPath,
			Name:   skillName,
			Source: "discovered",
		})
	}

	// Sort for consistent ordering
	sort.Slice(skills, func(i, j int) bool {
		return skills[i].Name < skills[j].Name
	})

	return skills, nil
}

// resolveSkillRef resolves a single skill reference to one or more skills
// Resolution order (first match wins):
// 1. Remote URL
// 2. Catalog reference (skills:, gh:)
// 3. Workspace skill (.claude/skills/{ref})
// 4. Absolute/relative path
// 5. Builtin skill
// Returns multiple skills for URLs (zip can contain multiple skills) or single skill for other types
func (r *Resolver) resolveSkillRef(ref string, source string) ([]Skill, error) {
	// 1. Check if it's a URL
	if remote.IsURL(ref) {
		// Parse the URL reference
		skillRef, err := remote.ParseSkillRef(ref)
		if err != nil {
			return nil, fmt.Errorf("invalid URL reference: %w", err)
		}

		// Download and extract
		skillPaths, err := r.cache.DownloadAndExtract(skillRef)
		if err != nil {
			return nil, fmt.Errorf("failed to download remote skills: %w", err)
		}

		// Convert to Skill structs
		var skills []Skill
		for _, skillPath := range skillPaths {
			skillName := filepath.Base(skillPath)

			// Validate the downloaded skill
			skillManifestPath := filepath.Join(skillPath, SkillManifestFile)
			if _, err := os.Stat(skillManifestPath); err != nil {
				return nil, fmt.Errorf("downloaded skill missing %s: %s", SkillManifestFile, skillPath)
			}

			skills = append(skills, Skill{
				Path:   skillPath,
				Name:   skillName,
				Source: source,
			})
		}

		return skills, nil
	}

	// 2. Check if it's a catalog reference
	if catalog.IsCatalogRef(ref) {
		return r.resolveCatalogRef(ref, source)
	}

	// 3. Check workspace skills
	workspaceSkillPath := filepath.Join(r.workspace, SkillsDir, ref)
	if info, err := os.Stat(workspaceSkillPath); err == nil && info.IsDir() {
		// Validate that SKILL.md exists
		skillManifestPath := filepath.Join(workspaceSkillPath, SkillManifestFile)
		if _, err := os.Stat(skillManifestPath); err == nil {
			// Found valid workspace skill
			skillName := filepath.Base(workspaceSkillPath)
			return []Skill{{
				Path:    workspaceSkillPath,
				Name:    skillName,
				Source:  source,
				Builtin: false,
			}}, nil
		}
	}

	// 4. Check absolute/relative path
	if _, err := os.Stat(ref); err == nil {
		skill, err := r.ValidateAndNormalize(ref, source)
		if err != nil {
			return nil, err
		}
		skill.Builtin = false
		return []Skill{skill}, nil
	}

	// 5. Check builtin skills
	if builtin.Has(ref) {
		// Use the reference as the path (for builtin skills, Path is the ref)
		skillName := filepath.Base(ref)
		return []Skill{{
			Path:    ref,
			Name:    skillName,
			Source:  source,
			Builtin: true,
		}}, nil
	}

	// Not found anywhere
	return nil, fmt.Errorf("skill not found: %s (checked: URL, catalog, workspace, filesystem, builtin)", ref)
}

// resolveCatalogRef resolves a catalog reference to one or more skills
func (r *Resolver) resolveCatalogRef(ref string, source string) ([]Skill, error) {
	// Try to resolve using catalog registry
	entry, err := r.catalogRegistry.Resolve(ref)
	if err != nil {
		// If catalog resolution fails, return error
		return nil, fmt.Errorf("failed to resolve catalog reference %s: %w", ref, err)
	}

	// Build URL with optional checksum
	url := entry.URL
	if entry.SHA256 != "" {
		url += "#sha256=" + entry.SHA256
	}

	// Parse the URL reference
	skillRef, err := remote.ParseSkillRef(url)
	if err != nil {
		return nil, fmt.Errorf("invalid catalog URL reference: %w", err)
	}

	// Download and extract
	skillPaths, err := r.cache.DownloadAndExtract(skillRef)
	if err != nil {
		return nil, fmt.Errorf("failed to download catalog skill: %w", err)
	}

	// Convert to Skill structs
	var skills []Skill
	for _, skillPath := range skillPaths {
		skillName := filepath.Base(skillPath)

		// Validate the downloaded skill
		skillManifestPath := filepath.Join(skillPath, SkillManifestFile)
		if _, err := os.Stat(skillManifestPath); err != nil {
			return nil, fmt.Errorf("downloaded catalog skill missing %s: %s", SkillManifestFile, skillPath)
		}

		skills = append(skills, Skill{
			Path:   skillPath,
			Name:   skillName,
			Source: source,
		})
	}

	return skills, nil
}

// ValidateAndNormalize validates a skill path and normalizes it to an absolute path
// Exported version for use by other packages
func (r *Resolver) ValidateAndNormalize(path string, source string) (Skill, error) {
	// Convert to absolute path
	absPath, err := filepath.Abs(path)
	if err != nil {
		return Skill{}, fmt.Errorf("failed to resolve absolute path for %s: %w", path, err)
	}

	// Check if path exists
	info, err := os.Stat(absPath)
	if err != nil {
		if os.IsNotExist(err) {
			return Skill{}, fmt.Errorf("skill path does not exist: %s", path)
		}
		return Skill{}, fmt.Errorf("failed to access skill path %s: %w", path, err)
	}

	// Check if it's a directory
	if !info.IsDir() {
		return Skill{}, fmt.Errorf("skill path is not a directory: %s", path)
	}

	// Check for SKILL.md
	skillManifestPath := filepath.Join(absPath, SkillManifestFile)
	if _, err := os.Stat(skillManifestPath); err != nil {
		if os.IsNotExist(err) {
			return Skill{}, fmt.Errorf("skill directory missing required %s file: %s (expected at %s)",
				SkillManifestFile, path, skillManifestPath)
		}
		return Skill{}, fmt.Errorf("failed to check for %s in %s: %w", SkillManifestFile, path, err)
	}

	// Get the skill directory name
	skillName := filepath.Base(absPath)

	return Skill{
		Path:    absPath,
		Name:    skillName,
		Source:  source,
		Builtin: false,
	}, nil
}

// containsSkill checks if a skill is already in the list (by path)
func (r *Resolver) containsSkill(skills []Skill, skill Skill) bool {
	for _, s := range skills {
		if s.Path == skill.Path {
			return true
		}
	}
	return false
}

// Stage copies skills to the workspace snapshot's .claude/skills/ directory
// If includeSkillsPrefix is false, copies skills directly to dest (for external mounting)
func Stage(workspaceDest string, skills []Skill) error {
	return StageWithPrefix(workspaceDest, skills, true)
}

// StageWithPrefix copies skills with control over whether to add .claude/skills prefix
// If includeSkillsPrefix is true, copies to dest/.claude/skills/ (for workspace staging)
// If includeSkillsPrefix is false, copies to dest/ (for external mounting to /root/.claude/skills)
func StageWithPrefix(workspaceDest string, skills []Skill, includeSkillsPrefix bool) error {
	if len(skills) == 0 {
		return nil
	}

	var destSkillsDir string
	if includeSkillsPrefix {
		// Create .claude/skills directory in workspace
		destSkillsDir = filepath.Join(workspaceDest, SkillsDir)
	} else {
		// Use destination directory directly (for external mounting)
		destSkillsDir = workspaceDest
	}

	if err := os.MkdirAll(destSkillsDir, 0755); err != nil {
		return fmt.Errorf("failed to create skills directory: %w", err)
	}

	// Copy each skill
	for _, skill := range skills {
		// Use the skill name (base directory) for all skills
		// For builtin skills: "github-issue-solve" -> Name is "github-issue-solve"
		// For filesystem skills: Name is the directory name
		destPath := filepath.Join(destSkillsDir, skill.Name)

		// Check if destination already exists
		if _, err := os.Stat(destPath); err == nil {
			// Directory exists, skip it
			continue
		} else if !os.IsNotExist(err) {
			return fmt.Errorf("failed to check skill destination %s: %w", destPath, err)
		}

		// Handle builtin skills differently - extract from embedded FS
		if skill.Builtin {
			if err := stageBuiltinSkill(skill.Path, destPath); err != nil {
				return fmt.Errorf("failed to stage builtin skill %s: %w", skill.Name, err)
			}
			continue
		}

		// Copy the skill directory from filesystem
		if err := copyDir(skill.Path, destPath); err != nil {
			return fmt.Errorf("failed to copy skill %s: %w", skill.Name, err)
		}
	}

	return nil
}

// stageBuiltinSkill extracts a builtin skill from the embedded FS to the destination
func stageBuiltinSkill(ref string, destPath string) error {
	// Load all files from the builtin skill directory
	files, err := builtin.LoadDir(ref)
	if err != nil {
		return fmt.Errorf("failed to load builtin skill %s: %w", ref, err)
	}

	// Create destination directory
	if err := os.MkdirAll(destPath, 0755); err != nil {
		return fmt.Errorf("failed to create directory: %w", err)
	}

	// Write each file
	for relPath, content := range files {
		filePath := filepath.Join(destPath, relPath)
		if err := os.MkdirAll(filepath.Dir(filePath), 0755); err != nil {
			return fmt.Errorf("failed to create directory: %w", err)
		}
		if err := os.WriteFile(filePath, content, 0644); err != nil {
			return fmt.Errorf("failed to write file %s: %w", relPath, err)
		}
	}

	return nil
}

// copyDir recursively copies a directory tree, handling symlinks and preserving permissions
func copyDir(src, dst string) error {
	// Create destination directory
	if err := os.MkdirAll(dst, 0755); err != nil {
		return fmt.Errorf("failed to create directory: %w", err)
	}

	// Read source directory
	entries, err := os.ReadDir(src)
	if err != nil {
		return fmt.Errorf("failed to read directory: %w", err)
	}

	// Copy each entry
	for _, entry := range entries {
		srcPath := filepath.Join(src, entry.Name())
		dstPath := filepath.Join(dst, entry.Name())

		// Check if it's a symbolic link
		if entry.Type()&os.ModeSymlink != 0 {
			// Read the symlink target
			target, err := os.Readlink(srcPath)
			if err != nil {
				return fmt.Errorf("failed to read symlink %s: %w", srcPath, err)
			}
			// Create the same symlink at destination
			if err := os.Symlink(target, dstPath); err != nil {
				return fmt.Errorf("failed to create symlink %s: %w", dstPath, err)
			}
			continue
		}

		if entry.IsDir() {
			// Recursively copy subdirectory
			if err := copyDir(srcPath, dstPath); err != nil {
				return err
			}
		} else {
			// Copy file
			if err := copyFile(srcPath, dstPath); err != nil {
				return err
			}
		}
	}

	return nil
}

// copyFile copies a single file, preserving permissions
func copyFile(src, dst string) error {
	// Get source file info to preserve permissions
	info, err := os.Stat(src)
	if err != nil {
		return fmt.Errorf("failed to stat source file: %w", err)
	}

	data, err := os.ReadFile(src)
	if err != nil {
		return fmt.Errorf("failed to read file: %w", err)
	}

	if err := os.WriteFile(dst, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	// Preserve original file permissions
	if err := os.Chmod(dst, info.Mode().Perm()); err != nil {
		return fmt.Errorf("failed to set file permissions: %w", err)
	}

	return nil
}

// ParseSkillsList parses a comma-separated list of skill paths
func ParseSkillsList(skillsStr string) []string {
	if skillsStr == "" {
		return []string{}
	}

	var skills []string
	for _, s := range strings.Split(skillsStr, ",") {
		s = strings.TrimSpace(s)
		if s != "" {
			skills = append(skills, s)
		}
	}
	return skills
}

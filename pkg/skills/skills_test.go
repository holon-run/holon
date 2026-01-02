package skills

import (
	"archive/zip"
	"bytes"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

func TestResolver_Discover(t *testing.T) {
	// Create a temporary directory for testing
	tempDir, err := os.MkdirTemp("", "holon-skills-test-*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tempDir)

	// Create .claude/skills directory structure
	skillsDir := filepath.Join(tempDir, ".claude", "skills")
	if err := os.MkdirAll(skillsDir, 0755); err != nil {
		t.Fatalf("failed to create skills dir: %v", err)
	}

	// Create a valid skill directory
	skill1Dir := filepath.Join(skillsDir, "skill1")
	if err := os.MkdirAll(skill1Dir, 0755); err != nil {
		t.Fatalf("failed to create skill1 dir: %v", err)
	}
	skill1Manifest := filepath.Join(skill1Dir, "SKILL.md")
	if err := os.WriteFile(skill1Manifest, []byte("# Skill 1\n"), 0644); err != nil {
		t.Fatalf("failed to create skill1 manifest: %v", err)
	}

	// Create another valid skill
	skill2Dir := filepath.Join(skillsDir, "skill2")
	if err := os.MkdirAll(skill2Dir, 0755); err != nil {
		t.Fatalf("failed to create skill2 dir: %v", err)
	}
	skill2Manifest := filepath.Join(skill2Dir, "SKILL.md")
	if err := os.WriteFile(skill2Manifest, []byte("# Skill 2\n"), 0644); err != nil {
		t.Fatalf("failed to create skill2 manifest: %v", err)
	}

	// Create an invalid directory (no SKILL.md)
	invalidDir := filepath.Join(skillsDir, "invalid")
	if err := os.MkdirAll(invalidDir, 0755); err != nil {
		t.Fatalf("failed to create invalid dir: %v", err)
	}

	// Test discovery
	resolver := NewResolver(tempDir)
	discovered, err := resolver.discover()
	if err != nil {
		t.Fatalf("discover failed: %v", err)
	}

	// Should find 2 skills (invalid directory should be skipped)
	if len(discovered) != 2 {
		t.Errorf("expected 2 skills, got %d", len(discovered))
	}

	// Check that skill names are correct
	names := make(map[string]bool)
	for _, skill := range discovered {
		names[skill.Name] = true
		if skill.Source != "discovered" {
			t.Errorf("expected source 'discovered', got '%s'", skill.Source)
		}
	}

	if !names["skill1"] || !names["skill2"] {
		t.Errorf("missing expected skills: got %v", names)
	}
}

func TestResolver_ValidateAndNormalize(t *testing.T) {
	// Create a temporary directory for testing
	tempDir, err := os.MkdirTemp("", "holon-skills-test-*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tempDir)

	// Create a valid skill directory
	skillDir := filepath.Join(tempDir, "test-skill")
	if err := os.MkdirAll(skillDir, 0755); err != nil {
		t.Fatalf("failed to create skill dir: %v", err)
	}
	skillManifest := filepath.Join(skillDir, "SKILL.md")
	if err := os.WriteFile(skillManifest, []byte("# Test Skill\n"), 0644); err != nil {
		t.Fatalf("failed to create skill manifest: %v", err)
	}

	resolver := NewResolver(tempDir)

	// Test valid skill
	skill, err := resolver.ValidateAndNormalize(skillDir, "test")
	if err != nil {
		t.Fatalf("ValidateAndNormalize failed: %v", err)
	}

	if skill.Name != "test-skill" {
		t.Errorf("expected name 'test-skill', got '%s'", skill.Name)
	}

	if skill.Source != "test" {
		t.Errorf("expected source 'test', got '%s'", skill.Source)
	}

	if !filepath.IsAbs(skill.Path) {
		t.Errorf("expected absolute path, got '%s'", skill.Path)
	}
}

func TestResolver_ValidateAndNormalize_Errors(t *testing.T) {
	tempDir, err := os.MkdirTemp("", "holon-skills-test-*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tempDir)

	resolver := NewResolver(tempDir)

	// Test non-existent path
	_, err = resolver.ValidateAndNormalize("/nonexistent/path", "test")
	if err == nil {
		t.Error("expected error for non-existent path, got nil")
	}

	// Create a directory without SKILL.md
	invalidDir := filepath.Join(tempDir, "invalid")
	if err := os.MkdirAll(invalidDir, 0755); err != nil {
		t.Fatalf("failed to create invalid dir: %v", err)
	}

	_, err = resolver.ValidateAndNormalize(invalidDir, "test")
	if err == nil {
		t.Error("expected error for directory without SKILL.md, got nil")
	}

	// Create a file instead of directory
	filePath := filepath.Join(tempDir, "file.txt")
	if err := os.WriteFile(filePath, []byte("test"), 0644); err != nil {
		t.Fatalf("failed to create test file: %v", err)
	}

	_, err = resolver.ValidateAndNormalize(filePath, "test")
	if err == nil {
		t.Error("expected error for file path, got nil")
	}
}

func TestParseSkillsList(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		expected []string
	}{
		{
			name:     "empty string",
			input:    "",
			expected: []string{},
		},
		{
			name:     "single skill",
			input:    "/path/to/skill",
			expected: []string{"/path/to/skill"},
		},
		{
			name:     "multiple skills",
			input:    "/skill1,/skill2,/skill3",
			expected: []string{"/skill1", "/skill2", "/skill3"},
		},
		{
			name:     "skills with spaces",
			input:    "/skill1, /skill2 , /skill3",
			expected: []string{"/skill1", "/skill2", "/skill3"},
		},
		{
			name:     "empty entries",
			input:    "/skill1,,/skill2,",
			expected: []string{"/skill1", "/skill2"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := ParseSkillsList(tt.input)
			if len(result) != len(tt.expected) {
				t.Errorf("expected %d skills, got %d", len(tt.expected), len(result))
				return
			}
			for i, skill := range result {
				if skill != tt.expected[i] {
					t.Errorf("skill %d: expected '%s', got '%s'", i, tt.expected[i], skill)
				}
			}
		})
	}
}

func TestStage(t *testing.T) {
	// Create source skills
	sourceDir, err := os.MkdirTemp("", "holon-skills-source-*")
	if err != nil {
		t.Fatalf("failed to create source dir: %v", err)
	}
	defer os.RemoveAll(sourceDir)

	skill1Dir := filepath.Join(sourceDir, "skill1")
	if err := os.MkdirAll(skill1Dir, 0755); err != nil {
		t.Fatalf("failed to create skill1 dir: %v", err)
	}
	skill1Manifest := filepath.Join(skill1Dir, "SKILL.md")
	if err := os.WriteFile(skill1Manifest, []byte("# Skill 1\n"), 0644); err != nil {
		t.Fatalf("failed to create skill1 manifest: %v", err)
	}

	// Create destination workspace
	workspaceDir, err := os.MkdirTemp("", "holon-workspace-*")
	if err != nil {
		t.Fatalf("failed to create workspace dir: %v", err)
	}
	defer os.RemoveAll(workspaceDir)

	// Stage skills
	skillsList := []Skill{
		{Path: skill1Dir, Name: "skill1", Source: "cli"},
	}

	err = Stage(workspaceDir, skillsList)
	if err != nil {
		t.Fatalf("Stage failed: %v", err)
	}

	// Verify skill was copied
	destSkillPath := filepath.Join(workspaceDir, ".claude", "skills", "skill1", "SKILL.md")
	if _, err := os.Stat(destSkillPath); os.IsNotExist(err) {
		t.Errorf("skill was not copied to destination: %s", destSkillPath)
	}
}

func TestResolver_Resolve(t *testing.T) {
	// Create a temporary workspace for testing
	tempDir, err := os.MkdirTemp("", "holon-skills-resolve-*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	defer os.RemoveAll(tempDir)

	// Create .claude/skills directory structure for discovered skills
	skillsDir := filepath.Join(tempDir, ".claude", "skills")
	if err := os.MkdirAll(skillsDir, 0755); err != nil {
		t.Fatalf("failed to create skills dir: %v", err)
	}

	// Create discovered skill1
	discoveredSkill1Dir := filepath.Join(skillsDir, "discovered1")
	if err := os.MkdirAll(discoveredSkill1Dir, 0755); err != nil {
		t.Fatalf("failed to create discovered1 dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(discoveredSkill1Dir, "SKILL.md"), []byte("# Discovered 1\n"), 0644); err != nil {
		t.Fatalf("failed to create discovered1 manifest: %v", err)
	}

	// Create discovered skill2
	discoveredSkill2Dir := filepath.Join(skillsDir, "discovered2")
	if err := os.MkdirAll(discoveredSkill2Dir, 0755); err != nil {
		t.Fatalf("failed to create discovered2 dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(discoveredSkill2Dir, "SKILL.md"), []byte("# Discovered 2\n"), 0644); err != nil {
		t.Fatalf("failed to create discovered2 manifest: %v", err)
	}

	// Create external skills for CLI/config/spec
	cliSkillDir := filepath.Join(tempDir, "cli-skill")
	if err := os.MkdirAll(cliSkillDir, 0755); err != nil {
		t.Fatalf("failed to create cli-skill dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cliSkillDir, "SKILL.md"), []byte("# CLI Skill\n"), 0644); err != nil {
		t.Fatalf("failed to create cli-skill manifest: %v", err)
	}

	configSkillDir := filepath.Join(tempDir, "config-skill")
	if err := os.MkdirAll(configSkillDir, 0755); err != nil {
		t.Fatalf("failed to create config-skill dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(configSkillDir, "SKILL.md"), []byte("# Config Skill\n"), 0644); err != nil {
		t.Fatalf("failed to create config-skill manifest: %v", err)
	}

	specSkillDir := filepath.Join(tempDir, "spec-skill")
	if err := os.MkdirAll(specSkillDir, 0755); err != nil {
		t.Fatalf("failed to create spec-skill dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(specSkillDir, "SKILL.md"), []byte("# Spec Skill\n"), 0644); err != nil {
		t.Fatalf("failed to create spec-skill manifest: %v", err)
	}

	resolver := NewResolver(tempDir)

	// Test 1: Precedence - CLI > config > spec > discovered
	t.Run("precedence", func(t *testing.T) {
		cliSkills := []string{cliSkillDir}
		configSkills := []string{configSkillDir}
		specSkills := []string{specSkillDir}

		resolved, err := resolver.Resolve(cliSkills, configSkills, specSkills)
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		// Should have 5 skills: cli, config, spec, and 2 discovered
		if len(resolved) != 5 {
			t.Errorf("expected 5 skills, got %d", len(resolved))
		}

		// Check precedence order
		sources := make([]string, len(resolved))
		for i, skill := range resolved {
			sources[i] = skill.Source
		}

		// CLI should be first
		if sources[0] != "cli" {
			t.Errorf("expected first source to be 'cli', got '%s'", sources[0])
		}
		// config should be second
		if sources[1] != "config" {
			t.Errorf("expected second source to be 'config', got '%s'", sources[1])
		}
		// spec should be third
		if sources[2] != "spec" {
			t.Errorf("expected third source to be 'spec', got '%s'", sources[2])
		}
		// discovered should be last (sorted alphabetically)
		if sources[3] != "discovered" || sources[4] != "discovered" {
			t.Errorf("expected last two sources to be 'discovered', got '%s' and '%s'", sources[3], sources[4])
		}
	})

	// Test 2: Deduplication - discovered skills don't duplicate explicit skills
	t.Run("deduplication", func(t *testing.T) {
		// Add discovered1 (which is also in .claude/skills/) to CLI
		cliSkills := []string{discoveredSkill1Dir} // Same as discovered1 from auto-discovery
		configSkills := []string{}
		specSkills := []string{}

		resolved, err := resolver.Resolve(cliSkills, configSkills, specSkills)
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		// Should only have 2 skills: discovered1 (from CLI, deduped from discovered) and discovered2
		if len(resolved) != 2 {
			t.Errorf("expected 2 skills (deduplicated), got %d", len(resolved))
		}

		// First skill should be from CLI (highest precedence)
		if resolved[0].Source != "cli" {
			t.Errorf("expected first skill source to be 'cli', got '%s'", resolved[0].Source)
		}
		if resolved[0].Name != "discovered1" {
			t.Errorf("expected first skill name to be 'discovered1', got '%s'", resolved[0].Name)
		}

		// Second skill should be discovered2 (auto-discovered, not in CLI/config/spec)
		if resolved[1].Source != "discovered" {
			t.Errorf("expected second skill source to be 'discovered', got '%s'", resolved[1].Source)
		}
		if resolved[1].Name != "discovered2" {
			t.Errorf("expected second skill name to be 'discovered2', got '%s'", resolved[1].Name)
		}
	})

	// Test 3: Empty lists - only discovered skills
	t.Run("only discovered", func(t *testing.T) {
		resolved, err := resolver.Resolve([]string{}, []string{}, []string{})
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		// Should have 2 discovered skills
		if len(resolved) != 2 {
			t.Errorf("expected 2 discovered skills, got %d", len(resolved))
		}

		for _, skill := range resolved {
			if skill.Source != "discovered" {
				t.Errorf("expected all sources to be 'discovered', got '%s'", skill.Source)
			}
		}
	})

	// Test 4: Invalid skill path
	t.Run("invalid path", func(t *testing.T) {
		invalidSkills := []string{"/nonexistent/skill"}
		_, err := resolver.Resolve(invalidSkills, []string{}, []string{})
		if err == nil {
			t.Error("expected error for invalid skill path, got nil")
		}
	})

	// Test 5: Remote skill URL
	t.Run("remote URL", func(t *testing.T) {
		// Create a test zip with skills
		zipData := createTestZipData()

		// Create test server
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Content-Type", "application/zip")
			w.WriteHeader(http.StatusOK)
			w.Write(zipData)
		}))
		defer server.Close()

		// Create a separate temporary workspace without discovered skills
		cleanWorkspace, err := os.MkdirTemp("", "holon-remote-test-*")
		if err != nil {
			t.Fatalf("failed to create clean workspace: %v", err)
		}
		defer os.RemoveAll(cleanWorkspace)

		cleanResolver := NewResolver(cleanWorkspace)

		// Resolve remote URL
		cliSkills := []string{server.URL + "/skills.zip"}
		resolved, err := cleanResolver.Resolve(cliSkills, []string{}, []string{})
		if err != nil {
			t.Fatalf("Resolve failed for remote URL: %v", err)
		}

		// Should find 2 skills from the zip
		if len(resolved) != 2 {
			t.Errorf("expected 2 skills from remote URL, got %d", len(resolved))
		}

		// Check source is 'cli'
		for _, skill := range resolved {
			if skill.Source != "cli" {
				t.Errorf("expected source 'cli', got '%s'", skill.Source)
			}
		}
	})
}

// createTestZipData creates a test zip file with two skills
func createTestZipData() []byte {
	buf := new(bytes.Buffer)
	zipWriter := zip.NewWriter(buf)

	// Create skill1
	w, err := zipWriter.Create("skill1/SKILL.md")
	if err != nil {
		panic("failed to create zip entry skill1/SKILL.md: " + err.Error())
	}
	if _, err := w.Write([]byte("# Skill 1\n")); err != nil {
		panic("failed to write to zip entry skill1/SKILL.md: " + err.Error())
	}

	// Create skill2
	w, err = zipWriter.Create("skill2/SKILL.md")
	if err != nil {
		panic("failed to create zip entry skill2/SKILL.md: " + err.Error())
	}
	if _, err := w.Write([]byte("# Skill 2\n")); err != nil {
		panic("failed to write to zip entry skill2/SKILL.md: " + err.Error())
	}

	if err := zipWriter.Close(); err != nil {
		panic("failed to close zip writer: " + err.Error())
	}
	return buf.Bytes()
}

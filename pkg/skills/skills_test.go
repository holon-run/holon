package skills

import (
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

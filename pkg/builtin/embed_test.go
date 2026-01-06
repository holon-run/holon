// Package builtin provides embedded builtin skills for Holon.
package builtin

import (
	"testing"
)

func TestFS(t *testing.T) {
	f := FS()
	if f == nil {
		t.Fatal("FS() returned nil, expected non-nil filesystem")
	}
}

func TestHas(t *testing.T) {
	tests := []struct {
		name      string
		ref       string
		want      bool
	}{
		{
			name: "existing builtin skill",
			ref:  "github/solve",
			want: true,
		},
		{
			name: "non-existent skill",
			ref:  "nonexistent/skill",
			want: false,
		},
		{
			name: "empty reference",
			ref:  "",
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := Has(tt.ref)
			if got != tt.want {
				t.Errorf("Has(%q) = %v, want %v", tt.ref, got, tt.want)
			}
		})
	}
}

func TestLoad(t *testing.T) {
	tests := []struct {
		name      string
		ref       string
		wantErr   bool
		validate  func([]byte) error
	}{
		{
			name:    "existing builtin skill",
			ref:     "github/solve",
			wantErr: false,
			validate: func(content []byte) error {
				if len(content) == 0 {
					t.Errorf("Load() returned empty content")
				}
				// Check that it starts with expected markdown
				if len(content) < 20 {
					t.Errorf("Load() content too short")
				}
				return nil
			},
		},
		{
			name:    "non-existent skill",
			ref:     "nonexistent/skill",
			wantErr: true,
		},
		{
			name:    "empty reference",
			ref:     "",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := Load(tt.ref)
			if (err != nil) != tt.wantErr {
				t.Errorf("Load(%q) error = %v, wantErr %v", tt.ref, err, tt.wantErr)
				return
			}
			if !tt.wantErr && tt.validate != nil {
				if err := tt.validate(got); err != nil {
					t.Errorf("Load(%q) validation failed: %v", tt.ref, err)
				}
			}
		})
	}
}

func TestLoadDir(t *testing.T) {
	tests := []struct {
		name         string
		ref          string
		wantErr      bool
		minFileCount int
	}{
		{
			name:         "existing builtin skill directory",
			ref:          "github/solve",
			wantErr:      false,
			minFileCount: 1, // At least SKILL.md
		},
		{
			name:         "non-existent skill",
			ref:          "nonexistent/skill",
			wantErr:      true,
			minFileCount: 0,
		},
		{
			name:         "empty reference",
			ref:          "",
			wantErr:      true,
			minFileCount: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := LoadDir(tt.ref)
			if (err != nil) != tt.wantErr {
				t.Errorf("LoadDir(%q) error = %v, wantErr %v", tt.ref, err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if len(got) < tt.minFileCount {
					t.Errorf("LoadDir(%q) returned %d files, want at least %d", tt.ref, len(got), tt.minFileCount)
				}
				// Check that SKILL.md is present
				hasSkillMD := false
				for path := range got {
					if path == "SKILL.md" || len(path) >= len("SKILL.md") && path[len(path)-len("SKILL.md"):] == "SKILL.md" {
						hasSkillMD = true
						break
					}
				}
				if !hasSkillMD {
					t.Errorf("LoadDir(%q) did not return SKILL.md file", tt.ref)
				}
			}
		})
	}
}

func TestLoadDirEdgeCase(t *testing.T) {
	// Test the edge case where ref equals path (directory itself during walk)
	// This should not cause a slicing panic
	files, err := LoadDir("github/solve")
	if err != nil {
		t.Fatalf("LoadDir() error = %v", err)
	}
	if len(files) == 0 {
		t.Error("LoadDir() returned no files")
	}
	// Verify all paths are valid
	for path := range files {
		if path == "" {
			t.Error("LoadDir() returned empty path")
		}
	}
}

func TestList(t *testing.T) {
	skills, err := List()
	if err != nil {
		t.Fatalf("List() error = %v", err)
	}
	// Should at least contain github/solve
	found := false
	for _, skill := range skills {
		if skill == "github/solve" {
			found = true
			break
		}
	}
	if !found {
		t.Error("List() did not return 'github/solve' skill")
	}
	// All returned skills should be non-empty
	for _, skill := range skills {
		if skill == "" {
			t.Error("List() returned empty skill reference")
		}
	}
}

func TestListConsistency(t *testing.T) {
	// Test that List() returns skills that Has() confirms exist
	skills, err := List()
	if err != nil {
		t.Fatalf("List() error = %v", err)
	}
	for _, skill := range skills {
		if !Has(skill) {
			t.Errorf("List() returned %q but Has(%q) = false", skill, skill)
		}
	}
}

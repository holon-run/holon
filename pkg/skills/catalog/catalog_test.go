package catalog

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestSkillsShAdapter_Resolve(t *testing.T) {
	// Create a test server that simulates skills.sh catalog
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/skill/test-package" {
			w.Header().Set("Content-Type", "application/json")
			entry := CatalogEntry{
				Name:        "test-package",
				URL:         "https://example.com/test-package.zip",
				Description: "A test package",
				SHA256:      strings.Repeat("a", 64),
			}
			json.NewEncoder(w).Encode(entry)
		} else if r.URL.Path == "/skill/another-package" {
			w.Header().Set("Content-Type", "application/json")
			entry := CatalogEntry{
				Name:        "another-package",
				URL:         "https://example.com/another.zip",
				Description: "Another test package",
			}
			json.NewEncoder(w).Encode(entry)
		} else {
			w.WriteHeader(http.StatusNotFound)
		}
	}))
	defer server.Close()

	adapter := NewSkillsShAdapter()
	adapter.catalogURL = server.URL

	t.Run("valid package reference", func(t *testing.T) {
		entry, err := adapter.Resolve("skills:test-package")
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		if entry.Name != "test-package" {
			t.Errorf("expected name 'test-package', got '%s'", entry.Name)
		}

		if entry.URL != "https://example.com/test-package.zip" {
			t.Errorf("expected URL 'https://example.com/test-package.zip', got '%s'", entry.URL)
		}

		if entry.SHA256 != strings.Repeat("a", 64) {
			t.Errorf("expected SHA256 checksum, got '%s'", entry.SHA256)
		}
	})

	t.Run("package without checksum", func(t *testing.T) {
		entry, err := adapter.Resolve("skills:another-package")
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		if entry.Name != "another-package" {
			t.Errorf("expected name 'another-package', got '%s'", entry.Name)
		}

		if entry.SHA256 != "" {
			t.Errorf("expected empty SHA256, got '%s'", entry.SHA256)
		}
	})

	t.Run("package not found", func(t *testing.T) {
		_, err := adapter.Resolve("skills:nonexistent")
		if err == nil {
			t.Error("expected error for nonexistent package, got nil")
		}

		if !strings.Contains(err.Error(), "not found") {
			t.Errorf("error should mention 'not found': %v", err)
		}
	})

	t.Run("invalid reference format", func(t *testing.T) {
		tests := []struct {
			name string
			ref  string
		}{
			{"missing colon", "skillstest-package"},
			{"empty package name", "skills:"},
			{"wrong prefix", "pkg:test-package"},
		}

		for _, tt := range tests {
			t.Run(tt.name, func(t *testing.T) {
				_, err := adapter.Resolve(tt.ref)
				if err == nil {
					t.Error("expected error for invalid reference, got nil")
				}
			})
		}
	})
}

func TestGitHubAdapter_Resolve(t *testing.T) {
	adapter := NewGitHubAdapter()

	t.Run("valid repository reference", func(t *testing.T) {
		entry, err := adapter.Resolve("gh:owner/repo")
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		expectedURL := "https://github.com/owner/repo/archive/refs/heads/main.zip"
		if entry.URL != expectedURL {
			t.Errorf("expected URL '%s', got '%s'", expectedURL, entry.URL)
		}

		if entry.Name != "owner/repo" {
			t.Errorf("expected name 'owner/repo', got '%s'", entry.Name)
		}
	})

	t.Run("repository with path", func(t *testing.T) {
		entry, err := adapter.Resolve("gh:owner/repo/skills/custom")
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		expectedURL := "https://github.com/owner/repo/archive/refs/heads/main.zip"
		if entry.URL != expectedURL {
			t.Errorf("expected URL '%s', got '%s'", expectedURL, entry.URL)
		}

		if !strings.Contains(entry.Description, "path: skills/custom") {
			t.Errorf("description should contain path: %s", entry.Description)
		}
	})

	t.Run("invalid reference format", func(t *testing.T) {
		tests := []struct {
			name string
			ref  string
		}{
			{"missing colon", "ghowner/repo"},
			{"empty path", "gh:"},
			{"missing repo", "gh:owner"},
			{"wrong prefix", "github:owner/repo"},
		}

		for _, tt := range tests {
			t.Run(tt.name, func(t *testing.T) {
				_, err := adapter.Resolve(tt.ref)
				if err == nil {
					t.Error("expected error for invalid reference, got nil")
				}
			})
		}
	})
}

func TestBuiltinCatalogAdapter_Resolve(t *testing.T) {
	catalogJSON := []byte(`{
		"name": "builtin",
		"description": "Built-in skill catalog",
		"skills": [
			{
				"name": "test-skill",
				"url": "https://example.com/test-skill.zip",
				"description": "A test skill",
				"sha256": "abcd1234"
			},
			{
				"name": "another-skill",
				"url": "https://example.com/another-skill.zip",
				"description": "Another test skill"
			}
		]
	}`)

	adapter, err := NewBuiltinCatalogAdapter(catalogJSON)
	if err != nil {
		t.Fatalf("NewBuiltinCatalogAdapter failed: %v", err)
	}

	t.Run("existing skill", func(t *testing.T) {
		entry, err := adapter.Resolve("test-skill")
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		if entry.Name != "test-skill" {
			t.Errorf("expected name 'test-skill', got '%s'", entry.Name)
		}

		if entry.URL != "https://example.com/test-skill.zip" {
			t.Errorf("expected URL 'https://example.com/test-skill.zip', got '%s'", entry.URL)
		}
	})

	t.Run("nonexistent skill", func(t *testing.T) {
		_, err := adapter.Resolve("nonexistent")
		if err == nil {
			t.Error("expected error for nonexistent skill, got nil")
		}

		if !strings.Contains(err.Error(), "not found") {
			t.Errorf("error should mention 'not found': %v", err)
		}
	})

	t.Run("invalid catalog JSON", func(t *testing.T) {
		invalidJSON := []byte(`{invalid json}`)
		_, err := NewBuiltinCatalogAdapter(invalidJSON)
		if err == nil {
			t.Error("expected error for invalid JSON, got nil")
		}
	})
}

func TestRegistry(t *testing.T) {
	t.Run("resolve with first successful adapter", func(t *testing.T) {
		registry := NewRegistry()

		// Add adapters in order
		registry.Register(NewGitHubAdapter())
		registry.Register(&MockAdapter{shouldFail: false})

		// GitHub adapter should succeed
		entry, err := registry.Resolve("gh:test/repo")
		if err != nil {
			t.Fatalf("Resolve failed: %v", err)
		}

		if entry.Name != "test/repo" {
			t.Errorf("expected name 'test/repo', got '%s'", entry.Name)
		}
	})

	t.Run("all adapters fail", func(t *testing.T) {
		registry := NewRegistry()
		registry.Register(&MockAdapter{shouldFail: true})

		_, err := registry.Resolve("test")
		if err == nil {
			t.Error("expected error when all adapters fail, got nil")
		}
	})

	t.Run("empty registry", func(t *testing.T) {
		registry := NewRegistry()

		_, err := registry.Resolve("test")
		if err == nil {
			t.Error("expected error for empty registry, got nil")
		}
	})
}

func TestIsCatalogRef(t *testing.T) {
	tests := []struct {
		name string
		ref  string
		want bool
	}{
		{"skills: prefix", "skills:package", true},
		{"skills: with path", "skills:owner/repo", true},
		{"gh: prefix", "gh:owner/repo", true},
		{"gh: with path", "gh:owner/repo/skills/custom", true},
		{"HTTP URL", "https://example.com/skill.zip", false},
		{"local path", "/path/to/skill", false},
		{"relative path", "./skills/test", false},
		{"empty string", "", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := IsCatalogRef(tt.ref); got != tt.want {
				t.Errorf("IsCatalogRef(%q) = %v, want %v", tt.ref, got, tt.want)
			}
		})
	}
}

// MockAdapter is a mock adapter for testing
type MockAdapter struct {
	shouldFail bool
}

func (m *MockAdapter) Resolve(ref string) (*CatalogEntry, error) {
	if m.shouldFail {
		return nil, fmt.Errorf("mock adapter error")
	}
	return &CatalogEntry{
		Name: ref,
		URL:  "https://example.com/mock.zip",
	}, nil
}

func (m *MockAdapter) Name() string {
	return "mock"
}

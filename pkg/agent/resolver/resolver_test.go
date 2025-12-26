package resolver

import (
	"context"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/holon-run/holon/pkg/agent/cache"
)

func TestFileResolver(t *testing.T) {
	resolver := &FileResolver{}

	// Create a temporary test file
	tmpFile, err := os.CreateTemp("", "test-bundle-*.tar.gz")
	if err != nil {
		t.Fatalf("Failed to create temp file: %v", err)
	}
	defer os.Remove(tmpFile.Name())
	tmpFile.Close()

	tests := []struct {
		name     string
		ref      string
		can      bool
		shouldErr bool
	}{
		{
			name:      "absolute path to existing file",
			ref:       tmpFile.Name(),
			can:       true,
			shouldErr: false,
		},
		{
			name:      "relative path to existing file",
			ref:       filepath.Base(tmpFile.Name()),
			can:       false, // Will be false in test context since we're not in that dir
			shouldErr: true,
		},
		{
			name:      "non-existent file",
			ref:       "/path/to/nonexistent.tar.gz",
			can:       true, // Absolute paths are considered resolvable
			shouldErr: true,
		},
		{
			name:      "directory path",
			ref:       "/tmp",
			can:       true,
			shouldErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if resolver.CanResolve(tt.ref) != tt.can {
				t.Errorf("CanResolve() = %v, want %v", resolver.CanResolve(tt.ref), tt.can)
				return
			}

			if !tt.can {
				return
			}

			_, err := resolver.Resolve(context.Background(), tt.ref)
			if (err != nil) != tt.shouldErr {
				t.Errorf("Resolve() error = %v, shouldErr %v", err, tt.shouldErr)
			}
		})
	}
}

func TestHTTPResolver(t *testing.T) {
	cacheDir, err := os.MkdirTemp("", "holon-test-cache-*")
	if err != nil {
		t.Fatalf("Failed to create temp cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	c := cache.New(cacheDir)
	resolver := &HTTPResolver{
		cache:  c,
		client: &http.Client{Timeout: 5 * time.Second},
	}

	tests := []struct {
		name string
		ref  string
		can  bool
	}{
		{
			name: "https URL",
			ref:  "https://example.com/bundle.tar.gz",
			can:  true,
		},
		{
			name: "http URL",
			ref:  "http://example.com/bundle.tar.gz",
			can:  true,
		},
		{
			name: "file path",
			ref:  "/path/to/bundle.tar.gz",
			can:  false,
		},
		{
			name: "alias",
			ref:  "myagent",
			can:  false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if resolver.CanResolve(tt.ref) != tt.can {
				t.Errorf("CanResolve() = %v, want %v", resolver.CanResolve(tt.ref), tt.can)
			}
		})
	}
}

func TestAliasResolver(t *testing.T) {
	cacheDir, err := os.MkdirTemp("", "holon-test-cache-*")
	if err != nil {
		t.Fatalf("Failed to create temp cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	c := cache.New(cacheDir)

	// Set up a test alias
	testURL := "https://example.com/bundle.tar.gz"
	testAlias := "testagent"
	if err := c.SetAlias(testAlias, testURL); err != nil {
		t.Fatalf("Failed to set test alias: %v", err)
	}

	resolver := &AliasResolver{cache: c}

	tests := []struct {
		name string
		ref  string
		can  bool
	}{
		{
			name: "existing alias",
			ref:  testAlias,
			can:  true,
		},
		{
			name: "non-existent alias",
			ref:  "nonexistent",
			can:  false,
		},
		{
			name: "URL",
			ref:  "https://example.com/bundle.tar.gz",
			can:  false,
		},
		{
			name: "file path",
			ref:  "/path/to/bundle.tar.gz",
			can:  false,
		},
		{
			name: "alias with dot",
			ref:  "my.agent",
			can:  false,
		},
		{
			name: "alias with slash",
			ref:  "my/agent",
			can:  false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if resolver.CanResolve(tt.ref) != tt.can {
				t.Errorf("CanResolve() = %v, want %v", resolver.CanResolve(tt.ref), tt.can)
			}
		})
	}
}

func TestRegistry(t *testing.T) {
	cacheDir, err := os.MkdirTemp("", "holon-test-cache-*")
	if err != nil {
		t.Fatalf("Failed to create temp cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	registry := NewRegistry(cacheDir)

	// Create a temporary test file
	tmpFile, err := os.CreateTemp("", "test-bundle-*.tar.gz")
	if err != nil {
		t.Fatalf("Failed to create temp file: %v", err)
	}
	defer os.Remove(tmpFile.Name())
	tmpFile.Close()

	tests := []struct {
		name      string
		ref       string
		shouldErr bool
		errContains string // Optional substring to check in error message
	}{
		{
			name:      "local file",
			ref:       tmpFile.Name(),
			shouldErr: false,
		},
		{
			name:      "non-existent file",
			ref:       "/path/to/nonexistent.tar.gz",
			shouldErr: true,
		},
		{
			name:         "empty string with auto-install disabled",
			ref:          "",
			shouldErr:    true,
			errContains:  "auto-install may be disabled",
		},
		{
			name:      "unsupported protocol",
			ref:       "ftp://example.com/bundle.tar.gz",
			shouldErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Disable auto-install for consistent testing
			t.Setenv("HOLON_NO_AUTO_INSTALL", "1")

			_, err := registry.Resolve(context.Background(), tt.ref)
			if (err != nil) != tt.shouldErr {
				t.Errorf("Resolve() error = %v, shouldErr %v", err, tt.shouldErr)
				return
			}

			if tt.shouldErr && tt.errContains != "" {
				if err == nil {
					t.Errorf("Expected error containing %q, got nil", tt.errContains)
				} else if !strings.Contains(err.Error(), tt.errContains) {
					t.Errorf("Expected error containing %q, got %q", tt.errContains, err.Error())
				}
			}
		})
	}
}

func TestRegistryWithAlias(t *testing.T) {
	cacheDir, err := os.MkdirTemp("", "holon-test-cache-*")
	if err != nil {
		t.Fatalf("Failed to create temp cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	registry := NewRegistry(cacheDir)
	c := cache.New(cacheDir)

	// Set up a test alias
	testURL := "https://example.com/bundle.tar.gz"
	testAlias := "testagent"
	if err := c.SetAlias(testAlias, testURL); err != nil {
		t.Fatalf("Failed to set test alias: %v", err)
	}

	// Test alias resolution directly - verify that the alias exists and can be resolved
	aliasResolver := &AliasResolver{cache: c}

	// Verify the alias resolver can resolve the alias
	if !aliasResolver.CanResolve(testAlias) {
		t.Errorf("AliasResolver should be able to resolve alias %q", testAlias)
	}

	// Test with the full registry - we expect an error because the HTTP resolver
	// will try to download a non-existent URL, but the alias resolution part should work
	_, err = registry.Resolve(context.Background(), testAlias)
	if err == nil {
		t.Errorf("Expected registry resolve to fail due to HTTP download error")
	}

	// The error should be about downloading, not about alias resolution
	expectedErrorSubstrings := []string{"download", "HTTP", "404", "Not Found"}
	errorMsg := err.Error()
	foundExpected := false
	for _, substr := range expectedErrorSubstrings {
		if strings.Contains(errorMsg, substr) {
			foundExpected = true
			break
		}
	}
	if !foundExpected {
		t.Errorf("Expected download-related error, got: %v", err)
	}
}

func TestBuiltinResolver_CanResolve(t *testing.T) {
	resolver := &BuiltinResolver{}

	tests := []struct {
		name           string
		ref            string
		disableAutoInstall bool
		can            bool
	}{
		{
			name:           "empty string with auto-install enabled",
			ref:            "",
			disableAutoInstall: false,
			can:            true,
		},
		{
			name:           "default alias with auto-install enabled",
			ref:            "default",
			disableAutoInstall: false,
			can:            true,
		},
		{
			name:           "empty string with auto-install disabled",
			ref:            "",
			disableAutoInstall: true,
			can:            false,
		},
		{
			name:           "default alias with auto-install disabled",
			ref:            "default",
			disableAutoInstall: true,
			can:            false,
		},
		{
			name:           "whitespace-only with auto-install enabled",
			ref:            "   ",
			disableAutoInstall: false,
			can:            true,
		},
		{
			name:           "non-empty non-default ref",
			ref:            "someagent",
			disableAutoInstall: false,
			can:            false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Set HOLON_NO_AUTO_INSTALL environment variable
			if tt.disableAutoInstall {
				t.Setenv("HOLON_NO_AUTO_INSTALL", "1")
			} else {
				t.Setenv("HOLON_NO_AUTO_INSTALL", "")
			}

			if got := resolver.CanResolve(tt.ref); got != tt.can {
				t.Errorf("CanResolve() = %v, want %v", got, tt.can)
			}
		})
	}
}

func TestRegistry_EmptyRef(t *testing.T) {
	cacheDir, err := os.MkdirTemp("", "holon-test-cache-*")
	if err != nil {
		t.Fatalf("Failed to create temp cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	registry := NewRegistry(cacheDir)

	tests := []struct {
		name           string
		ref            string
		disableAutoInstall bool
		shouldErr      bool
		errContains    string
	}{
		{
			name:           "empty ref with auto-install enabled - should succeed",
			ref:            "",
			disableAutoInstall: false,
			shouldErr:      false, // Builtin resolver downloads and caches agent
			errContains:    "",
		},
		{
			name:           "empty ref with auto-install disabled",
			ref:            "",
			disableAutoInstall: true,
			shouldErr:      true,
			errContains:    "auto-install may be disabled",
		},
		{
			name:           "whitespace-only ref with auto-install disabled",
			ref:            "   ",
			disableAutoInstall: true,
			shouldErr:      true,
			errContains:    "auto-install may be disabled",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Set HOLON_NO_AUTO_INSTALL environment variable
			if tt.disableAutoInstall {
				t.Setenv("HOLON_NO_AUTO_INSTALL", "1")
			} else {
				t.Setenv("HOLON_NO_AUTO_INSTALL", "")
			}

			_, err := registry.Resolve(context.Background(), tt.ref)
			if (err != nil) != tt.shouldErr {
				t.Errorf("Resolve() error = %v, shouldErr %v", err, tt.shouldErr)
				return
			}

			if tt.shouldErr && tt.errContains != "" {
				if err == nil {
					t.Errorf("Expected error containing %q, got nil", tt.errContains)
				} else if !strings.Contains(err.Error(), tt.errContains) {
					t.Errorf("Expected error containing %q, got %q", tt.errContains, err.Error())
				}
			}
		})
	}
}
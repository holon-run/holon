package cache

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestNewCache(t *testing.T) {
	// Test with custom cache directory
	customDir := t.TempDir()
	cache1 := New(customDir)
	if cache1 == nil {
		t.Fatal("New() returned nil")
	}

	// Test with empty string (should use default)
	cache2 := New("")
	if cache2 == nil {
		t.Fatal("New() returned nil")
	}
}

func TestCacheKey(t *testing.T) {
	cache := &Cache{}

	tests := []struct {
		url      string
		checksum string
		expected string
	}{
		{
			url:      "https://example.com/bundle.tar.gz",
			checksum: "",
			expected: "https___example_com_bundle.tar.gz",
		},
		{
			url:      "https://example.com/bundle.tar.gz",
			checksum: "abcd1234ef567890",
			expected: "https___example_com_bundle.tar.gz_abcd1234",
		},
		{
			url:      "https://example.com/path/bundle.tar.gz?version=1.0",
			checksum: "1234567890abcdef",
			expected: "https___example_com_path_bundle.tar.gz_version_1.0_12345678",
		},
		{
			url:      "https://github.com/user/repo/releases/download/v1.0.0/agent-bundle-v1.0.0-linux-amd64.tar.gz",
			checksum: "deadbeefdeadbeef",
			expected: "https___github_com_user_repo_releases_download_v1.0.0_agent-bundle-v1.0.0-linux-amd64.tar.gz_deadbeef",
		},
	}

	for _, tt := range tests {
		t.Run(tt.url, func(t *testing.T) {
			key := cache.cacheKey(tt.url, tt.checksum)
			if key != tt.expected {
				t.Errorf("cacheKey() = %q, want %q", key, tt.expected)
			}
		})
	}
}

func TestValidateAliasName(t *testing.T) {
	cache := &Cache{}

	tests := []struct {
		name      string
		alias     string
		shouldErr bool
	}{
		{
			name:      "valid simple alias",
			alias:     "myagent",
			shouldErr: false,
		},
		{
			name:      "valid alias with hyphens",
			alias:     "my-agent",
			shouldErr: false,
		},
		{
			name:      "valid alias with numbers",
			alias:     "agent123",
			shouldErr: false,
		},
		{
			name:      "empty alias",
			alias:     "",
			shouldErr: true,
		},
		{
			name:      "alias with slash",
			alias:     "my/agent",
			shouldErr: true,
		},
		{
			name:      "alias with backslash",
			alias:     "my\\agent",
			shouldErr: true,
		},
		{
			name:      "alias with dot",
			alias:     "my.agent",
			shouldErr: false,
		},
		{
			name:      "very long alias",
			alias:     strings.Repeat("a", 101),
			shouldErr: true,
		},
		{
			name:      "default alias should be reserved",
			alias:     "default",
			shouldErr: true,
		},
		{
			name:      "default alias with spaces should be reserved",
			alias:     "  default  ",
			shouldErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := cache.validateAliasName(tt.alias)
			if (err != nil) != tt.shouldErr {
				t.Errorf("validateAliasName() error = %v, shouldErr %v", err, tt.shouldErr)
			}
		})
	}
}

func TestSetGetRemoveAlias(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Test setting and getting an alias
	aliasName := "testagent"
	url := "https://example.com/bundle.tar.gz"

	// Should not exist initially
	_, err := cache.GetAlias(aliasName)
	if err == nil {
		t.Error("Expected error when getting non-existent alias")
	}

	// Set alias
	if err := cache.SetAlias(aliasName, url); err != nil {
		t.Fatalf("Failed to set alias: %v", err)
	}

	// Get alias
	retrievedURL, err := cache.GetAlias(aliasName)
	if err != nil {
		t.Errorf("Failed to get alias: %v", err)
	}
	if retrievedURL != url {
		t.Errorf("GetAlias() = %q, want %q", retrievedURL, url)
	}

	// List aliases
	aliases, err := cache.ListAliases()
	if err != nil {
		t.Errorf("Failed to list aliases: %v", err)
	}
	if len(aliases) != 1 {
		t.Errorf("ListAliases() returned %d aliases, want 1", len(aliases))
	}
	if aliases[aliasName] != url {
		t.Errorf("ListAliases() returned wrong URL for alias: got %q, want %q", aliases[aliasName], url)
	}

	// Remove alias
	if err := cache.RemoveAlias(aliasName); err != nil {
		t.Errorf("Failed to remove alias: %v", err)
	}

	// Should not exist after removal
	_, err = cache.GetAlias(aliasName)
	if err == nil {
		t.Error("Expected error when getting removed alias")
	}
}

func TestStoreAndGetBundle(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	url := "https://example.com/bundle.tar.gz"
	checksum := "abcd1234ef567890"
	content := "mock bundle content"

	// Create a reader with test content
	tmpFile := filepath.Join(t.TempDir(), "test-bundle.tar.gz")
	if err := os.WriteFile(tmpFile, []byte(content), 0644); err != nil {
		t.Fatalf("Failed to create test file: %v", err)
	}

	file, err := os.Open(tmpFile)
	if err != nil {
		t.Fatalf("Failed to open test file: %v", err)
	}
	defer file.Close()

	// Store bundle
	bundlePath, err := cache.StoreBundle(url, checksum, file, int64(len(content)))
	if err != nil {
		t.Fatalf("Failed to store bundle: %v", err)
	}

	// Verify file exists
	if _, err := os.Stat(bundlePath); err != nil {
		t.Errorf("Stored bundle file does not exist: %v", err)
	}

	// Get bundle
	retrievedPath, err := cache.GetBundle(url, checksum)
	if err != nil {
		t.Errorf("Failed to get bundle: %v", err)
	}
	if retrievedPath != bundlePath {
		t.Errorf("GetBundle() returned %q, want %q", retrievedPath, bundlePath)
	}

	// Verify content
	retrievedContent, err := os.ReadFile(retrievedPath)
	if err != nil {
		t.Errorf("Failed to read retrieved bundle: %v", err)
	}
	if string(retrievedContent) != content {
		t.Errorf("Retrieved content = %q, want %q", string(retrievedContent), content)
	}
}

func TestGetBundleWithWrongChecksum(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	url := "https://example.com/bundle.tar.gz"
	checksum := "abcd1234ef567890"
	content := "mock bundle content"

	tmpFile := filepath.Join(t.TempDir(), "test-bundle.tar.gz")
	if err := os.WriteFile(tmpFile, []byte(content), 0644); err != nil {
		t.Fatalf("Failed to create test file: %v", err)
	}

	file, err := os.Open(tmpFile)
	if err != nil {
		t.Fatalf("Failed to open test file: %v", err)
	}
	defer file.Close()

	// Store bundle
	_, err = cache.StoreBundle(url, checksum, file, int64(len(content)))
	if err != nil {
		t.Fatalf("Failed to store bundle: %v", err)
	}

	// Try to get with wrong checksum
	wrongChecksum := "deadbeefdeadbeef"
	_, err = cache.GetBundle(url, wrongChecksum)
	if err == nil {
		t.Error("Expected error when getting bundle with wrong checksum")
	}
}

func TestGetNonExistentBundle(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	url := "https://example.com/nonexistent.tar.gz"
	checksum := "abcd1234ef567890"

	// Should not exist
	_, err := cache.GetBundle(url, checksum)
	if err == nil {
		t.Error("Expected error when getting non-existent bundle")
	}
}

func TestSetAndGetLatestAgentMetadata(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	metadata := &LatestAgentMetadata{
		Version:   "agent-claude-v0.3.0",
		URL:       "https://github.com/holon-run/holon/releases/download/v0.3.0/agent-bundle.tar.gz",
		Checksum:  "abcd1234ef567890",
		FetchedAt: 1704067200, // 2024-01-01 00:00:00 UTC
	}

	// Set metadata
	if err := cache.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set latest agent metadata: %v", err)
	}

	// Get metadata
	retrieved, err := cache.GetLatestAgentMetadata()
	if err != nil {
		t.Fatalf("Failed to get latest agent metadata: %v", err)
	}
	if retrieved == nil {
		t.Fatal("GetLatestAgentMetadata() returned nil")
	}

	// Verify fields
	if retrieved.Version != metadata.Version {
		t.Errorf("Version = %q, want %q", retrieved.Version, metadata.Version)
	}
	if retrieved.URL != metadata.URL {
		t.Errorf("URL = %q, want %q", retrieved.URL, metadata.URL)
	}
	if retrieved.Checksum != metadata.Checksum {
		t.Errorf("Checksum = %q, want %q", retrieved.Checksum, metadata.Checksum)
	}
	if retrieved.FetchedAt != metadata.FetchedAt {
		t.Errorf("FetchedAt = %d, want %d", retrieved.FetchedAt, metadata.FetchedAt)
	}
}

func TestGetLatestAgentMetadataWhenMissing(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Should return nil, not error, when file doesn't exist
	metadata, err := cache.GetLatestAgentMetadata()
	if err != nil {
		t.Fatalf("GetLatestAgentMetadata() returned error: %v", err)
	}
	if metadata != nil {
		t.Errorf("GetLatestAgentMetadata() = %v, want nil", metadata)
	}
}

func TestSetLatestAgentMetadataOverwrites(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Set initial metadata
	metadata1 := &LatestAgentMetadata{
		Version:   "agent-claude-v0.1.0",
		URL:       "https://example.com/v0.1.0/bundle.tar.gz",
		Checksum:  "aaa111",
		FetchedAt: 1704067200,
	}
	if err := cache.SetLatestAgentMetadata(metadata1); err != nil {
		t.Fatalf("Failed to set initial metadata: %v", err)
	}

	// Overwrite with new metadata
	metadata2 := &LatestAgentMetadata{
		Version:   "agent-claude-v0.2.0",
		URL:       "https://example.com/v0.2.0/bundle.tar.gz",
		Checksum:  "bbb222",
		FetchedAt: 1704153600,
	}
	if err := cache.SetLatestAgentMetadata(metadata2); err != nil {
		t.Fatalf("Failed to overwrite metadata: %v", err)
	}

	// Verify we get the new metadata
	retrieved, err := cache.GetLatestAgentMetadata()
	if err != nil {
		t.Fatalf("Failed to get metadata: %v", err)
	}
	if retrieved.Version != "agent-claude-v0.2.0" {
		t.Errorf("Version = %q, want %q", retrieved.Version, "agent-claude-v0.2.0")
	}
	if retrieved.Checksum != "bbb222" {
		t.Errorf("Checksum = %q, want %q", retrieved.Checksum, "bbb222")
	}
}

func TestGetLatestAgentMetadataWithCorruptFile(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Write corrupt JSON to the metadata file
	metadataPath := filepath.Join(cacheDir, "latest-agent.json")
	if err := os.WriteFile(metadataPath, []byte("{invalid json"), 0644); err != nil {
		t.Fatalf("Failed to write corrupt metadata: %v", err)
	}

	// Should return an error
	_, err := cache.GetLatestAgentMetadata()
	if err == nil {
		t.Error("Expected error when getting corrupt metadata, got nil")
	}
}

func TestIsLatestAgentStaleWithMissingMetadata(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// No metadata file exists
	if !cache.IsLatestAgentStale() {
		t.Error("IsLatestAgentStale() = false, want true (missing metadata)")
	}
}

func TestIsLatestAgentStaleWithEmptyVersion(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Set metadata with empty version
	metadata := &LatestAgentMetadata{
		Version:   "",
		URL:       "https://example.com/bundle.tar.gz",
		Checksum:  "abcd1234",
		FetchedAt: time.Now().Unix(), // Fresh timestamp
	}
	if err := cache.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set metadata: %v", err)
	}

	if !cache.IsLatestAgentStale() {
		t.Error("IsLatestAgentStale() = false, want true (empty version)")
	}
}

func TestIsLatestAgentStaleWithOldMetadata(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Set metadata with old timestamp (25 hours ago)
	oldTimestamp := time.Now().Add(-25 * time.Hour).Unix()
	metadata := &LatestAgentMetadata{
		Version:   "agent-claude-v0.3.0",
		URL:       "https://example.com/bundle.tar.gz",
		Checksum:  "abcd1234",
		FetchedAt: oldTimestamp,
	}
	if err := cache.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set metadata: %v", err)
	}

	if !cache.IsLatestAgentStale() {
		t.Error("IsLatestAgentStale() = false, want true (old metadata)")
	}
}

func TestIsLatestAgentStaleWithFreshMetadata(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Set metadata with recent timestamp (1 hour ago)
	recentTimestamp := time.Now().Add(-1 * time.Hour).Unix()
	metadata := &LatestAgentMetadata{
		Version:   "agent-claude-v0.3.0",
		URL:       "https://example.com/bundle.tar.gz",
		Checksum:  "abcd1234",
		FetchedAt: recentTimestamp,
	}
	if err := cache.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set metadata: %v", err)
	}

	if cache.IsLatestAgentStale() {
		t.Error("IsLatestAgentStale() = true, want false (fresh metadata)")
	}
}

func TestIsLatestAgentStaleAtExactly24Hours(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Set metadata with timestamp safely under 24 hours ago
	// Using 23 hours 59 minutes to avoid race conditions with time.Now() calls
	timestamp := time.Now().Add(-23*time.Hour - 59*time.Minute).Unix()
	metadata := &LatestAgentMetadata{
		Version:   "agent-claude-v0.3.0",
		URL:       "https://example.com/bundle.tar.gz",
		Checksum:  "abcd1234",
		FetchedAt: timestamp,
	}
	if err := cache.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set metadata: %v", err)
	}

	if cache.IsLatestAgentStale() {
		t.Error("IsLatestAgentStale() = true, want false (just under 24 hours old)")
	}
}

func TestIsLatestAgentStaleAt24HoursAndOneSecond(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Set metadata with timestamp just over 24 hours ago
	// This SHOULD be considered stale (> 24 hours)
	timestamp := time.Now().Add(-24*time.Hour - time.Second).Unix()
	metadata := &LatestAgentMetadata{
		Version:   "agent-claude-v0.3.0",
		URL:       "https://example.com/bundle.tar.gz",
		Checksum:  "abcd1234",
		FetchedAt: timestamp,
	}
	if err := cache.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set metadata: %v", err)
	}

	if !cache.IsLatestAgentStale() {
		t.Error("IsLatestAgentStale() = false, want true (> 24 hours old)")
	}
}

func TestIsLatestAgentStaleWithFutureTimestamp(t *testing.T) {
	cacheDir := t.TempDir()
	cache := New(cacheDir)

	// Set metadata with future timestamp (1 hour in the future)
	futureTimestamp := time.Now().Add(1 * time.Hour).Unix()
	metadata := &LatestAgentMetadata{
		Version:   "agent-claude-v0.3.0",
		URL:       "https://example.com/bundle.tar.gz",
		Checksum:  "abcd1234",
		FetchedAt: futureTimestamp,
	}
	if err := cache.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set metadata: %v", err)
	}

	// Future timestamps should NOT be considered stale
	if cache.IsLatestAgentStale() {
		t.Error("IsLatestAgentStale() = true, want false (future timestamp)")
	}
}

func TestLatestAgentMetadataPersistence(t *testing.T) {
	cacheDir := t.TempDir()

	// Create first cache instance and set metadata
	cache1 := New(cacheDir)
	metadata := &LatestAgentMetadata{
		Version:   "agent-claude-v0.5.0",
		URL:       "https://example.com/bundle.tar.gz",
		Checksum:  "xyz789",
		FetchedAt: 1704067200,
	}
	if err := cache1.SetLatestAgentMetadata(metadata); err != nil {
		t.Fatalf("Failed to set metadata: %v", err)
	}

	// Create second cache instance pointing to same directory
	// This simulates restarting the application
	cache2 := New(cacheDir)

	// Should retrieve the same metadata
	retrieved, err := cache2.GetLatestAgentMetadata()
	if err != nil {
		t.Fatalf("Failed to get metadata from new cache instance: %v", err)
	}
	if retrieved.Version != "agent-claude-v0.5.0" {
		t.Errorf("Version = %q, want %q", retrieved.Version, "agent-claude-v0.5.0")
	}
}
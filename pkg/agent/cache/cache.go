package cache

import (
	"encoding/json"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

// Cache manages agent bundle storage and aliases
type Cache struct {
	mu       sync.RWMutex
	cacheDir string
}

// New creates a new cache instance
func New(cacheDir string) *Cache {
	if cacheDir == "" {
		homeDir, err := os.UserHomeDir()
		if err != nil {
			cacheDir = "/tmp/.holon-cache"
		} else {
			cacheDir = filepath.Join(homeDir, ".holon", "cache")
		}
	}

	cache := &Cache{cacheDir: cacheDir}
	cache.initialize()
	return cache
}

// initialize sets up the cache directory structure
func (c *Cache) initialize() {
	c.mu.Lock()
	defer c.mu.Unlock()

	// Create cache directories
	dirs := []string{
		c.cacheDir,
		filepath.Join(c.cacheDir, "bundles"),
		filepath.Join(c.cacheDir, "aliases"),
	}

	for _, dir := range dirs {
		if err := os.MkdirAll(dir, 0755); err != nil {
			// Continue even if we can't create directories
			// The cache will be non-functional but won't crash the application
			continue
		}
	}
}

// StoreBundle stores a downloaded bundle in the cache
func (c *Cache) StoreBundle(url, checksum string, reader io.Reader, size int64) (string, error) {
	return c.StoreBundleWithVersion(url, checksum, reader, size, "")
}

// StoreBundleWithVersion stores a downloaded bundle in the cache with version info
func (c *Cache) StoreBundleWithVersion(url, checksum string, reader io.Reader, size int64, version string) (string, error) {
	c.mu.Lock()
	defer c.mu.Unlock()

	bundleDir := filepath.Join(c.cacheDir, "bundles", c.cacheKey(url, checksum))
	if err := os.MkdirAll(bundleDir, 0755); err != nil {
		return "", fmt.Errorf("failed to create bundle directory: %w", err)
	}

	bundlePath := filepath.Join(bundleDir, "bundle.tar.gz")
	file, err := os.OpenFile(bundlePath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0644)
	if err != nil {
		return "", fmt.Errorf("failed to create bundle file: %w", err)
	}
	defer file.Close()

	written, err := io.Copy(file, reader)
	if err != nil {
		if removeErr := os.RemoveAll(bundleDir); removeErr != nil {
			// Log the cleanup error but don't mask the original error
			holonlog.Warn("failed to cleanup bundle directory", "error", removeErr)
		}
		return "", fmt.Errorf("failed to write bundle file: %w", err)
	}

	// Verify that we wrote all the data
	if written != size {
		if removeErr := os.RemoveAll(bundleDir); removeErr != nil {
			// Log the cleanup error but don't mask the original error
			holonlog.Warn("failed to cleanup bundle directory", "error", removeErr)
		}
		return "", fmt.Errorf("incomplete write: expected %d bytes, wrote %d", size, written)
	}

	// Store metadata with version if provided
	metadata := BundleMetadata{
		URL:       url,
		Version:   version,
		Checksum:  checksum,
		Size:      size,
		CachedAt:  time.Now().Unix(),
		FetchedAt: time.Now().Unix(),
	}

	metadataPath := filepath.Join(bundleDir, "metadata.json")
	if err := c.writeMetadata(metadataPath, metadata); err != nil {
		if removeErr := os.RemoveAll(bundleDir); removeErr != nil {
			// Log the cleanup error but don't mask the original error
			fmt.Printf("Warning: failed to cleanup bundle directory: %v\n", removeErr)
		}
		return "", fmt.Errorf("failed to write metadata: %w", err)
	}

	return bundlePath, nil
}

// GetBundle retrieves a cached bundle
func (c *Cache) GetBundle(url, checksum string) (string, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()

	bundleDir := filepath.Join(c.cacheDir, "bundles", c.cacheKey(url, checksum))
	bundlePath := filepath.Join(bundleDir, "bundle.tar.gz")

	// Check if bundle exists
	if _, err := os.Stat(bundlePath); err != nil {
		return "", fmt.Errorf("bundle not found in cache")
	}

	// Verify checksum if provided
	if checksum != "" {
		metadataPath := filepath.Join(bundleDir, "metadata.json")
		metadata, err := c.readMetadata(metadataPath)
		if err != nil {
			return "", fmt.Errorf("failed to read metadata: %w", err)
		}

		if metadata.Checksum != checksum {
			return "", fmt.Errorf("checksum mismatch")
		}
	}

	return bundlePath, nil
}

// SetAlias stores an alias for a URL
func (c *Cache) SetAlias(name, url string) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if err := c.validateAliasName(name); err != nil {
		return err
	}

	aliasesPath := filepath.Join(c.cacheDir, "aliases", "aliases.json")
	aliases, err := c.loadAliases(aliasesPath)
	if err != nil {
		aliases = make(map[string]string)
	}

	aliases[name] = url

	return c.writeAliases(aliasesPath, aliases)
}

// GetAlias retrieves a URL by alias name
func (c *Cache) GetAlias(name string) (string, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()

	if err := c.validateAliasName(name); err != nil {
		return "", err
	}

	aliasesPath := filepath.Join(c.cacheDir, "aliases", "aliases.json")
	aliases, err := c.loadAliases(aliasesPath)
	if err != nil {
		return "", fmt.Errorf("failed to load aliases: %w", err)
	}

	url, exists := aliases[name]
	if !exists {
		return "", fmt.Errorf("alias not found: %s", name)
	}

	return url, nil
}

// RemoveAlias removes an alias
func (c *Cache) RemoveAlias(name string) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if err := c.validateAliasName(name); err != nil {
		return err
	}

	aliasesPath := filepath.Join(c.cacheDir, "aliases", "aliases.json")
	aliases, err := c.loadAliases(aliasesPath)
	if err != nil {
		return fmt.Errorf("failed to load aliases: %w", err)
	}

	if _, exists := aliases[name]; !exists {
		return fmt.Errorf("alias not found: %s", name)
	}

	delete(aliases, name)
	return c.writeAliases(aliasesPath, aliases)
}

// ListAliases returns all configured aliases
func (c *Cache) ListAliases() (map[string]string, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()

	aliasesPath := filepath.Join(c.cacheDir, "aliases", "aliases.json")
	return c.loadAliases(aliasesPath)
}

// cacheKey generates a deterministic cache key for a URL and checksum
func (c *Cache) cacheKey(rawURL, checksum string) string {
	// Parse URL to handle hostname differently from path
	parsedURL, err := url.Parse(rawURL)
	if err != nil {
		// If URL parsing fails, fall back to simple replacement
		safeURL := strings.NewReplacer(
			":", "_",
			"/", "_",
			"?", "_",
			"#", "_",
			"&", "_",
			"=", "_",
			".", "_",
		).Replace(rawURL)

		if checksum != "" {
			n := len(checksum)
			if n > 8 {
				n = 8
			}
			safeURL += "_" + checksum[:n]
		}
		return safeURL
	}

	// Replace dots in hostname but keep them in path
	safeHost := strings.ReplaceAll(parsedURL.Host, ".", "_")

	// Build the key by replacing special characters in the rest
	queryAndFragment := ""
	if parsedURL.RawQuery != "" {
		queryAndFragment += "?" + parsedURL.RawQuery
	}
	if parsedURL.Fragment != "" {
		queryAndFragment += "#" + parsedURL.Fragment
	}

	safePath := strings.NewReplacer(
		"/", "_",
		"?", "_",
		"&", "_",
		"=", "_",
	).Replace(parsedURL.Path + queryAndFragment)

	safeURL := strings.ReplaceAll(parsedURL.Scheme, ":", "_") + "___" + safeHost + safePath

	if checksum != "" {
		// Use up to first 8 chars of checksum to prevent panic if checksum is too short
		n := len(checksum)
		if n > 8 {
			n = 8
		}
		safeURL += "_" + checksum[:n]
	}

	return safeURL
}

// validateAliasName ensures alias names are safe
func (c *Cache) validateAliasName(name string) error {
	if name == "" {
		return fmt.Errorf("alias name cannot be empty")
	}

	// Reserve "default" for the builtin agent
	if strings.TrimSpace(name) == "default" {
		return fmt.Errorf("alias name 'default' is reserved for the builtin agent")
	}

	if strings.Contains(name, "/") || strings.Contains(name, "\\") {
		return fmt.Errorf("alias name contains invalid characters")
	}

	if len(name) > 100 {
		return fmt.Errorf("alias name too long (max 100 characters)")
	}

	return nil
}

// BundleMetadata stores information about a cached bundle
type BundleMetadata struct {
	URL       string `json:"url"`
	Version   string `json:"version,omitempty"`   // Version tag (e.g., "agent-claude-v0.3.0")
	Checksum  string `json:"checksum"`
	Size      int64  `json:"size"`
	CachedAt  int64  `json:"cached_at"`
	FetchedAt int64  `json:"fetched_at,omitempty"` // When this version was fetched (for staleness check)
}

func (c *Cache) writeMetadata(path string, metadata BundleMetadata) error {
	data, err := json.MarshalIndent(metadata, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(path, data, 0644)
}

func (c *Cache) readMetadata(path string) (BundleMetadata, error) {
	var metadata BundleMetadata
	data, err := os.ReadFile(path)
	if err != nil {
		return metadata, err
	}

	if err := json.Unmarshal(data, &metadata); err != nil {
		return metadata, fmt.Errorf("failed to unmarshal metadata: %w", err)
	}

	return metadata, nil
}

func (c *Cache) writeAliases(path string, aliases map[string]string) error {
	data, err := json.MarshalIndent(aliases, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(path, data, 0644)
}

func (c *Cache) loadAliases(path string) (map[string]string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return make(map[string]string), nil
		}
		return nil, err
	}

	var aliases map[string]string
	err = json.Unmarshal(data, &aliases)
	if err != nil {
		return nil, err
	}

	if aliases == nil {
		aliases = make(map[string]string)
	}

	return aliases, nil
}

// LatestAgentMetadata stores metadata about the latest known agent version
type LatestAgentMetadata struct {
	Version   string `json:"version"`   // Version tag (e.g., "agent-claude-v0.3.0")
	URL       string `json:"url"`       // Download URL
	Checksum  string `json:"checksum"`  // SHA256 checksum
	FetchedAt int64  `json:"fetched_at"` // When this was fetched (for staleness check)
}

// GetLatestAgentMetadata retrieves the cached latest agent metadata
func (c *Cache) GetLatestAgentMetadata() (*LatestAgentMetadata, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()

	metadataPath := filepath.Join(c.cacheDir, "latest-agent.json")
	data, err := os.ReadFile(metadataPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil // No cached metadata
		}
		return nil, fmt.Errorf("failed to read latest agent metadata: %w", err)
	}

	var metadata LatestAgentMetadata
	if err := json.Unmarshal(data, &metadata); err != nil {
		return nil, fmt.Errorf("failed to unmarshal latest agent metadata: %w", err)
	}

	return &metadata, nil
}

// SetLatestAgentMetadata stores the latest agent metadata
func (c *Cache) SetLatestAgentMetadata(metadata *LatestAgentMetadata) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	metadataPath := filepath.Join(c.cacheDir, "latest-agent.json")
	data, err := json.MarshalIndent(metadata, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal latest agent metadata: %w", err)
	}

	if err := os.WriteFile(metadataPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write latest agent metadata: %w", err)
	}

	return nil
}

// IsLatestAgentStale checks if the cached latest agent metadata is stale
// Returns true if metadata is missing, older than 24 hours, or has empty version
func (c *Cache) IsLatestAgentStale() bool {
	metadata, err := c.GetLatestAgentMetadata()
	if err != nil || metadata == nil {
		return true // No metadata or error means stale
	}

	// Check if version is empty
	if metadata.Version == "" {
		return true
	}

	// Check if older than 24 hours
	const stalenessThreshold = 24 * time.Hour
	if time.Now().Unix()-metadata.FetchedAt > int64(stalenessThreshold.Seconds()) {
		return true
	}

	return false
}
// Package remote provides remote skill downloading and caching functionality.
// It supports downloading zip files from HTTP/HTTPS URLs, extracting them safely,
// and discovering skills within the extracted contents.
package remote

import (
	"archive/zip"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

const (
	// MaxZipSize is the maximum size of a downloadable zip file (100MB)
	MaxZipSize = 100 * 1024 * 1024
	// DownloadTimeout is the timeout for downloading a zip file
	DownloadTimeout = 5 * time.Minute
)

// Cache manages remote skill storage
type Cache struct {
	mu       sync.RWMutex
	cacheDir string
}

// NewCache creates a new remote skills cache
func NewCache(cacheDir string) *Cache {
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

	dirs := []string{
		filepath.Join(c.cacheDir, "skills"),
	}

	for _, dir := range dirs {
		if err := os.MkdirAll(dir, 0755); err != nil {
			// Continue even if we can't create directories
			holonlog.Warn("failed to create cache directory", "dir", dir, "error", err)
		}
	}
}

// SkillRef represents a remote skill reference with URL and optional checksum
type SkillRef struct {
	URL      string
	Checksum string // Optional SHA256 checksum
}

// ParseSkillRef parses a skill reference that may be a URL with optional checksum
func ParseSkillRef(ref string) (*SkillRef, error) {
	// Check if it's an HTTP/HTTPS URL
	if !strings.HasPrefix(ref, "http://") && !strings.HasPrefix(ref, "https://") {
		return nil, fmt.Errorf("not a valid URL: %s", ref)
	}

	// Parse URL to extract fragment
	parsedURL, err := url.Parse(ref)
	if err != nil {
		return nil, fmt.Errorf("failed to parse URL: %w", err)
	}

	skillRef := &SkillRef{}

	// Reconstruct URL without fragment (keep scheme, host, path, and query)
	baseURL := url.URL{
		Scheme:   parsedURL.Scheme,
		Host:     parsedURL.Host,
		Path:     parsedURL.Path,
		RawQuery: parsedURL.RawQuery,
	}
	skillRef.URL = baseURL.String()

	// Check for sha256 in fragment
	if parsedURL.Fragment != "" {
		if strings.HasPrefix(parsedURL.Fragment, "sha256=") {
			checksum := strings.TrimPrefix(parsedURL.Fragment, "sha256=")
			if err := validateSHA256(checksum); err != nil {
				return nil, fmt.Errorf("invalid sha256 checksum: %w", err)
			}
			skillRef.Checksum = checksum
		}
		// Unknown fragments are ignored (only sha256 is supported)
	}

	return skillRef, nil
}

// IsURL returns true if the ref appears to be a URL
func IsURL(ref string) bool {
	return strings.HasPrefix(ref, "http://") || strings.HasPrefix(ref, "https://")
}

// DownloadAndExtract downloads and extracts a remote skill zip
func (c *Cache) DownloadAndExtract(ref *SkillRef) ([]string, error) {
	// Check cache first
	cacheDir, err := c.getCachedDir(ref)
	if err == nil {
		// Cache hit, discover skills in cached directory
		holonlog.Debug("using cached remote skills", "url", ref.URL)
		return DiscoverSkills(cacheDir)
	}

	// Cache miss, download
	holonlog.Info("downloading remote skills", "url", ref.URL)

	// Download zip file
	zipData, err := c.download(ref.URL)
	if err != nil {
		return nil, fmt.Errorf("failed to download: %w", err)
	}

	// Verify checksum if provided
	if ref.Checksum != "" {
		if err := c.verifyChecksum(zipData, ref.Checksum); err != nil {
			return nil, fmt.Errorf("checksum verification failed: %w", err)
		}
		holonlog.Info("verified checksum", "sha256", ref.Checksum)
	}

	// Extract to temporary directory
	tempDir, err := c.extract(zipData)
	if err != nil {
		return nil, fmt.Errorf("failed to extract: %w", err)
	}

	// Discover skills in extracted directory
	skillPaths, err := DiscoverSkills(tempDir)
	if err != nil {
		os.RemoveAll(tempDir)
		return nil, fmt.Errorf("failed to discover skills: %w", err)
	}

	if len(skillPaths) == 0 {
		os.RemoveAll(tempDir)
		return nil, fmt.Errorf("no skills found in zip archive (missing SKILL.md files)")
	}

	// Move to cache
	cacheDir = c.cachePath(ref)
	if err := os.MkdirAll(filepath.Dir(cacheDir), 0755); err != nil {
		os.RemoveAll(tempDir)
		return nil, fmt.Errorf("failed to create cache directory: %w", err)
	}

	if err := os.Rename(tempDir, cacheDir); err != nil {
		os.RemoveAll(tempDir)
		return nil, fmt.Errorf("failed to move to cache: %w", err)
	}

	// Write metadata
	if err := c.writeMetadata(cacheDir, ref); err != nil {
		holonlog.Warn("failed to write cache metadata", "error", err)
	}

	holonlog.Info("cached remote skills", "url", ref.URL, "count", len(skillPaths))

	// Return paths from cache
	return DiscoverSkills(cacheDir)
}

// getCachedDir returns the cached directory if it exists and is valid
func (c *Cache) getCachedDir(ref *SkillRef) (string, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()

	cacheDir := c.cachePath(ref)

	// Check if directory exists
	if _, err := os.Stat(cacheDir); os.IsNotExist(err) {
		return "", fmt.Errorf("not cached")
	}

	// Verify metadata if checksum was provided
	if ref.Checksum != "" {
		metadataPath := filepath.Join(cacheDir, "metadata.json")
		metadata, err := c.readMetadata(metadataPath)
		if err != nil {
			return "", fmt.Errorf("metadata error: %w", err)
		}

		if metadata.Checksum != ref.Checksum {
			return "", fmt.Errorf("checksum mismatch")
		}
	}

	return cacheDir, nil
}

// cachePath returns the cache directory path for a skill ref
func (c *Cache) cachePath(ref *SkillRef) string {
	key := c.cacheKey(ref.URL, ref.Checksum)
	return filepath.Join(c.cacheDir, "skills", key)
}

// cacheKey generates a deterministic cache key for a URL and checksum
func (c *Cache) cacheKey(rawURL, checksum string) string {
	// Use similar logic to agent cache
	parsedURL, err := url.Parse(rawURL)
	if err != nil {
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

	safeHost := strings.ReplaceAll(parsedURL.Host, ".", "_")
	safePath := strings.NewReplacer(
		"/", "_",
		"?", "_",
		"&", "_",
		"=", "_",
	).Replace(parsedURL.Path)

	safeURL := strings.ReplaceAll(parsedURL.Scheme, ":", "_") + "___" + safeHost + safePath

	if checksum != "" {
		n := len(checksum)
		if n > 8 {
			n = 8
		}
		safeURL += "_" + checksum[:n]
	}

	return safeURL
}

// download downloads a zip file from the given URL
func (c *Cache) download(url string) ([]byte, error) {
	client := &http.Client{
		Timeout: DownloadTimeout,
	}

	resp, err := client.Get(url)
	if err != nil {
		return nil, fmt.Errorf("HTTP request failed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d: %s", resp.StatusCode, resp.Status)
	}

	// Check content length
	if resp.ContentLength > 0 && resp.ContentLength > MaxZipSize {
		return nil, fmt.Errorf("zip file too large: %d bytes (max %d)", resp.ContentLength, MaxZipSize)
	}

	// Limit reader to prevent memory issues
	limitedReader := io.LimitReader(resp.Body, MaxZipSize+1)

	data, err := io.ReadAll(limitedReader)
	if err != nil {
		return nil, fmt.Errorf("failed to read response body: %w", err)
	}

	// Check if we hit the limit
	if int64(len(data)) > MaxZipSize {
		return nil, fmt.Errorf("zip file exceeds maximum size of %d bytes", MaxZipSize)
	}

	return data, nil
}

// verifyChecksum verifies the SHA256 checksum of the data
func (c *Cache) verifyChecksum(data []byte, expectedChecksum string) error {
	hash := sha256.Sum256(data)
	actualChecksum := hex.EncodeToString(hash[:])

	if actualChecksum != expectedChecksum {
		return fmt.Errorf("checksum mismatch: expected %s, got %s", expectedChecksum, actualChecksum)
	}

	return nil
}

// extract extracts a zip file to a temporary directory with zip-slip protection
func (c *Cache) extract(zipData []byte) (string, error) {
	// Create temporary directory
	tempDir, err := os.MkdirTemp("", "holon-skills-extract-*")
	if err != nil {
		return "", fmt.Errorf("failed to create temp directory: %w", err)
	}

	// Open zip archive using bytes.NewReader which implements io.ReaderAt
	reader, err := zip.NewReader(bytes.NewReader(zipData), int64(len(zipData)))
	if err != nil {
		os.RemoveAll(tempDir)
		return "", fmt.Errorf("failed to open zip: %w", err)
	}

	// Extract each file
	for _, file := range reader.File {
		if err := c.extractFile(file, tempDir); err != nil {
			os.RemoveAll(tempDir)
			return "", fmt.Errorf("failed to extract %s: %w", file.Name, err)
		}
	}

	return tempDir, nil
}

// extractFile extracts a single file from the zip with safety checks
func (c *Cache) extractFile(file *zip.File, destDir string) error {
	// Prevent zip-slip vulnerability by sanitizing the file path
	joinedPath := filepath.Join(destDir, file.Name)
	cleanDest := filepath.Clean(destDir)
	cleanPath := filepath.Clean(joinedPath)

	// Validate that the sanitized path is within the destination directory
	if cleanPath != cleanDest && !strings.HasPrefix(cleanPath, cleanDest+string(os.PathSeparator)) {
		return fmt.Errorf("invalid file path (zip-slip): %s", file.Name)
	}

	// Use the sanitized path for all operations
	safePath := cleanPath

	// Create directory if needed
	if file.FileInfo().IsDir() {
		return os.MkdirAll(safePath, file.Mode())
	}

	// Create parent directory
	if err := os.MkdirAll(filepath.Dir(safePath), 0755); err != nil {
		return err
	}

	// Extract file
	fileReader, err := file.Open()
	if err != nil {
		return err
	}
	defer fileReader.Close()

	destFile, err := os.OpenFile(safePath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, file.Mode())
	if err != nil {
		return err
	}
	defer destFile.Close()

	_, err = io.Copy(destFile, fileReader)
	closeErr := destFile.Close()
	if err != nil {
		if closeErr != nil {
			return fmt.Errorf("failed to copy file data: %v; also failed to close file: %v", err, closeErr)
		}
		return err
	}
	if closeErr != nil {
		return fmt.Errorf("failed to close file after writing: %w", closeErr)
	}
	return nil
}

// DiscoverSkills discovers all skill directories in a directory tree
// A skill directory is one that contains a SKILL.md file
func DiscoverSkills(rootDir string) ([]string, error) {
	var skillPaths []string

	err := filepath.Walk(rootDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}

		// Check if this is a SKILL.md file
		if info.Name() == "SKILL.md" && !info.IsDir() {
			// Get parent directory
			skillDir := filepath.Dir(path)

			// Validate it's actually a directory
			if dirInfo, err := os.Stat(skillDir); err == nil && dirInfo.IsDir() {
				skillPaths = append(skillPaths, skillDir)
			}
		}

		return nil
	})

	if err != nil {
		return nil, fmt.Errorf("failed to walk directory: %w", err)
	}

	return skillPaths, nil
}

// Metadata stores information about cached remote skills
type Metadata struct {
	URL       string `json:"url"`
	Checksum  string `json:"checksum,omitempty"`
	CachedAt  int64  `json:"cached_at"`
	SkillCount int   `json:"skill_count"`
}

func (c *Cache) writeMetadata(cacheDir string, ref *SkillRef) error {
	// Count skills
	skills, err := DiscoverSkills(cacheDir)
	if err != nil {
		return err
	}

	metadata := Metadata{
		URL:       ref.URL,
		Checksum:  ref.Checksum,
		CachedAt:  time.Now().Unix(),
		SkillCount: len(skills),
	}

	metadataPath := filepath.Join(cacheDir, "metadata.json")
	data, err := json.MarshalIndent(metadata, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(metadataPath, data, 0644)
}

func (c *Cache) readMetadata(metadataPath string) (Metadata, error) {
	var metadata Metadata
	data, err := os.ReadFile(metadataPath)
	if err != nil {
		return metadata, err
	}

	if err := json.Unmarshal(data, &metadata); err != nil {
		return metadata, fmt.Errorf("failed to unmarshal metadata: %w", err)
	}

	return metadata, nil
}

// validateSHA256 validates that a string is a valid SHA256 hex string
func validateSHA256(s string) error {
	// SHA256 is 64 hex characters
	if len(s) != 64 {
		return fmt.Errorf("invalid length: expected 64 characters, got %d", len(s))
	}

	// Check that all characters are valid hex
	_, err := hex.DecodeString(s)
	if err != nil {
		return fmt.Errorf("invalid hex encoding: %w", err)
	}

	return nil
}

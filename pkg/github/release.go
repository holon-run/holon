package github

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

const (
	// HolonRepoOwner is the GitHub repository owner for Holon
	HolonRepoOwner = "holon-run"
	// HolonRepoName is the GitHub repository name for Holon
	HolonRepoName = "holon"
	// VersionCheckCacheFile is the filename for the version check cache
	VersionCheckCacheFile = "version_check_cache.json"
	// VersionCheckCacheTTL is the time-to-live for the version check cache (24 hours)
	VersionCheckCacheTTL = 24 * time.Hour
	// VersionCheckEnvVar is the environment variable to disable version checking
	VersionCheckEnvVar = "HOLON_NO_VERSION_CHECK"
)

// ReleaseInfo represents information about a GitHub release
type ReleaseInfo struct {
	TagName     string    `json:"tag_name"`
	Name        string    `json:"name"`
	Draft       bool      `json:"draft"`
	Prerelease  bool      `json:"prerelease"`
	PublishedAt time.Time `json:"published_at"`
	HTMLURL     string    `json:"html_url"`
}

// versionCacheData represents the cached version check data
type versionCacheData struct {
	LatestVersion  string    `json:"latest_version"`
	CacheTime      time.Time `json:"cache_time"`
	ReleaseInfo    *ReleaseInfo `json:"release_info,omitempty"`
}

// FetchLatestRelease fetches the latest release from GitHub
// Uses direct HTTP to avoid go-github dependency for this simple operation
func (c *Client) FetchLatestRelease(ctx context.Context, owner, repo string) (*ReleaseInfo, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/releases/latest", c.baseURL, owner, repo)

	req, err := c.NewRequest(ctx, "GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	var release ReleaseInfo
	resp, err := c.Do(req, &release)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch latest release: %w", err)
	}
	defer resp.Close()

	return &release, nil
}

// FetchLatestHolonRelease fetches the latest Holon CLI release from GitHub
// This filters out draft/prerelease releases and returns only releases starting with 'v'
func (c *Client) FetchLatestHolonRelease(ctx context.Context, owner, repo string) (*ReleaseInfo, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/releases", c.baseURL, owner, repo)

	req, err := c.NewRequest(ctx, "GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	var releases []ReleaseInfo
	resp, err := c.Do(req, &releases)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch releases: %w", err)
	}
	defer resp.Close()

	// Find the latest release that starts with 'v' and is not a draft/prerelease
	for _, r := range releases {
		// Skip drafts and prereleases
		if r.Draft || r.Prerelease {
			continue
		}
		// Filter for Holon CLI releases (vX.Y.Z format)
		if strings.HasPrefix(r.TagName, "v") && !strings.HasPrefix(r.TagName, "agent-") {
			return &r, nil
		}
	}

	return nil, fmt.Errorf("no Holon CLI release found")
}

// CheckForUpdates checks if a newer version of Holon is available
// Returns the latest version info and whether the current version is up to date
func CheckForUpdates(ctx context.Context, currentVersion string) (*ReleaseInfo, bool, error) {
	// Check if version check is disabled via environment variable
	if os.Getenv(VersionCheckEnvVar) != "" {
		return nil, false, fmt.Errorf("version check disabled via %s", VersionCheckEnvVar)
	}

	// Try to get from cache first
	cacheDir, err := os.UserCacheDir()
	if err == nil {
		cachePath := filepath.Join(cacheDir, "holon", VersionCheckCacheFile)
		cached, err := readVersionCache(cachePath)
		if err == nil && time.Since(cached.CacheTime) < VersionCheckCacheTTL {
			// Cache is valid
			if cached.ReleaseInfo != nil {
				upToDate := compareVersions(currentVersion, cached.ReleaseInfo.TagName) >= 0
				return cached.ReleaseInfo, upToDate, nil
			}
		}
	}

	// Cache miss or expired, fetch from GitHub
	// Use anonymous client (no token needed for public releases)
	client := NewClient("")
	release, err := client.FetchLatestHolonRelease(ctx, HolonRepoOwner, HolonRepoName)
	if err != nil {
		return nil, false, fmt.Errorf("failed to fetch latest Holon release: %w", err)
	}

	// Update cache
	if cacheDir != "" {
		cachePath := filepath.Join(cacheDir, "holon", VersionCheckCacheFile)
		_ = writeVersionCache(cachePath, &versionCacheData{
			LatestVersion: release.TagName,
			CacheTime:     time.Now(),
			ReleaseInfo:   release,
		})
	}

	// Compare versions
	upToDate := compareVersions(currentVersion, release.TagName) >= 0
	return release, upToDate, nil
}

// readVersionCache reads the version check cache from disk
func readVersionCache(cachePath string) (*versionCacheData, error) {
	data, err := os.ReadFile(cachePath)
	if err != nil {
		return nil, err
	}

	var cache versionCacheData
	if err := json.Unmarshal(data, &cache); err != nil {
		return nil, err
	}

	return &cache, nil
}

// writeVersionCache writes the version check cache to disk
func writeVersionCache(cachePath string, data *versionCacheData) error {
	// Ensure cache directory exists
	cacheDir := filepath.Dir(cachePath)
	if err := os.MkdirAll(cacheDir, 0755); err != nil {
		return err
	}

	jsonData, err := json.Marshal(data)
	if err != nil {
		return err
	}

	return os.WriteFile(cachePath, jsonData, 0644)
}

// compareVersions compares two version strings
// Returns 1 if v1 > v2, -1 if v1 < v2, 0 if equal
func compareVersions(v1, v2 string) int {
	// Handle "dev" version - always consider it the latest
	if v1 == "dev" {
		return 1
	}
	if v2 == "dev" {
		return -1
	}

	// Strip "v" prefix if present
	v1 = strings.TrimPrefix(v1, "v")
	v2 = strings.TrimPrefix(v2, "v")

	// Split by dots
	parts1 := strings.Split(v1, ".")
	parts2 := strings.Split(v2, ".")

	// Compare each part
	maxLen := len(parts1)
	if len(parts2) > maxLen {
		maxLen = len(parts2)
	}

	for i := 0; i < maxLen; i++ {
		// Get the part from each version, treating missing parts as empty
		var part1, part2 string
		if i < len(parts1) {
			part1 = parts1[i]
		}
		if i < len(parts2) {
			part2 = parts2[i]
		}

		// Both parts are missing (shouldn't happen with maxLen, but handle it)
		if part1 == "" && part2 == "" {
			continue
		}

		// One part is missing - treat it as 0 if the other is numeric
		if part1 == "" {
			// v1 is shorter, check if v2 is numeric
			var p2 int
			_, err2 := fmt.Sscanf(part2, "%d", &p2)
			if err2 == nil {
				// v2 has numeric part, v1 missing it means v1 has 0
				// If v2's part is non-zero, v2 > v1
				if p2 > 0 {
					return -1
				}
				continue
			}
			// v2 has non-numeric part, v1 is shorter so v1 < v2
			return -1
		}

		if part2 == "" {
			// v2 is shorter, check if v1 is numeric
			var p1 int
			_, err1 := fmt.Sscanf(part1, "%d", &p1)
			if err1 == nil {
				// v1 has numeric part, v2 missing it means v2 has 0
				// If v1's part is non-zero, v1 > v2
				if p1 > 0 {
					return 1
				}
				continue
			}
			// v1 has non-numeric part, v2 is shorter so v1 > v2
			return 1
		}

		// Both parts exist - try to parse as numbers
		var p1, p2 int
		_, err1 := fmt.Sscanf(part1, "%d", &p1)
		_, err2 := fmt.Sscanf(part2, "%d", &p2)

		if err1 == nil && err2 == nil {
			// Both numeric - compare numerically
			if p1 > p2 {
				return 1
			} else if p1 < p2 {
				return -1
			}
			// Equal, continue to next part
		} else if err1 == nil && err2 != nil {
			// v1 numeric, v2 non-numeric - numeric < non-numeric
			return -1
		} else if err1 != nil && err2 == nil {
			// v1 non-numeric, v2 numeric - non-numeric > numeric
			return 1
		} else {
			// Both non-numeric - compare as strings
			cmp := strings.Compare(part1, part2)
			if cmp > 0 {
				return 1
			} else if cmp < 0 {
				return -1
			}
		}
	}

	return 0
}

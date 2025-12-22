package agent

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// GitHubRelease represents a GitHub release
type GitHubRelease struct {
	TagName string `json:"tag_name"`
	Name    string `json:"name"`
	Draft   bool   `json:"draft"`
	Prerelease bool `json:"prerelease"`
	PublishedAt string `json:"published_at"`
	Assets []struct {
		Name string `json:"name"`
		URL  string `json:"browser_download_url"`
	} `json:"assets"`
}

// GetLatestAgentRelease fetches the latest agent-specific release from GitHub
// This filters through releases to find ones that contain agent bundles
func GetLatestAgentRelease(repo string) (*GitHubRelease, error) {
	url := fmt.Sprintf("https://api.github.com/repos/%s/releases", repo)

	client := &http.Client{
		Timeout: 10 * time.Second,
	}

	resp, err := client.Get(url)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch releases: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GitHub API returned status %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response body: %w", err)
	}

	var releases []GitHubRelease
	if err := json.Unmarshal(body, &releases); err != nil {
		return nil, fmt.Errorf("failed to parse releases info: %w", err)
	}

	// Find the latest release that contains agent bundles
	for _, release := range releases {
		// Skip drafts and prereleases
		if release.Draft || release.Prerelease {
			continue
		}

		// Check if this is an agent release by looking at tag name and assets
		if strings.HasPrefix(release.TagName, "agent-") {
			// Verify it has agent bundle assets
			_, _, err := FindAgentBundleAsset(&release)
			if err == nil {
				return &release, nil
			}
		}
	}

	return nil, fmt.Errorf("no agent releases found in repository %s", repo)
}

// FindAgentBundleAsset finds the agent bundle asset in a release
func FindAgentBundleAsset(release *GitHubRelease) (string, string, error) {
	// Look for agent bundle files specifically
	for _, asset := range release.Assets {
		if strings.HasSuffix(asset.Name, ".tar.gz") &&
		   !strings.HasSuffix(asset.Name, ".sha256") &&
		   (strings.Contains(asset.Name, "holon-agent") ||
		    strings.Contains(asset.Name, "agent") ||
		    strings.Contains(asset.Name, "bundle")) {
			return asset.Name, asset.URL, nil
		}
	}

	return "", "", fmt.Errorf("no agent bundle found in release %s", release.TagName)
}
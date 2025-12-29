package resolver

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/holon-run/holon/pkg/agent"
	"github.com/holon-run/holon/pkg/agent/cache"
	holonlog "github.com/holon-run/holon/pkg/log"
)

// Channel represents the agent resolution channel
type Channel string

const (
	// ChannelLatest uses the latest stable agent from cache or GitHub
	ChannelLatest Channel = "latest"
	// ChannelBuiltin forces the use of the embedded builtin agent
	ChannelBuiltin Channel = "builtin"
	// ChannelPinned is a prefix for pinned versions (e.g., "pinned:agent-claude-v0.3.0")
	ChannelPinned Channel = "pinned:"
)

// ChannelResolver resolves agent bundles based on the configured channel
type ChannelResolver struct {
	cache           *cache.Cache
	httpClient      *http.Client
	channel         Channel
	pinnedVersion   string // For ChannelPinned
	repo            string // GitHub repo for fetching releases (e.g., "holon-run/holon")
}

// NewChannelResolver creates a new channel-based agent resolver
func NewChannelResolver(cacheDir string, channel string, repo string) *ChannelResolver {
	ch := Channel(channel)

	// Check if this is a pinned version
	pinnedVersion := ""
	if strings.HasPrefix(channel, string(ChannelPinned)) {
		ch = ChannelPinned
		pinnedVersion = strings.TrimPrefix(channel, string(ChannelPinned))
	}

	return &ChannelResolver{
		cache:         cache.New(cacheDir),
		channel:       ch,
		pinnedVersion: pinnedVersion,
		repo:          repo,
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

// CanResolve returns true if the reference is empty (no explicit agent specified)
// This resolver should only be used when no explicit agent reference is provided
func (r *ChannelResolver) CanResolve(ref string) bool {
	// Only resolve empty references (when no agent is explicitly specified)
	return strings.TrimSpace(ref) == ""
}

// Resolve resolves an agent bundle based on the configured channel
func (r *ChannelResolver) Resolve(ctx context.Context, ref string) (string, error) {
	if !r.CanResolve(ref) {
		return "", fmt.Errorf("channel resolver cannot handle non-empty reference: %s", ref)
	}

	switch r.channel {
	case ChannelBuiltin:
		return r.resolveBuiltin(ctx)
	case ChannelLatest:
		return r.resolveLatest(ctx)
	case ChannelPinned:
		return r.resolvePinned(ctx)
	default:
		// Treat unknown channels as "latest" with a warning
		holonlog.Warn("unknown agent channel, treating as 'latest'", "channel", r.channel)
		return r.resolveLatest(ctx)
	}
}

// resolveBuiltin resolves the builtin agent without any network/cache lookup
func (r *ChannelResolver) resolveBuiltin(ctx context.Context) (string, error) {
	builtinAgent := agent.DefaultBuiltinAgent()
	if builtinAgent == nil {
		return "", fmt.Errorf("no builtin agent configured")
	}

	// Resolve using HTTP resolver to use cached version if available
	// Build URL with checksum
	checksum := strings.TrimPrefix(builtinAgent.Checksum, "sha256=")
	agentURL := builtinAgent.URL + "#sha256=" + checksum

	// Check cache first
	cachedPath, err := r.cache.GetBundle(agentURL, checksum)
	if err == nil {
		holonlog.Info("using cached builtin agent", "version", builtinAgent.Version, "channel", "builtin")
		return cachedPath, nil
	}

	// Download and cache
	holonlog.Info("downloading builtin agent", "version", builtinAgent.Version, "channel", "builtin")
	return r.downloadAndCache(ctx, agentURL, checksum, builtinAgent.Version)
}

// resolveLatest resolves the latest stable agent from cache or GitHub
func (r *ChannelResolver) resolveLatest(ctx context.Context) (string, error) {
	// Check if cached metadata is stale
	if !r.cache.IsLatestAgentStale() {
		// Use cached latest metadata
		metadata, err := r.cache.GetLatestAgentMetadata()
		if err == nil && metadata != nil {
			// Try to get bundle from cache
			checksum := strings.TrimPrefix(metadata.Checksum, "sha256=")
			agentURL := metadata.URL + "#sha256=" + checksum
			cachedPath, err := r.cache.GetBundle(agentURL, checksum)
			if err == nil {
				holonlog.Info("using cached latest agent", "version", metadata.Version, "channel", "latest")
				return cachedPath, nil
			}
		}
	}

	// Fetch latest from GitHub
	holonlog.Info("fetching latest agent from GitHub", "channel", "latest")
	latestRelease, err := agent.GetLatestAgentRelease(r.repo)
	if err != nil {
		// Fall back to builtin agent on network failure
		holonlog.Info("failed to fetch latest agent from GitHub, falling back to builtin", "error", err)
		return r.resolveBuiltin(ctx)
	}

	// Extract bundle info
	bundleName, bundleURL, err := agent.FindAgentBundleAsset(latestRelease)
	if err != nil {
		holonlog.Warn("failed to find agent bundle in release", "error", err)
		return r.resolveBuiltin(ctx)
	}

	// Download the .sha256 file to get checksum
	checksum, err := r.fetchChecksum(ctx, bundleURL+".sha256")
	if err != nil {
		holonlog.Warn("failed to fetch checksum, downloading without verification", "error", err)
		checksum = ""
	}

	// Store latest metadata
	metadata := &cache.LatestAgentMetadata{
		Version:   latestRelease.TagName,
		URL:       bundleURL,
		Checksum:  checksum,
		FetchedAt: time.Now().Unix(),
	}
	if err := r.cache.SetLatestAgentMetadata(metadata); err != nil {
		holonlog.Warn("failed to cache latest agent metadata", "error", err)
	}

	// Check cache first with the checksum
	checksumForCache := strings.TrimPrefix(checksum, "sha256=")
	agentURL := bundleURL
	if checksum != "" {
		agentURL = bundleURL + "#sha256=" + checksumForCache
	}

	cachedPath, err := r.cache.GetBundle(agentURL, checksumForCache)
	if err == nil {
		holonlog.Info("using cached latest agent", "version", latestRelease.TagName, "channel", "latest")
		return cachedPath, nil
	}

	// Download and cache
	holonlog.Info("downloading latest agent", "version", latestRelease.TagName, "bundle", bundleName, "channel", "latest")
	return r.downloadAndCache(ctx, agentURL, checksumForCache, latestRelease.TagName)
}

// resolvePinned resolves a specific pinned version
func (r *ChannelResolver) resolvePinned(ctx context.Context) (string, error) {
	if r.pinnedVersion == "" {
		return "", fmt.Errorf("pinned version is empty")
	}

	// Check if pinned version matches builtin
	builtinAgent := agent.DefaultBuiltinAgent()
	if builtinAgent != nil && builtinAgent.Version == r.pinnedVersion {
		holonlog.Info("pinned version matches builtin", "version", r.pinnedVersion, "channel", "pinned")
		return r.resolveBuiltin(ctx)
	}

	// For pinned versions, we need to fetch from GitHub or find in cache
	// For simplicity, if not in cache and not matching builtin, we return an error
	// Full implementation would query GitHub API for the specific release

	return "", fmt.Errorf("pinned version %q not found; use 'latest' channel to auto-fetch or provide explicit agent URL", r.pinnedVersion)
}

// downloadAndCache downloads an agent bundle and caches it
func (r *ChannelResolver) downloadAndCache(ctx context.Context, url, checksum, version string) (string, error) {
	// Remove checksum fragment from URL for downloading
	downloadURL := url
	if idx := strings.Index(url, "#sha256="); idx != -1 {
		downloadURL = url[:idx]
	}

	// Create request
	req, err := http.NewRequestWithContext(ctx, "GET", downloadURL, nil)
	if err != nil {
		return "", fmt.Errorf("failed to create request: %w", err)
	}

	resp, err := r.httpClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("failed to download agent bundle: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("failed to download agent bundle: HTTP %d", resp.StatusCode)
	}

	// Calculate checksum while downloading
	hasher := sha256.New()
	multiWriter := io.MultiWriter(hasher)

	// We need to tee the response to both calculate checksum and store it
	size, err := io.Copy(multiWriter, resp.Body)
	if err != nil {
		return "", fmt.Errorf("failed to download agent bundle: %w", err)
	}

	actualChecksum := hex.EncodeToString(hasher.Sum(nil))

	// Verify checksum if expected
	if checksum != "" && actualChecksum != checksum {
		return "", fmt.Errorf("checksum verification failed: expected %s, got %s", checksum, actualChecksum)
	}

	if checksum == "" {
		holonlog.Warn("downloaded agent bundle without integrity verification", "url", downloadURL)
	}

	// Read the response body again for caching (we need to re-download since we already consumed it)
	// In production, we'd want to stream to a temp file while calculating checksum
	req2, err := http.NewRequestWithContext(ctx, "GET", downloadURL, nil)
	if err != nil {
		return "", fmt.Errorf("failed to create second request: %w", err)
	}

	resp2, err := r.httpClient.Do(req2)
	if err != nil {
		return "", fmt.Errorf("failed to re-download agent bundle: %w", err)
	}
	defer resp2.Body.Close()

	// Cache the bundle with extended metadata including version
	cachedPath, err := r.cache.StoreBundleWithVersion(url, actualChecksum, resp2.Body, size, version)
	if err != nil {
		return "", fmt.Errorf("failed to cache agent bundle: %w", err)
	}

	return cachedPath, nil
}

// fetchChecksum fetches the SHA256 checksum file for a bundle
func (r *ChannelResolver) fetchChecksum(ctx context.Context, checksumURL string) (string, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", checksumURL, nil)
	if err != nil {
		return "", fmt.Errorf("failed to create checksum request: %w", err)
	}

	resp, err := r.httpClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("failed to fetch checksum: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("failed to fetch checksum: HTTP %d", resp.StatusCode)
	}

	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("failed to read checksum: %w", err)
	}

	// Checksum files typically contain: <hash>  <filename>
	// We just need the hash part
	parts := strings.Fields(string(data))
	if len(parts) == 0 {
		return "", fmt.Errorf("empty checksum file")
	}

	return parts[0], nil
}

// readBundleMetadata reads bundle metadata from a file
func (r *ChannelResolver) readBundleMetadata(path string) (*cache.BundleMetadata, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var metadata cache.BundleMetadata
	if err := json.Unmarshal(data, &metadata); err != nil {
		return nil, fmt.Errorf("failed to unmarshal metadata: %w", err)
	}

	return &metadata, nil
}

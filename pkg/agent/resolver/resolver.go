package resolver

import (
	"context"
	"crypto/sha256"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/holon-run/holon/pkg/agent/cache"
)

// Resolver interface defines how agent references are resolved to local paths
type Resolver interface {
	Resolve(ctx context.Context, ref string) (string, error)
	CanResolve(ref string) bool
}

// Registry holds all registered resolvers
type Registry struct {
	resolvers []Resolver
	cache     *cache.Cache
}

// NewRegistry creates a new resolver registry with default resolvers
func NewRegistry(cacheDir string) *Registry {
	cache := cache.New(cacheDir)

	// Configure transport with granular timeout controls for poor network conditions
	transport := &http.Transport{
		// Connection timeout - time to establish the first connection
		DialContext: (&net.Dialer{
			Timeout:   30 * time.Second, // Connection establishment timeout
			KeepAlive: 30 * time.Second, // Keep alive for connections
		}).DialContext,

		// Maximum time to wait for a TLS handshake
		TLSHandshakeTimeout:   30 * time.Second,

		// Response header timeout - time to wait for response headers
		ResponseHeaderTimeout: 60 * time.Second,

		// Connection management for better performance in poor network conditions
		MaxIdleConns:          100,
		MaxIdleConnsPerHost:   10,
		IdleConnTimeout:       90 * time.Second, // How long idle connections remain open
		DisableKeepAlives:     false,
		DisableCompression:    false,

		// Expect continue timeout for request bodies
		ExpectContinueTimeout: 1 * time.Second,
	}

	return &Registry{
		cache: cache,
		resolvers: []Resolver{
			&FileResolver{},
			&HTTPResolver{
				cache: cache,
				client: &http.Client{
					// Overall timeout for the entire request (including redirects, download, etc.)
					// Set higher to accommodate slow downloads and network issues
					Timeout: 300 * time.Second, // 5 minutes total timeout

					// Use custom transport with granular timeout controls
					Transport: transport,

					// Redirect handling
					CheckRedirect: func(req *http.Request, via []*http.Request) error {
						// Enforce a maximum number of redirects to avoid redirect loops
						const maxRedirects = 10
						if len(via) >= maxRedirects {
							return fmt.Errorf("stopped after %d redirects", maxRedirects)
						}

						// Only allow redirects to HTTP or HTTPS endpoints
						if req.URL != nil && req.URL.Scheme != "" {
							scheme := strings.ToLower(req.URL.Scheme)
							if scheme != "http" && scheme != "https" {
								return fmt.Errorf("redirect to unsupported scheme: %s", req.URL.Scheme)
							}
						}

						return nil
					},
				},
			},
			&AliasResolver{
				cache: cache,
			},
		},
	}
}

// Resolve resolves an agent reference to a local bundle path
func (r *Registry) Resolve(ctx context.Context, ref string) (string, error) {
	ref = strings.TrimSpace(ref)
	if ref == "" {
		return "", fmt.Errorf("empty agent reference")
	}

	for _, resolver := range r.resolvers {
		if resolver.CanResolve(ref) {
			return resolver.Resolve(ctx, ref)
		}
	}

	return "", fmt.Errorf("no resolver found for agent reference: %s", ref)
}

// FileResolver resolves local file paths
type FileResolver struct{}

func (r *FileResolver) CanResolve(ref string) bool {
	// Check if it's an absolute path
	if filepath.IsAbs(ref) {
		return true
	}

	// Check if file exists relative to current working directory
	if _, err := os.Stat(ref); err == nil {
		return true
	}

	return false
}

func (r *FileResolver) Resolve(ctx context.Context, ref string) (string, error) {
	absPath, err := filepath.Abs(ref)
	if err != nil {
		return "", fmt.Errorf("failed to resolve absolute path: %w", err)
	}

	info, err := os.Stat(absPath)
	if err != nil {
		return "", fmt.Errorf("agent bundle not found: %w", err)
	}

	if info.IsDir() {
		return "", fmt.Errorf("agent bundle path is a directory: %s", absPath)
	}

	return absPath, nil
}

// HTTPResolver resolves HTTPS URLs to agent bundles
type HTTPResolver struct {
	cache  *cache.Cache
	client *http.Client
}

func (r *HTTPResolver) CanResolve(ref string) bool {
	return strings.HasPrefix(ref, "https://") || strings.HasPrefix(ref, "http://")
}

func (r *HTTPResolver) Resolve(ctx context.Context, ref string) (string, error) {
	// Parse URL and extract checksum if present
	parsedURL, err := url.Parse(ref)
	if err != nil {
		return "", fmt.Errorf("invalid URL: %w", err)
	}

	// Extract checksum from fragment
	var expectedChecksum string
	if parsedURL.Fragment != "" {
		if strings.HasPrefix(parsedURL.Fragment, "sha256=") {
			expectedChecksum = strings.TrimPrefix(parsedURL.Fragment, "sha256=")
			// Remove fragment from URL for downloading
			parsedURL.Fragment = ""
			ref = parsedURL.String()
		}
	}

	// Check cache first
	cachedPath, err := r.cache.GetBundle(ref, expectedChecksum)
	if err == nil {
		return cachedPath, nil
	}

	// Download and cache
	return r.downloadAndCache(ctx, ref, expectedChecksum)
}

func (r *HTTPResolver) downloadAndCache(ctx context.Context, url, expectedChecksum string) (string, error) {
	// Create a temporary file for downloading
	tempFile, err := os.CreateTemp("", "holon-agent-download-*.tar.gz")
	if err != nil {
		return "", fmt.Errorf("failed to create temp file: %w", err)
	}
	defer tempFile.Close()
	defer os.Remove(tempFile.Name())

	// Download the file
	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		return "", fmt.Errorf("failed to create request: %w", err)
	}

	resp, err := r.client.Do(req)
	if err != nil {
		return "", fmt.Errorf("failed to download agent bundle: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		// Read a small portion of the response body for better diagnostics
		var bodySnippet string
		if resp.Body != nil {
			const maxBodyBytes = 1024
			if b, readErr := io.ReadAll(io.LimitReader(resp.Body, maxBodyBytes)); readErr == nil && len(b) > 0 {
				bodySnippet = strings.TrimSpace(string(b))
			}
		}

		if bodySnippet != "" {
			return "", fmt.Errorf(
				"failed to download agent bundle: HTTP %d %s: %s",
				resp.StatusCode,
				http.StatusText(resp.StatusCode),
				bodySnippet,
			)
		}

		return "", fmt.Errorf(
			"failed to download agent bundle: HTTP %d %s",
			resp.StatusCode,
			http.StatusText(resp.StatusCode),
		)
	}

	// Calculate checksum while downloading
	hasher := sha256.New()
	multiWriter := io.MultiWriter(tempFile, hasher)

	size, err := io.Copy(multiWriter, resp.Body)
	if err != nil {
		return "", fmt.Errorf("failed to download agent bundle: %w", err)
	}

	actualChecksum := fmt.Sprintf("%x", hasher.Sum(nil))

	// Verify checksum if expected
	if expectedChecksum != "" && actualChecksum != expectedChecksum {
		return "", fmt.Errorf("checksum verification failed: expected %s, got %s", expectedChecksum, actualChecksum)
	}

	if expectedChecksum == "" {
		fmt.Fprintf(os.Stderr, "Warning: Downloaded agent bundle without integrity verification (URL: %s)\n", url)
	}

	// Seek back to beginning of file for caching
	if _, err := tempFile.Seek(0, 0); err != nil {
		return "", fmt.Errorf("failed to seek temp file: %w", err)
	}

	// Cache the downloaded bundle
	cachedPath, err := r.cache.StoreBundle(url, actualChecksum, tempFile, size)
	if err != nil {
		return "", fmt.Errorf("failed to cache agent bundle: %w", err)
	}

	return cachedPath, nil
}

// AliasResolver resolves named aliases to agent bundles
type AliasResolver struct {
	cache *cache.Cache
}

func (r *AliasResolver) CanResolve(ref string) bool {
	// Don't resolve URLs, absolute paths, or aliases containing path separators
	if strings.Contains(ref, "://") || filepath.IsAbs(ref) || strings.Contains(ref, "/") || strings.Contains(ref, "\\") {
		return false
	}

	// Check if this is a known alias
	_, err := r.cache.GetAlias(ref)
	return err == nil
}

func (r *AliasResolver) Resolve(ctx context.Context, ref string) (string, error) {
	url, err := r.cache.GetAlias(ref)
	if err != nil {
		return "", fmt.Errorf("alias not found: %s", ref)
	}

	// Delegate to HTTP resolver with same granular timeout configuration
	transport := &http.Transport{
		DialContext: (&net.Dialer{
			Timeout:   30 * time.Second,
			KeepAlive: 30 * time.Second,
		}).DialContext,
		TLSHandshakeTimeout:   30 * time.Second,
		ResponseHeaderTimeout: 60 * time.Second,
		MaxIdleConns:          100,
		MaxIdleConnsPerHost:   10,
		IdleConnTimeout:       90 * time.Second,
		DisableKeepAlives:     false,
		DisableCompression:    false,
		ExpectContinueTimeout: 1 * time.Second,
	}

	httpResolver := &HTTPResolver{
		cache: r.cache,
		client: &http.Client{
			Timeout:   300 * time.Second,
			Transport: transport,
			CheckRedirect: func(req *http.Request, via []*http.Request) error {
				const maxRedirects = 10
				if len(via) >= maxRedirects {
					return fmt.Errorf("stopped after %d redirects", maxRedirects)
				}
				if req.URL != nil && req.URL.Scheme != "" {
					scheme := strings.ToLower(req.URL.Scheme)
					if scheme != "http" && scheme != "https" {
						return fmt.Errorf("redirect to unsupported scheme: %s", req.URL.Scheme)
					}
				}
				return nil
			},
		},
	}

	return httpResolver.Resolve(ctx, url)
}
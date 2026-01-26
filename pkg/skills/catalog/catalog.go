// Package catalog provides skill catalog adapters for resolving skill references
// from various sources including skills.sh (Vercel-style catalog) and GitHub repositories.
package catalog

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

const (
	// SkillsShCatalogURL is the default Vercel-style catalog URL
	SkillsShCatalogURL = "https://catalog.skills.sh"
	// GitHubArchiveURL is the base URL for GitHub repository archives
	GitHubArchiveURL = "https://github.com/%s/%s/archive/refs/heads/%s.zip"
	// GitHubDefaultBranch is the default branch to use when none is specified
	GitHubDefaultBranch = "main"
	// RequestTimeout is the timeout for catalog HTTP requests
	RequestTimeout = 30 * time.Second
)

// CatalogEntry represents a single skill in a catalog
type CatalogEntry struct {
	Name        string `json:"name"`
	URL         string `json:"url"`
	Description string `json:"description"`
	SHA256      string `json:"sha256,omitempty"`
	Version     string `json:"version,omitempty"`
}

// Catalog represents a skill catalog
type Catalog struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	Skills      []CatalogEntry `json:"skills"`
}

// Adapter is the interface for catalog adapters
type Adapter interface {
	// Resolve resolves a skill reference to a downloadable URL and optional checksum
	Resolve(ref string) (*CatalogEntry, error)
	// Name returns the adapter name
	Name() string
}

// SkillsShAdapter resolves skills from the Vercel-style skills.sh catalog
type SkillsShAdapter struct {
	client     *http.Client
	catalogURL string
}

// NewSkillsShAdapter creates a new skills.sh catalog adapter
func NewSkillsShAdapter() *SkillsShAdapter {
	return &SkillsShAdapter{
		client: &http.Client{
			Timeout: RequestTimeout,
		},
		catalogURL: SkillsShCatalogURL,
	}
}

// Resolve resolves a skills:<package> reference to a catalog entry
func (a *SkillsShAdapter) Resolve(ref string) (*CatalogEntry, error) {
	// Parse the reference
	if !strings.HasPrefix(ref, "skills:") {
		return nil, fmt.Errorf("invalid skills.sh reference: %s (must start with 'skills:')", ref)
	}

	packageName := strings.TrimPrefix(ref, "skills:")
	if packageName == "" {
		return nil, fmt.Errorf("empty package name in skills.sh reference")
	}

	// Fetch catalog entry for the package
	catalogURL := fmt.Sprintf("%s/skill/%s", a.catalogURL, packageName)

	holonlog.Debug("fetching skills.sh catalog entry", "package", packageName, "url", catalogURL)

	resp, err := a.client.Get(catalogURL)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch skills.sh catalog entry: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusNotFound {
		return nil, fmt.Errorf("package not found in skills.sh catalog: %s", packageName)
	}

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("skills.sh catalog returned HTTP %d: %s", resp.StatusCode, string(body))
	}

	// Parse response
	var entry CatalogEntry
	if err := json.NewDecoder(resp.Body).Decode(&entry); err != nil {
		return nil, fmt.Errorf("failed to parse skills.sh catalog response: %w", err)
	}

	// Validate entry
	if entry.URL == "" {
		return nil, fmt.Errorf("skills.sh catalog entry missing URL for package: %s", packageName)
	}

	holonlog.Info("resolved skills.sh package", "package", packageName, "url", entry.URL)

	return &entry, nil
}

func (a *SkillsShAdapter) Name() string {
	return "skills.sh"
}

// GitHubAdapter resolves skills from GitHub repositories
type GitHubAdapter struct {
	client *http.Client
}

// NewGitHubAdapter creates a new GitHub catalog adapter
func NewGitHubAdapter() *GitHubAdapter {
	return &GitHubAdapter{
		client: &http.Client{
			Timeout: RequestTimeout,
		},
	}
}

// Resolve resolves a gh:<owner>/<repo>/<path> reference to a downloadable URL
func (a *GitHubAdapter) Resolve(ref string) (*CatalogEntry, error) {
	// Parse the reference
	if !strings.HasPrefix(ref, "gh:") {
		return nil, fmt.Errorf("invalid GitHub reference: %s (must start with 'gh:')", ref)
	}

	path := strings.TrimPrefix(ref, "gh:")
	if path == "" {
		return nil, fmt.Errorf("empty path in GitHub reference")
	}

	// Parse owner/repo/path
	parts := strings.SplitN(path, "/", 3)
	if len(parts) < 2 {
		return nil, fmt.Errorf("invalid GitHub reference format: %s (expected gh:owner/repo or gh:owner/repo/path)", ref)
	}

	owner := parts[0]
	repo := parts[1]
	repoPath := ""
	if len(parts) == 3 {
		repoPath = parts[2]
	}

	// Construct archive URL
	archiveURL := fmt.Sprintf(GitHubArchiveURL, owner, repo, GitHubDefaultBranch)

	entry := &CatalogEntry{
		Name:        fmt.Sprintf("%s/%s", owner, repo),
		URL:         archiveURL,
		Description: fmt.Sprintf("GitHub repository: %s/%s", owner, repo),
	}

	// Add repo path to description if specified
	if repoPath != "" {
		entry.Description += fmt.Sprintf(" (path: %s)", repoPath)
	}

	holonlog.Info("resolved GitHub repository", "owner", owner, "repo", repo, "path", repoPath, "url", archiveURL)

	return entry, nil
}

func (a *GitHubAdapter) Name() string {
	return "github"
}

// BuiltinCatalogAdapter resolves skills from a built-in JSON catalog
type BuiltinCatalogAdapter struct {
	catalog *Catalog
}

// NewBuiltinCatalogAdapter creates a new built-in catalog adapter
func NewBuiltinCatalogAdapter(catalogJSON []byte) (*BuiltinCatalogAdapter, error) {
	var catalog Catalog
	if err := json.Unmarshal(catalogJSON, &catalog); err != nil {
		return nil, fmt.Errorf("failed to parse built-in catalog: %w", err)
	}

	return &BuiltinCatalogAdapter{
		catalog: &catalog,
	}, nil
}

// Resolve resolves a skill name from the built-in catalog
func (a *BuiltinCatalogAdapter) Resolve(ref string) (*CatalogEntry, error) {
	// Search for the skill in the catalog
	for _, skill := range a.catalog.Skills {
		if skill.Name == ref {
			holonlog.Debug("resolved from built-in catalog", "skill", ref, "url", skill.URL)
			return &skill, nil
		}
	}

	return nil, fmt.Errorf("skill not found in built-in catalog: %s", ref)
}

func (a *BuiltinCatalogAdapter) Name() string {
	return "builtin"
}

// Registry manages multiple catalog adapters
type Registry struct {
	adapters []Adapter
}

// NewRegistry creates a new catalog adapter registry
func NewRegistry() *Registry {
	return &Registry{
		adapters: []Adapter{},
	}
}

// Register adds an adapter to the registry
func (r *Registry) Register(adapter Adapter) {
	r.adapters = append(r.adapters, adapter)
}

// Resolve attempts to resolve a skill reference using registered adapters
// Returns the first successful resolution or an error if all adapters fail
func (r *Registry) Resolve(ref string) (*CatalogEntry, error) {
	var lastErr error

	for _, adapter := range r.adapters {
		entry, err := adapter.Resolve(ref)
		if err == nil {
			return entry, nil
		}
		// Keep track of the last error
		lastErr = err
		holonlog.Debug("adapter failed to resolve", "adapter", adapter.Name(), "ref", ref, "error", err)
	}

	if lastErr != nil {
		return nil, fmt.Errorf("all catalog adapters failed: %w", lastErr)
	}

	return nil, fmt.Errorf("no catalog adapters registered")
}

// IsCatalogRef returns true if the ref appears to be a catalog reference
func IsCatalogRef(ref string) bool {
	return strings.HasPrefix(ref, "skills:") || strings.HasPrefix(ref, "gh:")
}

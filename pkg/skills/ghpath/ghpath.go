package ghpath

import (
	"archive/zip"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

const (
	defaultCacheDirName = "github-path-skills"
	defaultGitHubURL    = "https://github.com"
	defaultCodeloadURL  = "https://codeload.github.com"
	defaultGitHubAPIURL = "https://api.github.com"
	defaultRef          = "main"
	maxZipSize          = 100 * 1024 * 1024
	httpTimeout         = 5 * time.Minute
)

var (
	ErrNotGitHubPathRef = errors.New("not a GitHub path skill reference")
)

// Ref describes a GitHub repository path skill reference.
type Ref struct {
	Owner string
	Repo  string
	Ref   string
	Path  string
}

// GitRunner executes git commands.
type GitRunner interface {
	Run(ctx context.Context, args ...string) error
}

type defaultGitRunner struct{}

func (r *defaultGitRunner) Run(ctx context.Context, args ...string) error {
	cmd := exec.CommandContext(ctx, "git", args...)
	cmd.Env = append(os.Environ(), "GIT_TERMINAL_PROMPT=0")
	out, err := cmd.CombinedOutput()
	if err != nil {
		safeArgs := sanitizeGitArgs(args)
		trimmed := strings.TrimSpace(string(out))
		if trimmed == "" {
			return fmt.Errorf("git %s failed: %w", safeArgs, err)
		}
		return fmt.Errorf("git %s failed: %w: %s", safeArgs, err, trimmed)
	}
	return nil
}

func sanitizeGitArgs(args []string) string {
	safe := make([]string, len(args))
	for i, arg := range args {
		if strings.HasPrefix(arg, "http.extraHeader=Authorization: Bearer ") {
			safe[i] = "http.extraHeader=Authorization: Bearer [REDACTED]"
			continue
		}
		safe[i] = arg
	}
	return strings.Join(safe, " ")
}

// Resolver resolves GitHub path-based skill references into local skill directories.
type Resolver struct {
	cacheDir        string
	githubBaseURL   string
	codeloadBaseURL string
	apiBaseURL      string
	httpClient      *http.Client
	gitRunner       GitRunner
	token           string
}

// Option customizes resolver behavior.
type Option func(*Resolver)

// WithGitRunner injects a custom git command runner.
func WithGitRunner(runner GitRunner) Option {
	return func(r *Resolver) {
		r.gitRunner = runner
	}
}

// WithHTTPClient injects a custom HTTP client.
func WithHTTPClient(client *http.Client) Option {
	return func(r *Resolver) {
		r.httpClient = client
	}
}

// WithGitHubBaseURL overrides the GitHub base URL (used for sparse checkout clone URL).
func WithGitHubBaseURL(rawURL string) Option {
	return func(r *Resolver) {
		r.githubBaseURL = strings.TrimSuffix(rawURL, "/")
	}
}

// WithCodeloadBaseURL overrides the codeload base URL.
func WithCodeloadBaseURL(rawURL string) Option {
	return func(r *Resolver) {
		r.codeloadBaseURL = strings.TrimSuffix(rawURL, "/")
	}
}

// WithGitHubAPIBaseURL overrides the GitHub API base URL.
func WithGitHubAPIBaseURL(rawURL string) Option {
	return func(r *Resolver) {
		r.apiBaseURL = strings.TrimSuffix(rawURL, "/")
	}
}

// WithToken overrides token discovery and sets an explicit token.
func WithToken(token string) Option {
	return func(r *Resolver) {
		r.token = strings.TrimSpace(token)
	}
}

// NewResolver creates a GitHub path skill resolver.
func NewResolver(cacheDir string, opts ...Option) *Resolver {
	if cacheDir == "" {
		homeDir, err := os.UserHomeDir()
		if err != nil {
			cacheDir = filepath.Join(os.TempDir(), ".holon-cache", defaultCacheDirName)
		} else {
			cacheDir = filepath.Join(homeDir, ".holon", "cache", defaultCacheDirName)
		}
	}

	r := &Resolver{
		cacheDir:        cacheDir,
		githubBaseURL:   defaultGitHubURL,
		codeloadBaseURL: defaultCodeloadURL,
		apiBaseURL:      defaultGitHubAPIURL,
		httpClient: &http.Client{
			Timeout: httpTimeout,
		},
		gitRunner: &defaultGitRunner{},
	}

	for _, opt := range opts {
		opt(r)
	}

	if r.httpClient == nil {
		r.httpClient = &http.Client{Timeout: httpTimeout}
	}
	if r.gitRunner == nil {
		r.gitRunner = &defaultGitRunner{}
	}

	_ = os.MkdirAll(r.cacheDir, 0o755)
	return r
}

// ParseRef parses supported GitHub path reference formats.
// Supported formats:
// - ghpath:<owner>/<repo>/<path>@<ref>
// - github://<owner>/<repo>/<ref>/<path>
// - https://github.com/<owner>/<repo>/tree/<ref>/<path>
func ParseRef(input string) (*Ref, error) {
	if strings.HasPrefix(input, "ghpath:") {
		return parseGhpathRef(input)
	}
	if strings.HasPrefix(input, "github://") {
		return parseGitHubSchemeRef(input)
	}
	if strings.HasPrefix(input, "https://github.com/") || strings.HasPrefix(input, "https://www.github.com/") {
		if !strings.Contains(input, "/tree/") {
			return nil, ErrNotGitHubPathRef
		}
		return parseGitHubTreeURL(input)
	}
	return nil, ErrNotGitHubPathRef
}

func parseGhpathRef(input string) (*Ref, error) {
	raw := strings.TrimPrefix(input, "ghpath:")
	if raw == "" {
		return nil, fmt.Errorf("invalid ghpath reference: empty content")
	}

	at := strings.LastIndex(raw, "@")
	if at <= 0 || at == len(raw)-1 {
		return nil, fmt.Errorf("invalid ghpath reference %q: expected ghpath:<owner>/<repo>/<path>@<ref>", input)
	}

	left := raw[:at]
	refName := strings.TrimSpace(raw[at+1:])
	parts := strings.SplitN(left, "/", 3)
	if len(parts) < 3 {
		return nil, fmt.Errorf("invalid ghpath reference %q: expected owner/repo/path", input)
	}

	return buildRef(parts[0], parts[1], refName, parts[2])
}

func parseGitHubSchemeRef(input string) (*Ref, error) {
	raw := strings.TrimPrefix(input, "github://")
	parts := strings.SplitN(raw, "/", 4)
	if len(parts) < 4 {
		return nil, fmt.Errorf("invalid github:// reference %q: expected github://<owner>/<repo>/<ref>/<path>", input)
	}
	return buildRef(parts[0], parts[1], parts[2], parts[3])
}

func parseGitHubTreeURL(input string) (*Ref, error) {
	u, err := url.Parse(input)
	if err != nil {
		return nil, fmt.Errorf("invalid GitHub URL %q: %w", input, err)
	}

	segments := strings.Split(strings.Trim(u.Path, "/"), "/")
	if len(segments) < 5 {
		return nil, fmt.Errorf("invalid GitHub tree URL %q: expected /<owner>/<repo>/tree/<ref>/<path>", input)
	}
	if segments[2] != "tree" {
		return nil, fmt.Errorf("invalid GitHub tree URL %q: missing /tree/ segment", input)
	}

	return buildRef(segments[0], segments[1], segments[3], strings.Join(segments[4:], "/"))
}

func buildRef(owner, repo, refName, repoPath string) (*Ref, error) {
	owner = strings.TrimSpace(owner)
	repo = strings.TrimSpace(repo)
	refName = strings.TrimSpace(refName)
	repoPath = strings.TrimSpace(repoPath)

	if owner == "" || repo == "" {
		return nil, fmt.Errorf("owner and repo are required")
	}
	if strings.Contains(owner, "/") || strings.Contains(repo, "/") {
		return nil, fmt.Errorf("owner/repo must not contain additional path separators")
	}
	if refName == "" {
		refName = defaultRef
	}

	cleanPath, err := normalizeRepoPath(repoPath)
	if err != nil {
		return nil, err
	}

	return &Ref{
		Owner: owner,
		Repo:  repo,
		Ref:   refName,
		Path:  cleanPath,
	}, nil
}

func normalizeRepoPath(repoPath string) (string, error) {
	if repoPath == "" {
		return "", fmt.Errorf("skill path is required")
	}
	if strings.Contains(repoPath, "\\") {
		return "", fmt.Errorf("skill path must use forward slashes: %s", repoPath)
	}
	if len(repoPath) >= 2 && repoPath[1] == ':' {
		return "", fmt.Errorf("skill path must not include a drive prefix: %s", repoPath)
	}
	if strings.HasPrefix(repoPath, "/") {
		return "", fmt.Errorf("skill path must be repository-relative")
	}

	clean := path.Clean(repoPath)
	if clean == "." || clean == "" {
		return "", fmt.Errorf("skill path must not be empty")
	}
	if clean == ".." || strings.HasPrefix(clean, "../") {
		return "", fmt.Errorf("skill path must not escape repository root: %s", repoPath)
	}
	return clean, nil
}

// Resolve resolves a parsed GitHub path reference and returns a cached local skill directory path.
func (r *Resolver) Resolve(ctx context.Context, ref *Ref) (string, error) {
	if ref == nil {
		return "", fmt.Errorf("nil GitHub skill reference")
	}
	if ctx == nil {
		ctx = context.Background()
	}

	cachePath := filepath.Join(r.cacheDir, cacheKey(ref))
	if hasSkillManifest(cachePath) {
		return cachePath, nil
	}

	if err := os.MkdirAll(r.cacheDir, 0o755); err != nil {
		return "", fmt.Errorf("failed to create cache directory: %w", err)
	}

	tmpParent, err := os.MkdirTemp(r.cacheDir, "ghpath-resolve-*")
	if err != nil {
		return "", fmt.Errorf("failed to create temporary cache directory: %w", err)
	}
	defer os.RemoveAll(tmpParent)

	tmpSkillPath := filepath.Join(tmpParent, "skill")

	sparseErr := r.resolveViaSparseCheckout(ctx, ref, tmpSkillPath)
	if sparseErr != nil {
		if err := checkContextCanceled(ctx); err != nil {
			return "", err
		}
		zipErr := r.resolveViaZip(ctx, ref, tmpSkillPath)
		if zipErr != nil {
			return "", fmt.Errorf("failed to resolve GitHub skill %s/%s path %q at ref %q (sparse checkout: %v; zip fallback: %v)",
				ref.Owner, ref.Repo, ref.Path, ref.Ref, sparseErr, zipErr)
		}
	}

	if err := os.RemoveAll(cachePath); err != nil && !os.IsNotExist(err) {
		return "", fmt.Errorf("failed to clear previous cache at %s: %w", cachePath, err)
	}

	if err := os.Rename(tmpSkillPath, cachePath); err != nil {
		if err := copyDir(tmpSkillPath, cachePath); err != nil {
			return "", fmt.Errorf("failed to move/copy resolved skill into cache: %w", err)
		}
	}

	if err := ensureSkillManifestFile(cachePath); err != nil {
		return "", fmt.Errorf("resolved path %s has invalid SKILL.md: %w", cachePath, err)
	}

	return cachePath, nil
}

func (r *Resolver) resolveViaSparseCheckout(ctx context.Context, ref *Ref, destPath string) error {
	if err := checkContextCanceled(ctx); err != nil {
		return err
	}

	tmpRepoDir, err := os.MkdirTemp("", "holon-ghpath-repo-*")
	if err != nil {
		return fmt.Errorf("failed to create temporary repository directory: %w", err)
	}
	defer os.RemoveAll(tmpRepoDir)

	cloneURL := fmt.Sprintf("%s/%s/%s.git", strings.TrimSuffix(r.githubBaseURL, "/"), url.PathEscape(ref.Owner), url.PathEscape(ref.Repo))

	cloneArgs := []string{}
	if token := r.authToken(); token != "" {
		cloneArgs = append(cloneArgs, "-c", fmt.Sprintf("http.extraHeader=Authorization: Bearer %s", token))
	}
	cloneArgs = append(cloneArgs,
		"clone",
		"--filter=blob:none",
		"--depth", "1",
		"--sparse",
		"--single-branch",
		"--branch", ref.Ref,
		cloneURL,
		tmpRepoDir,
	)

	if err := r.gitRunner.Run(ctx, cloneArgs...); err != nil {
		return fmt.Errorf("git clone failed: %w", err)
	}
	if err := checkContextCanceled(ctx); err != nil {
		return err
	}

	if err := r.gitRunner.Run(ctx, "-C", tmpRepoDir, "sparse-checkout", "set", "--no-cone", ref.Path); err != nil {
		return fmt.Errorf("git sparse-checkout failed: %w", err)
	}
	if err := checkContextCanceled(ctx); err != nil {
		return err
	}

	skillPath := filepath.Join(tmpRepoDir, filepath.FromSlash(ref.Path))
	if err := ensureSkillManifestFile(skillPath); err != nil {
		return fmt.Errorf("repository path %q has invalid SKILL.md: %w", ref.Path, err)
	}
	if err := checkContextCanceled(ctx); err != nil {
		return err
	}

	if err := copyDir(skillPath, destPath); err != nil {
		return fmt.Errorf("failed to copy sparse-checked skill path: %w", err)
	}

	return nil
}

func (r *Resolver) resolveViaZip(ctx context.Context, ref *Ref, destPath string) error {
	zipData, err := r.downloadArchiveZip(ctx, ref)
	if err != nil {
		return err
	}

	if err := extractSkillPathFromZip(zipData, ref.Path, destPath); err != nil {
		return fmt.Errorf("failed to extract skill path %q from archive: %w", ref.Path, err)
	}

	return nil
}

func (r *Resolver) downloadArchiveZip(ctx context.Context, ref *Ref) ([]byte, error) {
	candidates := []string{
		fmt.Sprintf("%s/%s/%s/zip/refs/heads/%s", r.codeloadBaseURL, ref.Owner, ref.Repo, url.PathEscape(ref.Ref)),
		fmt.Sprintf("%s/%s/%s/zip/refs/tags/%s", r.codeloadBaseURL, ref.Owner, ref.Repo, url.PathEscape(ref.Ref)),
		fmt.Sprintf("%s/%s/%s/zip/%s", r.codeloadBaseURL, ref.Owner, ref.Repo, url.PathEscape(ref.Ref)),
	}

	var lastErr error
	for _, downloadURL := range candidates {
		data, statusCode, err := r.download(ctx, downloadURL, "")
		if err == nil {
			return data, nil
		}
		lastErr = fmt.Errorf("%s (HTTP %d): %w", downloadURL, statusCode, err)
		if statusCode != http.StatusUnauthorized && statusCode != http.StatusForbidden && statusCode != http.StatusNotFound {
			break
		}
	}

	token := r.authToken()
	if token == "" {
		if lastErr != nil {
			return nil, fmt.Errorf("failed to download codeload archive: %w", lastErr)
		}
		return nil, fmt.Errorf("failed to download codeload archive")
	}

	apiURL := fmt.Sprintf("%s/repos/%s/%s/zipball/%s", r.apiBaseURL, ref.Owner, ref.Repo, url.PathEscape(ref.Ref))
	data, statusCode, err := r.download(ctx, apiURL, token)
	if err != nil {
		if lastErr != nil {
			return nil, fmt.Errorf("codeload archive failed (%v); GitHub API zipball failed (%s HTTP %d): %w", lastErr, apiURL, statusCode, err)
		}
		return nil, fmt.Errorf("GitHub API zipball failed (%s HTTP %d): %w", apiURL, statusCode, err)
	}

	return data, nil
}

func (r *Resolver) download(ctx context.Context, downloadURL, token string) ([]byte, int, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, downloadURL, nil)
	if err != nil {
		return nil, 0, fmt.Errorf("failed to create request: %w", err)
	}

	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
		req.Header.Set("Accept", "application/vnd.github+json")
	}

	resp, err := r.httpClient.Do(req)
	if err != nil {
		return nil, 0, fmt.Errorf("request failed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, readErr := io.ReadAll(io.LimitReader(resp.Body, 1024))
		if readErr != nil {
			return nil, resp.StatusCode, fmt.Errorf("unexpected HTTP status %d and failed to read error body: %w", resp.StatusCode, readErr)
		}
		return nil, resp.StatusCode, fmt.Errorf("unexpected HTTP status %d: %s", resp.StatusCode, strings.TrimSpace(string(body)))
	}

	if resp.ContentLength > 0 && resp.ContentLength > maxZipSize {
		return nil, resp.StatusCode, fmt.Errorf("archive too large: %d bytes (max %d)", resp.ContentLength, maxZipSize)
	}

	data, err := io.ReadAll(io.LimitReader(resp.Body, maxZipSize+1))
	if err != nil {
		return nil, resp.StatusCode, fmt.Errorf("failed to read archive: %w", err)
	}
	if int64(len(data)) > maxZipSize {
		return nil, resp.StatusCode, fmt.Errorf("archive exceeds maximum size %d", maxZipSize)
	}

	return data, resp.StatusCode, nil
}

func extractSkillPathFromZip(zipData []byte, repoPath, destPath string) error {
	reader, err := zip.NewReader(bytes.NewReader(zipData), int64(len(zipData)))
	if err != nil {
		return fmt.Errorf("failed to open ZIP archive: %w", err)
	}

	tmpDir, err := os.MkdirTemp("", "holon-ghpath-extract-*")
	if err != nil {
		return fmt.Errorf("failed to create temporary extraction directory: %w", err)
	}
	defer os.RemoveAll(tmpDir)

	targetSkillSuffix := "/" + path.Join(repoPath, "SKILL.md")
	var skillArchiveRoots []string

	for _, file := range reader.File {
		sanitizedPath, err := sanitizeArchiveEntryPath(file.Name)
		if err != nil {
			return err
		}
		if file.FileInfo().IsDir() {
			continue
		}

		if strings.HasSuffix("/"+sanitizedPath, targetSkillSuffix) {
			skillRoot := strings.TrimSuffix(sanitizedPath, "SKILL.md")
			skillRoot = strings.TrimSuffix(skillRoot, "/")
			skillArchiveRoots = append(skillArchiveRoots, skillRoot)
		}
	}

	if len(skillArchiveRoots) == 0 {
		return fmt.Errorf("skill path %q not found or missing SKILL.md", repoPath)
	}
	sort.Strings(skillArchiveRoots)
	skillRoot := skillArchiveRoots[0]

	// Extract only the target skill subtree to reduce extraction surface area.
	for _, file := range reader.File {
		sanitizedPath, err := sanitizeArchiveEntryPath(file.Name)
		if err != nil {
			return err
		}
		if sanitizedPath != skillRoot && !strings.HasPrefix(sanitizedPath, skillRoot+"/") {
			continue
		}
		if err := extractZipFile(file, tmpDir); err != nil {
			return err
		}
	}

	skillDir := filepath.Join(tmpDir, filepath.FromSlash(skillRoot))
	if err := ensureSkillManifestFile(skillDir); err != nil {
		return fmt.Errorf("extracted skill has invalid SKILL.md: %w", err)
	}

	if err := copyDir(skillDir, destPath); err != nil {
		return fmt.Errorf("failed to copy extracted skill: %w", err)
	}

	return nil
}

func sanitizeArchiveEntryPath(name string) (string, error) {
	slashPath := strings.ReplaceAll(name, "\\", "/")
	cleanPath := path.Clean(slashPath)
	if cleanPath == "." {
		return "", fmt.Errorf("invalid archive entry path: %q", name)
	}
	if strings.HasPrefix(cleanPath, "/") || cleanPath == ".." || strings.HasPrefix(cleanPath, "../") {
		return "", fmt.Errorf("invalid archive entry path: %q", name)
	}
	return cleanPath, nil
}

func extractZipFile(file *zip.File, destDir string) error {
	sanitizedPath, err := sanitizeArchiveEntryPath(file.Name)
	if err != nil {
		return err
	}

	joinedPath := filepath.Join(destDir, filepath.FromSlash(sanitizedPath))
	cleanDest := filepath.Clean(destDir)
	cleanPath := filepath.Clean(joinedPath)
	if cleanPath != cleanDest && !strings.HasPrefix(cleanPath, cleanDest+string(os.PathSeparator)) {
		// Mitigates zip-slip path traversal during extraction.
		return fmt.Errorf("invalid file path (zip-slip): %s", file.Name)
	}

	if file.FileInfo().IsDir() {
		if err := os.MkdirAll(cleanPath, file.Mode()); err != nil {
			return fmt.Errorf("failed to create directory %s: %w", cleanPath, err)
		}
		return nil
	}

	if err := os.MkdirAll(filepath.Dir(cleanPath), 0o755); err != nil {
		return fmt.Errorf("failed to create parent directory for %s: %w", cleanPath, err)
	}

	src, err := file.Open()
	if err != nil {
		return fmt.Errorf("failed to open archive file %s: %w", file.Name, err)
	}
	defer src.Close()

	dst, err := os.OpenFile(cleanPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, file.Mode())
	if err != nil {
		return fmt.Errorf("failed to create destination file %s: %w", cleanPath, err)
	}

	_, copyErr := io.Copy(dst, src)
	closeErr := dst.Close()
	if copyErr != nil {
		if closeErr != nil {
			return fmt.Errorf("failed to write file %s: %v; close error: %v", cleanPath, copyErr, closeErr)
		}
		return fmt.Errorf("failed to write file %s: %w", cleanPath, copyErr)
	}
	if closeErr != nil {
		return fmt.Errorf("failed to close file %s: %w", cleanPath, closeErr)
	}

	return nil
}

func ensureSkillManifestFile(skillDir string) error {
	if skillDir == "" {
		return fmt.Errorf("empty skill directory")
	}
	manifest := filepath.Join(skillDir, "SKILL.md")
	info, err := os.Lstat(manifest)
	if err != nil {
		return err
	}
	if info.IsDir() {
		return fmt.Errorf("manifest is a directory")
	}
	if info.Mode()&os.ModeSymlink != 0 {
		return fmt.Errorf("manifest must not be a symlink")
	}
	return nil
}

func hasSkillManifest(skillDir string) bool {
	return ensureSkillManifestFile(skillDir) == nil
}

func cacheKey(ref *Ref) string {
	source := fmt.Sprintf("%s/%s@%s:%s", ref.Owner, ref.Repo, ref.Ref, ref.Path)
	hash := sha256.Sum256([]byte(source))
	suffix := hex.EncodeToString(hash[:])[:12]
	return fmt.Sprintf("%s_%s_%s", sanitizePathPart(ref.Owner+"_"+ref.Repo), sanitizePathPart(ref.Ref+"_"+ref.Path), suffix)
}

func sanitizePathPart(s string) string {
	replacer := strings.NewReplacer(
		"/", "_",
		"\\", "_",
		":", "_",
		"@", "_",
		" ", "_",
	)
	return replacer.Replace(s)
}

func (r *Resolver) authToken() string {
	if r.token != "" {
		return r.token
	}
	if token := strings.TrimSpace(os.Getenv("GITHUB_TOKEN")); token != "" {
		return token
	}
	if token := strings.TrimSpace(os.Getenv("GH_TOKEN")); token != "" {
		return token
	}
	return ""
}

func copyDir(src, dst string) error {
	if err := os.MkdirAll(dst, 0o755); err != nil {
		return fmt.Errorf("failed to create destination directory %s: %w", dst, err)
	}

	entries, err := os.ReadDir(src)
	if err != nil {
		return fmt.Errorf("failed to read source directory %s: %w", src, err)
	}

	for _, entry := range entries {
		srcPath := filepath.Join(src, entry.Name())
		dstPath := filepath.Join(dst, entry.Name())

		if entry.Type()&os.ModeSymlink != 0 {
			return fmt.Errorf("symlink entries are not allowed in skill directory: %s", srcPath)
		}

		info, err := entry.Info()
		if err != nil {
			return fmt.Errorf("failed to stat source entry %s: %w", srcPath, err)
		}

		if info.IsDir() {
			if err := copyDir(srcPath, dstPath); err != nil {
				return err
			}
			continue
		}

		srcFile, err := os.Open(srcPath)
		if err != nil {
			return fmt.Errorf("failed to open source file %s: %w", srcPath, err)
		}

		dstFile, err := os.OpenFile(dstPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, info.Mode())
		if err != nil {
			srcCloseErr := srcFile.Close()
			if srcCloseErr != nil {
				return fmt.Errorf("failed to create destination file %s: %v (also failed to close source file: %v)", dstPath, err, srcCloseErr)
			}
			return fmt.Errorf("failed to create destination file %s: %w", dstPath, err)
		}

		_, copyErr := io.Copy(dstFile, srcFile)
		srcCloseErr := srcFile.Close()
		dstCloseErr := dstFile.Close()
		if copyErr != nil {
			if srcCloseErr != nil || dstCloseErr != nil {
				return fmt.Errorf("failed to copy file %s: %v (close source err: %v, close dest err: %v)", srcPath, copyErr, srcCloseErr, dstCloseErr)
			}
			return fmt.Errorf("failed to copy file %s: %w", srcPath, copyErr)
		}
		if srcCloseErr != nil || dstCloseErr != nil {
			return fmt.Errorf("failed to close file handles for %s (source close err: %v, destination close err: %v)", srcPath, srcCloseErr, dstCloseErr)
		}
	}

	return nil
}

func checkContextCanceled(ctx context.Context) error {
	if ctx == nil {
		return nil
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
		return nil
	}
}

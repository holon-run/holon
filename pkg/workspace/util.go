package workspace

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// MkdirTempOutsideWorkspace creates a temporary directory outside of the given path
// This is important to ensure the snapshot directory can be cleaned up independently
func MkdirTempOutsideWorkspace(workspace, pattern string) (string, error) {
	absWorkspace, err := cleanAbs(workspace)
	if err != nil {
		return "", err
	}

	var baseCandidates []string
	if v := strings.TrimSpace(os.Getenv("HOLON_SNAPSHOT_BASE")); v != "" {
		baseCandidates = append(baseCandidates, v)
	}
	baseCandidates = append(baseCandidates, os.TempDir())

	if cacheDir, err := os.UserCacheDir(); err == nil && cacheDir != "" {
		baseCandidates = append(baseCandidates, filepath.Join(cacheDir, "holon"))
	}

	// Parent directory is a good, usually writable, fallback.
	baseCandidates = append(baseCandidates, filepath.Dir(absWorkspace))

	if runtime.GOOS != "windows" {
		baseCandidates = append(baseCandidates, "/tmp")
	}

	var lastErr error
	for _, base := range baseCandidates {
		if strings.TrimSpace(base) == "" {
			continue
		}
		absBase, err := cleanAbs(base)
		if err != nil {
			lastErr = err
			continue
		}
		if isSubpath(absBase, absWorkspace) {
			continue
		}
		if err := os.MkdirAll(absBase, 0o755); err != nil {
			lastErr = err
			continue
		}
		dir, err := os.MkdirTemp(absBase, pattern)
		if err != nil {
			lastErr = err
			continue
		}
		return dir, nil
	}

	if lastErr != nil {
		return "", lastErr
	}
	return "", fmt.Errorf("unable to create temp dir outside workspace %q", absWorkspace)
}

// cleanAbs returns the absolute path, resolving symlinks if possible
func cleanAbs(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	if resolved, err := filepath.EvalSymlinks(abs); err == nil {
		return resolved, nil
	}
	return abs, nil
}

// isSubpath checks if candidate is a subpath of parent
func isSubpath(candidate, parent string) bool {
	rel, err := filepath.Rel(parent, candidate)
	if err != nil {
		return false
	}
	rel = filepath.Clean(rel)
	return rel == "." || !strings.HasPrefix(rel, "..")
}

// copyDir copies a directory recursively using cp -a (Unix) or xcopy (Windows)
func copyDir(src string, dst string) error {
	if runtime.GOOS == "windows" {
		// Windows: Use xcopy for recursive directory copy
		cmd := exec.Command("xcopy", src+"\\*", dst, "/E", "/I", "/H", "/Y", "/Q")
		if out, err := cmd.CombinedOutput(); err != nil {
			return fmt.Errorf("xcopy failed: %v, output: %s", err, string(out))
		}
		return nil
	}
	// Unix: Use cp -a for recursive copy with attributes preserved
	cmd := exec.Command("cp", "-a", src+"/.", dst+"/")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("cp failed: %v, output: %s", err, string(out))
	}
	return nil
}

// IsGitRepo checks if the given directory is inside a git repository
func IsGitRepo(dir string) bool {
	cmd := exec.Command("git", "-C", dir, "rev-parse", "--git-dir")
	if err := cmd.Run(); err != nil {
		return false
	}
	return true
}

// IsGitRepoContext checks if the given directory is inside a git repository with context support
func IsGitRepoContext(ctx context.Context, dir string) bool {
	cmd := exec.CommandContext(ctx, "git", "-C", dir, "rev-parse", "--git-dir")
	if err := cmd.Run(); err != nil {
		return false
	}
	return true
}

// getHeadSHA returns the current HEAD SHA of a git repository
func getHeadSHA(dir string) (string, error) {
	cmd := exec.Command("git", "-C", dir, "rev-parse", "HEAD")
	output, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("failed to get HEAD SHA: %w", err)
	}
	return strings.TrimSpace(string(output)), nil
}

// getHeadSHAContext returns the current HEAD SHA of a git repository with context support
func getHeadSHAContext(ctx context.Context, dir string) (string, error) {
	cmd := exec.CommandContext(ctx, "git", "-C", dir, "rev-parse", "HEAD")
	output, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("failed to get HEAD SHA: %w", err)
	}
	return strings.TrimSpace(string(output)), nil
}

// isShallowClone checks if a git repository is a shallow clone
func isShallowClone(dir string) bool {
	cmd := exec.Command("git", "-C", dir, "rev-parse", "--is-shallow-repository")
	output, err := cmd.Output()
	if err != nil {
		return false
	}
	return strings.TrimSpace(string(output)) == "true"
}

// isShallowCloneContext checks if a git repository is a shallow clone with context support
func isShallowCloneContext(ctx context.Context, dir string) bool {
	cmd := exec.CommandContext(ctx, "git", "-C", dir, "rev-parse", "--is-shallow-repository")
	output, err := cmd.Output()
	if err != nil {
		return false
	}
	return strings.TrimSpace(string(output)) == "true"
}

// checkoutRef checks out a git reference in a repository
func checkoutRef(dir, ref string) error {
	args := []string{"-C", dir, "checkout", "--quiet"}
	if ref != "" {
		args = append(args, ref)
	}
	cmd := exec.Command("git", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("failed to checkout ref %s: %v, output: %s", ref, err, string(out))
	}
	return nil
}

// checkoutRefContext checks out a git reference in a repository with context support
func checkoutRefContext(ctx context.Context, dir, ref string) error {
	args := []string{"-C", dir, "checkout", "--quiet"}
	if ref != "" {
		args = append(args, ref)
	}
	cmd := exec.CommandContext(ctx, "git", args...)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("failed to checkout ref %s: %v, output: %s", ref, err, string(out))
	}
	return nil
}

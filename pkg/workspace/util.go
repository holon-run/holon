package workspace

import (
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
// Returns early if src and dst are the same path to prevent self-copy truncation
func copyDir(src string, dst string) error {
	// Guard against self-copy: if src and dst resolve to the same path, skip copy
	// This prevents truncation bugs when copying a directory onto itself
	srcAbs, err := cleanAbs(src)
	if err != nil {
		return fmt.Errorf("failed to resolve source path: %w", err)
	}
	dstAbs, err := cleanAbs(dst)
	if err != nil {
		return fmt.Errorf("failed to resolve destination path: %w", err)
	}
	if srcAbs == dstAbs {
		return nil
	}

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

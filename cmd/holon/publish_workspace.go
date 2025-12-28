package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/holon-run/holon/pkg/git"
	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/workspace"
)

// publishWorkspace describes a prepared workspace for publishing.
type publishWorkspace struct {
	path    string
	cleanup func()
}

// preparePublishWorkspace restores a clean workspace for publishing based on the
// workspace manifest written during execution. It prefers creating a git
// worktree from the original source repository, falling back to an error if the
// source is not a git repository (publishing requires git).
func preparePublishWorkspace(ctx context.Context, outDir string) (*publishWorkspace, error) {
	manifest, err := workspace.ReadManifest(outDir)
	if err != nil {
		return nil, fmt.Errorf("failed to read workspace manifest: %w", err)
	}

	if manifest == nil || manifest.Source == "" {
		return nil, fmt.Errorf("workspace manifest is missing source; cannot prepare publish workspace")
	}

	sourceValue := manifest.Source

	ref := manifest.HeadSHA
	if ref == "" {
		ref = manifest.Ref
	}
	if ref == "" {
		ref = "HEAD"
	}

	// If the source is a git URL, clone a clean workspace first.
	if isGitURL(sourceValue) {
		tempDir, err := os.MkdirTemp("", "holon-publish-clone-*")
		if err != nil {
			return nil, fmt.Errorf("failed to create temp publish workspace: %w", err)
		}

		depth := 0
		if manifest.IsShallow {
			depth = 1
		}

		_, err = git.Clone(ctx, git.CloneOptions{
			Source: sourceValue,
			Dest:   tempDir,
			Ref:    ref,
			Depth:  depth,
			Quiet:  true,
		})
		if err != nil {
			os.RemoveAll(tempDir)
			return nil, fmt.Errorf("failed to clone publish workspace: %w", err)
		}

		cleanup := func() {
			if err := os.RemoveAll(tempDir); err != nil {
				holonlog.Warn("failed to clean publish clone", "path", tempDir, "error", err)
			}
		}

		return &publishWorkspace{path: tempDir, cleanup: cleanup}, nil
	}

	sourcePath, err := filepath.Abs(sourceValue)
	if err != nil {
		return nil, fmt.Errorf("failed to resolve manifest source path: %w", err)
	}

	// If the source is a git repo, create a worktree for a clean publish base.
	if workspace.IsGitRepo(sourcePath) {
		tempDir, err := os.MkdirTemp("", "holon-publish-worktree-*")
		if err != nil {
			return nil, fmt.Errorf("failed to create temp publish workspace: %w", err)
		}

		client := git.NewClient(sourcePath)
		if err := client.AddWorktree(ctx, tempDir, ref, true); err != nil {
			os.RemoveAll(tempDir)
			return nil, fmt.Errorf("failed to create publish worktree: %w", err)
		}

		cleanup := func() {
			if err := client.RemoveWorktree(context.Background(), tempDir, true); err != nil {
				holonlog.Warn("failed to remove publish worktree", "path", tempDir, "error", err)
			}
			if err := os.RemoveAll(tempDir); err != nil {
				holonlog.Warn("failed to clean publish worktree", "path", tempDir, "error", err)
			}
		}

		return &publishWorkspace{path: tempDir, cleanup: cleanup}, nil
	}

	return nil, fmt.Errorf("publish requires a git workspace; manifest source %q is not a git repository", sourcePath)
}

func isGitURL(src string) bool {
	if src == "" {
		return false
	}
	lower := strings.ToLower(src)
	return strings.HasPrefix(lower, "http://") ||
		strings.HasPrefix(lower, "https://") ||
		strings.HasPrefix(lower, "git@") ||
		strings.HasPrefix(lower, "ssh://") ||
		strings.HasPrefix(lower, "file://")
}

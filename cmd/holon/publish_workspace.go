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

	// Prefer the live HOLON_WORKSPACE (agent runtime workspace) when available.
	if liveWS := strings.TrimSpace(os.Getenv("HOLON_WORKSPACE")); liveWS != "" && workspace.IsGitRepo(liveWS) {
		ref := manifest.HeadSHA
		if ref == "" {
			ref = manifest.Ref
		}
		if ref == "" {
			ref = "HEAD"
		}
		ws, err := newWorktreeFromLocal(ctx, liveWS, ref)
		if err == nil {
			return ws, nil
		}
		holonlog.Warn("failed to prepare publish workspace from HOLON_WORKSPACE, falling back to manifest source", "error", err)
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
		return newClonePublishWorkspace(ctx, sourceValue, ref)
	}

	sourcePath, err := filepath.Abs(sourceValue)
	if err != nil {
		return nil, fmt.Errorf("failed to resolve manifest source path: %w", err)
	}

	// If the source is a git repo, create a worktree for a clean publish base.
	if workspace.IsGitRepo(sourcePath) {
		ws, err := newWorktreeFromLocal(ctx, sourcePath, ref)
		if err == nil {
			return ws, nil
		}
		holonlog.Warn("failed to prepare publish workspace from local source, falling back to clone", "error", err)
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

func newClonePublishWorkspace(ctx context.Context, sourceValue, ref string) (*publishWorkspace, error) {
	tempDir, err := os.MkdirTemp("", "holon-publish-clone-*")
	if err != nil {
		return nil, fmt.Errorf("failed to create temp publish workspace: %w", err)
	}

	_, err = git.Clone(ctx, git.CloneOptions{
		Source: sourceValue,
		Dest:   tempDir,
		Ref:    ref,
		// Always fetch full history for publish to ensure the referenced commit/ref exists.
		Depth: 0,
		Quiet: true,
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

func newWorktreeFromLocal(ctx context.Context, sourcePath, ref string) (*publishWorkspace, error) {
	tempDir, err := os.MkdirTemp("", "holon-publish-worktree-*")
	if err != nil {
		return nil, fmt.Errorf("failed to create temp publish workspace: %w", err)
	}

	client := git.NewClient(sourcePath)

	// Ensure the repository has a HEAD commit before creating a worktree.
	if _, err := client.GetHeadSHA(ctx); err != nil {
		os.RemoveAll(tempDir)
		return nil, fmt.Errorf("failed to read HEAD in source repo: %w", err)
	}

	// For shallow repos, try to fetch/deepen so the ref is available.
	if shallow, _ := client.IsShallowClone(ctx); shallow {
		// Best effort: fetch the specific ref; if it fails, continue and let AddWorktree surface error.
		_, _ = client.ExecCommand(ctx, "fetch", "--deepen=100", "--update-shallow")
		if ref != "" && ref != "HEAD" {
			_, _ = client.ExecCommand(ctx, "fetch", "origin", ref)
		}
	}

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

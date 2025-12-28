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
// workspace manifest written during execution. It prefers cloning from the live
// HOLON_WORKSPACE or manifest source to avoid worktree-related issues with go-git.
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
		holonlog.Info("preparing publish workspace via clone (git url)", "source", sourceValue, "ref", ref)
		return newClonePublishWorkspace(ctx, sourceValue, ref, false)
	}

	sourcePath, err := filepath.Abs(sourceValue)
	if err != nil {
		return nil, fmt.Errorf("failed to resolve manifest source path: %w", err)
	}

	// Prefer the live HOLON_WORKSPACE (agent runtime workspace) when available.
	if liveWS := strings.TrimSpace(os.Getenv("HOLON_WORKSPACE")); liveWS != "" && workspace.IsGitRepo(liveWS) {
		holonlog.Info("preparing publish workspace from HOLON_WORKSPACE via local clone", "source", liveWS, "ref", ref)
		ws, err := newClonePublishWorkspace(ctx, liveWS, ref, true)
		if err == nil {
			return ws, nil
		}
		holonlog.Warn("failed to prepare publish workspace from HOLON_WORKSPACE, falling back to manifest source", "error", err)
	}

	// If the source is a git repo, clone it locally for a clean publish base.
	if workspace.IsGitRepo(sourcePath) {
		holonlog.Info("preparing publish workspace from manifest source via local clone", "source", sourcePath, "ref", ref)
		ws, err := newClonePublishWorkspace(ctx, sourcePath, ref, true)
		if err == nil {
			return ws, nil
		}
		holonlog.Warn("failed to prepare publish workspace from local source, falling back to clone", "error", err)
	}

	// Fall back to cloning the manifest source as a URL/path (non-git sources will error).
	holonlog.Info("preparing publish workspace via clone (fallback)", "source", sourceValue, "ref", ref)
	return newClonePublishWorkspace(ctx, sourceValue, ref, false)
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

// deriveGitHubURL best-effort converts a path or owner/repo string to a https GitHub URL.
func deriveGitHubURL(src string) string {
	trimmed := strings.TrimSpace(src)
	if trimmed == "" {
		return ""
	}
	// Already a URL
	if strings.HasPrefix(trimmed, "http://") || strings.HasPrefix(trimmed, "https://") || strings.HasPrefix(trimmed, "git@") {
		return trimmed
	}
	parts := strings.Split(trimmed, "/")
	if len(parts) >= 2 {
		owner := parts[len(parts)-2]
		repo := strings.TrimSuffix(parts[len(parts)-1], ".git")
		if owner != "" && repo != "" {
			return fmt.Sprintf("https://github.com/%s/%s.git", owner, repo)
		}
	}
	return ""
}

func newClonePublishWorkspace(ctx context.Context, sourceValue, ref string, useLocal bool) (*publishWorkspace, error) {
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
		Local: useLocal,
	})
	if err != nil {
		os.RemoveAll(tempDir)
		return nil, fmt.Errorf("failed to clone publish workspace: %w", err)
	}

	// If we cloned locally, restore a usable origin remote so push/pr works.
	var originURL string
	if useLocal {
		sourceClient := git.NewClient(sourceValue)
		originURL, _ = sourceClient.ConfigGet(ctx, "remote.origin.url")
		if originURL == "" {
			// Best effort: derive from manifest source path if it looks like owner/repo.
			if derived := deriveGitHubURL(sourceValue); derived != "" {
				originURL = derived
			}
		}
		if originURL != "" {
			targetClient := git.NewClient(tempDir)
			if err := targetClient.SetRemote(ctx, "origin", originURL); err != nil {
				holonlog.Warn("failed to set origin on publish workspace", "path", tempDir, "origin", originURL, "error", err)
			}
		}
	}

	// Ensure publish workspace is not shallow; unshallow if possible.
	targetClient := git.NewClient(tempDir)
	if shallow, _ := targetClient.IsShallowClone(ctx); shallow {
		if originURL == "" {
			return nil, fmt.Errorf("publish workspace is shallow and origin is unknown; cannot fetch full history")
		}
		holonlog.Info("publish workspace is shallow, attempting to fetch full history", "origin", originURL)
		if _, err := targetClient.ExecCommand(ctx, "fetch", "--unshallow"); err != nil {
			return nil, fmt.Errorf("publish workspace is shallow; failed to unshallow from origin %s: %w", originURL, err)
		}
		if ref != "" && ref != "HEAD" {
			_, _ = targetClient.ExecCommand(ctx, "fetch", "origin", ref)
		}
	}

	cleanup := func() {
		if err := os.RemoveAll(tempDir); err != nil {
			holonlog.Warn("failed to clean publish clone", "path", tempDir, "error", err)
		}
	}

	return &publishWorkspace{path: tempDir, cleanup: cleanup}, nil
}

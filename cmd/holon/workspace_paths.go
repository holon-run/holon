package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const defaultWorkspaceTrack = "default"

func resolveAgentWorkspaceRoot(agentHome string) (string, error) {
	root := filepath.Join(agentHome, "workspaces")
	if err := os.MkdirAll(root, 0755); err != nil {
		return "", fmt.Errorf("failed to create workspace root at %s: %w", root, err)
	}
	return root, nil
}

func resolveRunWorkspacePath(agentHome, requested string) (string, error) {
	if strings.TrimSpace(requested) != "" {
		abs, err := filepath.Abs(requested)
		if err != nil {
			return "", fmt.Errorf("failed to resolve workspace path: %w", err)
		}
		return abs, nil
	}
	root, err := resolveAgentWorkspaceRoot(agentHome)
	if err != nil {
		return "", err
	}
	path := filepath.Join(root, "run", defaultWorkspaceTrack)
	if err := os.MkdirAll(path, 0755); err != nil {
		return "", fmt.Errorf("failed to create run workspace at %s: %w", path, err)
	}
	return path, nil
}

func workspacePathForRepoRef(root, repo, ref string) (string, string, error) {
	repo = strings.TrimSpace(repo)
	if repo == "" {
		return "", "", fmt.Errorf("repo is required")
	}
	owner, name, ok := strings.Cut(repo, "/")
	if !ok || strings.TrimSpace(owner) == "" || strings.TrimSpace(name) == "" {
		return "", "", fmt.Errorf("invalid repo format %q (expected owner/repo)", repo)
	}

	track := sanitizeWorkspaceSegment(ref)
	if track == "" {
		track = defaultWorkspaceTrack
	}
	path := filepath.Join(root, "repos", sanitizeWorkspaceSegment(owner), sanitizeWorkspaceSegment(name), track)
	workspaceRef := fmt.Sprintf("%s@%s", repo, track)
	return path, workspaceRef, nil
}

func sanitizeWorkspaceSegment(value string) string {
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		return ""
	}
	trimmed = strings.Trim(trimmed, "/")
	if trimmed == "" {
		return ""
	}
	replacer := strings.NewReplacer("/", "_", "\\", "_", ":", "_", " ", "_")
	return replacer.Replace(trimmed)
}

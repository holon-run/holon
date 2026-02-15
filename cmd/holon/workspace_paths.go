package main

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

const defaultWorkspaceTrack = "default"
const maxWorkspaceSegmentLength = 120

var nonWorkspaceSegmentChars = regexp.MustCompile(`[^a-zA-Z0-9._-]`)

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

	safeOwner := sanitizeWorkspaceSegment(owner)
	safeName := sanitizeWorkspaceSegment(name)
	if safeOwner == "" || safeName == "" {
		return "", "", fmt.Errorf("invalid repo format %q after sanitization", repo)
	}

	track := sanitizeWorkspaceSegment(ref)
	if track == "" {
		track = defaultWorkspaceTrack
	}
	path := filepath.Join(root, "repos", safeOwner, safeName, track)
	absRoot, err := filepath.Abs(root)
	if err != nil {
		return "", "", fmt.Errorf("failed to resolve workspace root: %w", err)
	}
	absPath, err := filepath.Abs(path)
	if err != nil {
		return "", "", fmt.Errorf("failed to resolve workspace path: %w", err)
	}
	rel, err := filepath.Rel(absRoot, absPath)
	if err != nil {
		return "", "", fmt.Errorf("failed to validate workspace path under root: %w", err)
	}
	if rel == ".." || strings.HasPrefix(rel, ".."+string(os.PathSeparator)) {
		return "", "", fmt.Errorf("resolved workspace path %s escapes root %s", absPath, absRoot)
	}
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
	normalized := nonWorkspaceSegmentChars.ReplaceAllString(trimmed, "_")
	normalized = strings.TrimSpace(normalized)
	if normalized == "" || normalized == "." || normalized == ".." {
		return ""
	}
	if len(normalized) > maxWorkspaceSegmentLength {
		normalized = normalized[:maxWorkspaceSegmentLength]
	}
	return normalized
}

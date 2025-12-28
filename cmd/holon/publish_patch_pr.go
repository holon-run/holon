package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/holon-run/holon/pkg/publisher"
	pubgit "github.com/holon-run/holon/pkg/publisher/git"
)

// publishPatchToPR applies/commits/pushes diff.patch to the PR head branch using the git publisher.
// It reads pr.json from the *input* context to discover the head branch. The publish workspace
// (pubWorkspace) should already be prepared and pointed to by HOLON_WORKSPACE.
func publishPatchToPR(ctx context.Context, pubWorkspace, inputDir, diffPath string) error {
	prJSON := filepath.Join(inputDir, "context", "github", "pr.json")
	data, err := os.ReadFile(prJSON)
	if err != nil {
		return fmt.Errorf("failed to read pr.json for patch publish: %w", err)
	}

	var prInfo struct {
		Head struct {
			Ref string `json:"ref"`
		} `json:"head"`
		HeadRef string `json:"head_ref"`
		PR      struct {
			HeadRef string `json:"head_ref"`
			Head    struct {
				Ref string `json:"ref"`
			} `json:"head"`
		} `json:"pr"`
	}
	if err := json.Unmarshal(data, &prInfo); err != nil {
		return fmt.Errorf("failed to parse pr.json: %w", err)
	}
	headRef := prInfo.Head.Ref
	if headRef == "" {
		headRef = prInfo.HeadRef
	}
	if headRef == "" {
		headRef = prInfo.PR.Head.Ref
	}
	if headRef == "" {
		headRef = prInfo.PR.HeadRef
	}
	if headRef == "" {
		return fmt.Errorf("pr.json missing head.ref; cannot determine target branch for patch publish (looked in %s)", prJSON)
	}

	// Build a PublishRequest for git publisher.
	req := publisher.PublishRequest{
		Target:    "origin/" + headRef,
		OutputDir: outDir,
		Manifest: map[string]interface{}{
			"metadata": map[string]interface{}{
				"branch": headRef,
				"commit": true,
				"push":   true,
			},
		},
		Artifacts: map[string]string{
			"diff.patch": diffPath,
		},
	}

	gitPub := pubgit.NewPublisher()

	// Ensure git publisher has a token if available.
	if os.Getenv(pubgit.GitTokenEnv) == "" {
		if tok := os.Getenv("GITHUB_TOKEN"); tok != "" {
			_ = os.Setenv(pubgit.GitTokenEnv, tok)
		} else if tok := os.Getenv("GH_TOKEN"); tok != "" {
			_ = os.Setenv(pubgit.GitTokenEnv, tok)
		}
	}

	if err := gitPub.Validate(req); err != nil {
		return fmt.Errorf("git publish validation failed: %w", err)
	}

	if _, err := gitPub.Publish(req); err != nil {
		return fmt.Errorf("git publish failed: %w", err)
	}

	return nil
}

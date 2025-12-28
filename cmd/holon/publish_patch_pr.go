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
// It reads pr.json from the input context to discover the head branch. The publish workspace
// (pubWorkspace) should already be prepared and pointed to by HOLON_WORKSPACE.
func publishPatchToPR(ctx context.Context, pubWorkspace, outDir, diffPath string) error {
	prJSON := filepath.Join(outDir, "context", "github", "pr.json")
	data, err := os.ReadFile(prJSON)
	if err != nil {
		return fmt.Errorf("failed to read pr.json for patch publish: %w", err)
	}

	var prInfo struct {
		Head struct {
			Ref string `json:"ref"`
		} `json:"head"`
	}
	if err := json.Unmarshal(data, &prInfo); err != nil {
		return fmt.Errorf("failed to parse pr.json: %w", err)
	}
	headRef := prInfo.Head.Ref
	if headRef == "" {
		return fmt.Errorf("pr.json missing head.ref; cannot determine target branch for patch publish")
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
	if err := gitPub.Validate(req); err != nil {
		return fmt.Errorf("git publish validation failed: %w", err)
	}

	if _, err := gitPub.Publish(req); err != nil {
		return fmt.Errorf("git publish failed: %w", err)
	}

	return nil
}

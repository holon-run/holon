package githubpr

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"time"

	hghelper "github.com/holon-run/holon/pkg/github"
	"github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/publisher"
)

const (
	// TokenEnv is the environment variable for GitHub token.
	TokenEnv = "GITHUB_TOKEN"

	// LegacyTokenEnv is the legacy environment variable for GitHub token.
	LegacyTokenEnv = "HOLON_GITHUB_TOKEN"

	// BranchFlag is the flag name for branch configuration.
	BranchFlag = "branch"

	// TitleFlag is the flag name for PR title configuration.
	TitleFlag = "title"

	// IssueFlag is the flag name for issue ID reference.
	IssueFlag = "issue_id"
)

// githubContextInfo represents the context information collected from GitHub
type githubContextInfo struct {
	// Provider is the context provider (e.g., "github")
	Provider string `json:"provider"`

	// Kind is the context type ("issue" or "pr")
	Kind string `json:"kind"`

	// Ref is the reference that was collected
	Ref string `json:"ref"`

	// Owner is the repository owner
	Owner string `json:"owner"`

	// Repo is the repository name
	Repo string `json:"repo"`

	// Number is the issue or PR number
	Number int `json:"number"`
}

// issueInfo represents minimal issue information from context
type issueInfo struct {
	Number int    `json:"number"`
	Title  string `json:"title"`
}

// prInfo represents minimal PR information from context
type prInfo struct {
	Number int    `json:"number"`
	Title  string `json:"title"`
}

// PRPublisher publishes Holon outputs as GitHub PRs.
type PRPublisher struct{}

// NewPRPublisher creates a new PR publisher instance.
func NewPRPublisher() *PRPublisher {
	return &PRPublisher{}
}

// Name returns the provider name.
func (p *PRPublisher) Name() string {
	return "github-pr"
}

// Validate checks if the request is valid for this publisher.
func (p *PRPublisher) Validate(req publisher.PublishRequest) error {
	// Parse the repository reference
	prRef, err := ParsePRRef(req.Target)
	if err != nil {
		return fmt.Errorf("invalid target format: %w", err)
	}

	// Validate PR reference fields
	if prRef.Owner == "" || prRef.Repo == "" {
		return fmt.Errorf("incomplete repository reference: owner=%s, repo=%s", prRef.Owner, prRef.Repo)
	}

	// Check for required artifacts
	patchPath := req.Artifacts["diff.patch"]
	if patchPath == "" {
		return fmt.Errorf("required artifact 'diff.patch' not found")
	}

	summaryPath := req.Artifacts["summary.md"]
	if summaryPath == "" {
		return fmt.Errorf("required artifact 'summary.md' not found")
	}

	return nil
}

// Publish sends Holon outputs to GitHub as a PR.
func (p *PRPublisher) Publish(req publisher.PublishRequest) (publisher.PublishResult, error) {
	ctx := context.Background()

	// Create GitHub client using shared helper (reads token from env vars)
	githubClient, err := hghelper.NewClientFromEnv()
	if err != nil {
		return publisher.PublishResult{}, fmt.Errorf("failed to create GitHub client: %w", err)
	}

	// Parse repository reference
	prRef, err := ParsePRRef(req.Target)
	if err != nil {
		return publisher.PublishResult{}, fmt.Errorf("invalid target: %w", err)
	}

	// Get workspace directory (required for git operations)
	workspaceDir := os.Getenv("HOLON_WORKSPACE")
	if workspaceDir == "" {
		// Fall back to current directory
		workspaceDir = "."
	}

	// Step 0: Generate deterministic title from context if not already in manifest
	// This ensures PR titles are derived from context (issue/PR metadata) rather than LLM output
	deterministicTitle, err := p.generateDeterministicTitle(req)
	if err != nil {
		// Non-fatal: log and continue with existing title logic
		fmt.Fprintf(os.Stderr, "Warning: failed to generate deterministic title: %v\n", err)
	} else if deterministicTitle != "" {
		// Inject the deterministic title into the manifest metadata
		rawMetadata := req.Manifest["metadata"]
		metadata, ok := rawMetadata.(map[string]interface{})
		if !ok || metadata == nil {
			metadata = make(map[string]interface{})
			req.Manifest["metadata"] = metadata
		}
		if _, exists := metadata[TitleFlag]; !exists {
			metadata[TitleFlag] = deterministicTitle
			// Write the updated manifest back to disk so it persists
			if err := p.writeManifestWithTitle(req.OutputDir, req.Manifest); err != nil {
				// Non-fatal: log and continue
				fmt.Fprintf(os.Stderr, "Warning: failed to write manifest with title: %v\n", err)
			}
		}
	}

	// Build configuration from manifest metadata (now with deterministic title if generated)
	config := p.buildConfig(req.Manifest)

	// Initialize result
	result := publisher.PublishResult{
		Provider: p.Name(),
		Target:   req.Target,
		Actions:  []publisher.PublishAction{},
		Errors:   []publisher.PublishError{},
		Success:  true,
	}

	// Step 1: Read summary for PR content
	summaryPath := req.Artifacts["summary.md"]
	summaryContent, err := os.ReadFile(summaryPath)
	if err != nil {
		wrappedErr := fmt.Errorf("failed to read summary.md: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}
	summary := string(summaryContent)

	// Derive PR content from summary if not explicitly configured
	if config.Title == "" {
		config.Title = ExtractTitleFromSummary(summary)
	}
	if config.CommitMessage == "" {
		config.CommitMessage = config.Title
	}
	if config.BranchName == "" {
		config.BranchName = ExtractBranchFromSummary(summary, config.IssueID)
	}

	// Step 2: Initialize Git client
	token := githubClient.GetToken()
	gitClient := NewGitClient(workspaceDir, token)

	// Step 3: Create or checkout branch FIRST (before applying patch)
	// This prevents Checkout from discarding patch-applied files
	if err := gitClient.CreateBranch(ctx, config.BranchName); err != nil {
		wrappedErr := fmt.Errorf("failed to create branch: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	result.Actions = append(result.Actions, publisher.PublishAction{
		Type:        "created_branch",
		Description: fmt.Sprintf("Created branch: %s", config.BranchName),
		Metadata: map[string]string{
			"branch": config.BranchName,
		},
	})

	// Step 4: Apply patch AFTER creating branch
	patchPath := req.Artifacts["diff.patch"]
	if err := gitClient.ApplyPatch(ctx, patchPath); err != nil {
		wrappedErr := fmt.Errorf("failed to apply patch: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	result.Actions = append(result.Actions, publisher.PublishAction{
		Type:        "applied_patch",
		Description: "Applied diff.patch to workspace",
	})

	// Step 5: Commit changes
	commitHash, err := gitClient.CommitChanges(ctx, config.CommitMessage)
	if err != nil {
		wrappedErr := fmt.Errorf("failed to commit changes: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	result.Actions = append(result.Actions, publisher.PublishAction{
		Type:        "created_commit",
		Description: fmt.Sprintf("Committed changes: %s", commitHash),
		Metadata: map[string]string{
			"commit": commitHash,
		},
	})

	// Step 6: Push branch
	if err := gitClient.Push(config.BranchName); err != nil {
		wrappedErr := fmt.Errorf("failed to push branch: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	result.Actions = append(result.Actions, publisher.PublishAction{
		Type:        "pushed_branch",
		Description: fmt.Sprintf("Pushed branch to remote: %s", config.BranchName),
		Metadata: map[string]string{
			"branch": config.BranchName,
		},
	})

	// Step 7: Create or update PR via GitHub API using the helper
	prBody := FormatPRBody(summary, config.IssueID)

	// Check if PR already exists for this branch
	existingPR, err := p.findPRByBranch(ctx, githubClient, *prRef, config.BranchName)
	if err != nil {
		wrappedErr := fmt.Errorf("failed to check for existing PR: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	if existingPR != nil {
		// Update existing PR using the helper
		updatedPR, err := githubClient.UpdatePullRequest(ctx, prRef.Owner, prRef.Repo, existingPR.Number, config.Title, prBody)
		if err != nil {
			wrappedErr := fmt.Errorf("failed to update PR: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
			return result, wrappedErr
		}

		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "updated_pr",
			Description: fmt.Sprintf("Updated PR #%d", updatedPR.Number),
			Metadata: map[string]string{
				"pr_number": strconv.FormatInt(int64(updatedPR.Number), 10),
				"pr_url":    updatedPR.URL,
			},
		})
	} else {
		// Create new PR using the helper
		newPR, err := githubClient.CreatePullRequest(ctx, prRef.Owner, prRef.Repo, &hghelper.NewPullRequest{
			Title:               config.Title,
			Head:                config.BranchName,
			Base:                prRef.BaseBranch,
			Body:                prBody,
			MaintainerCanModify: true,
		})
		if err != nil {
			wrappedErr := fmt.Errorf("failed to create PR: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
			return result, wrappedErr
		}

		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "created_pr",
			Description: fmt.Sprintf("Created PR #%d", newPR.Number),
			Metadata: map[string]string{
				"pr_number": strconv.FormatInt(int64(newPR.Number), 10),
				"pr_url":    newPR.URL,
			},
		})
	}

	result.PublishedAt = time.Now()
	return result, nil
}

// findPRByBranch finds an existing PR for the given head branch.
func (p *PRPublisher) findPRByBranch(ctx context.Context, client *hghelper.Client, prRef PRRef, branchName string) (*hghelper.PRInfo, error) {
	// Use the helper to list open PRs
	prs, err := client.ListPullRequests(ctx, prRef.Owner, prRef.Repo, "open")
	if err != nil {
		return nil, fmt.Errorf("failed to list PRs: %w", err)
	}

	// Search for PR with matching head branch
	for _, pr := range prs {
		if pr.HeadRef == branchName {
			return pr, nil
		}
	}

	return nil, nil
}

// buildConfig builds publisher configuration from manifest metadata.
func (p *PRPublisher) buildConfig(manifest map[string]interface{}) PRPublisherConfig {
	config := PRPublisherConfig{}

	// Extract configuration from manifest metadata
	if metadata, ok := manifest["metadata"].(map[string]interface{}); ok {
		if branch, ok := metadata[BranchFlag].(string); ok {
			config.BranchName = branch
		}
		if title, ok := metadata[TitleFlag].(string); ok {
			config.Title = title
		}
		if issueID, ok := metadata[IssueFlag].(string); ok {
			config.IssueID = issueID
		}
	}

	// Extract from goal if available
	if goal, ok := manifest["goal"].(map[string]interface{}); ok {
		if issueID, ok := goal["issue_id"].(string); ok && config.IssueID == "" {
			config.IssueID = issueID
		}
	}

	return config
}

// generateDeterministicTitle generates a deterministic PR title from context files.
// Returns empty string if context is not available or title cannot be generated.
func (p *PRPublisher) generateDeterministicTitle(req publisher.PublishRequest) (string, error) {
	// Check if title is already set in manifest metadata
	if metadata, ok := req.Manifest["metadata"].(map[string]interface{}); ok {
		if title, ok := metadata[TitleFlag].(string); ok && title != "" {
			// Title already exists, don't override
			return "", nil
		}
	}

	// Determine context directory path: use InputDir/context
	var contextDir string
	var contextSource string // Tracks where context was found for logging

	if req.InputDir != "" {
		contextDir = filepath.Join(req.InputDir, "context")
		contextSource = fmt.Sprintf("InputDir (%s)", contextDir)
	} else {
		// For backward compatibility with direct `holon publish` usage
		contextDir = filepath.Join(req.OutputDir, "context")
		contextSource = fmt.Sprintf("OutputDir (%s)", contextDir)
	}

	// Try to read context manifest to determine context type
	contextManifestPath := filepath.Join(contextDir, "manifest.json")
	var contextInfo githubContextInfo

	contextData, err := os.ReadFile(contextManifestPath)
	if err != nil {
		// No context manifest available, return empty title (will use fallback)
		log.Warn("Context manifest not found, skipping deterministic title generation", "path", contextManifestPath)
		return "", nil
	}

	if err := json.Unmarshal(contextData, &contextInfo); err != nil {
		return "", fmt.Errorf("failed to parse context manifest: %w", err)
	}

	// Only generate deterministic title for GitHub context
	if contextInfo.Provider != "github" {
		return "", nil
	}

	// Log where context was found for debugging
	log.Debug("Found context for deterministic title generation", "source", contextSource, "kind", contextInfo.Kind)

	// Generate title based on context kind
	switch contextInfo.Kind {
	case "issue":
		return p.generateIssueTitle(contextDir)
	case "pr":
		return p.generatePRFixTitle(contextDir)
	default:
		return "", nil
	}
}

// generateIssueTitle generates a deterministic title for issue-based PRs.
// Format: "Fix: <issue title>"
// contextDir is the context directory (e.g., inputDir/context or outputDir/context)
func (p *PRPublisher) generateIssueTitle(contextDir string) (string, error) {
	issuePath := filepath.Join(contextDir, "github", "issue.json")
	data, err := os.ReadFile(issuePath)
	if err != nil {
		return "", fmt.Errorf("failed to read issue.json: %w", err)
	}

	var info issueInfo
	if err := json.Unmarshal(data, &info); err != nil {
		return "", fmt.Errorf("failed to parse issue.json: %w", err)
	}

	if info.Title == "" {
		return "", fmt.Errorf("issue title is empty")
	}

	return fmt.Sprintf("Fix: %s", info.Title), nil
}

// generatePRFixTitle generates a deterministic title for PR-fix mode PRs.
// Format: "Address review comments on #<pr_number>: <pr title>"
// contextDir is the context directory (e.g., inputDir/context or outputDir/context)
func (p *PRPublisher) generatePRFixTitle(contextDir string) (string, error) {
	prPath := filepath.Join(contextDir, "github", "pr.json")
	data, err := os.ReadFile(prPath)
	if err != nil {
		return "", fmt.Errorf("failed to read pr.json: %w", err)
	}

	var info prInfo
	if err := json.Unmarshal(data, &info); err != nil {
		return "", fmt.Errorf("failed to parse pr.json: %w", err)
	}

	title := info.Title
	if title == "" {
		title = "pull request"
	}

	return fmt.Sprintf("Address review comments on #%d: %s", info.Number, title), nil
}

// writeManifestWithTitle writes the updated manifest (with injected title) back to disk.
func (p *PRPublisher) writeManifestWithTitle(outputDir string, manifest map[string]interface{}) error {
	manifestPath := filepath.Join(outputDir, "manifest.json")
	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal manifest: %w", err)
	}

	if err := os.WriteFile(manifestPath, data, 0o644); err != nil {
		return fmt.Errorf("failed to write manifest: %w", err)
	}

	return nil
}

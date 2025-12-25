package githubpr

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"time"

	"github.com/google/go-github/v68/github"
	"github.com/holon-run/holon/pkg/publisher"
	"golang.org/x/oauth2"
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

	// IssueFlag is the flag name for issue reference.
	IssueFlag = "issue"
)

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

	// Get GitHub token from environment
	token := os.Getenv(TokenEnv)
	if token == "" {
		token = os.Getenv(LegacyTokenEnv)
	}
	if token == "" {
		return publisher.PublishResult{}, fmt.Errorf("%s or %s environment variable is required", TokenEnv, LegacyTokenEnv)
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

	// Build configuration from manifest metadata
	config := p.buildConfig(req.Manifest)

	// Initialize result
	result := publisher.PublishResult{
		Provider:   p.Name(),
		Target:     req.Target,
		Actions:    []publisher.PublishAction{},
		Errors:     []publisher.PublishError{},
		Success:    true,
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
	gitClient := NewGitClient(workspaceDir, token)

	// Ensure workspace is clean
	if err := gitClient.EnsureCleanWorkspace(); err != nil {
		wrappedErr := fmt.Errorf("workspace validation failed: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	// Step 3: Apply patch
	patchPath := req.Artifacts["diff.patch"]
	if err := gitClient.ApplyPatch(patchPath); err != nil {
		wrappedErr := fmt.Errorf("failed to apply patch: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	result.Actions = append(result.Actions, publisher.PublishAction{
		Type:        "applied_patch",
		Description: "Applied diff.patch to workspace",
	})

	// Step 4: Create or checkout branch
	if err := gitClient.CreateBranch(config.BranchName); err != nil {
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

	// Step 5: Commit changes
	commitHash, err := gitClient.CommitChanges(config.CommitMessage)
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

	// Step 7: Create or update PR via GitHub API
	githubClient := p.createGitHubClient(ctx, token)
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
		// Update existing PR
		updatedPR, _, err := githubClient.PullRequests.Edit(ctx, prRef.Owner, prRef.Repo, *existingPR.Number, &github.PullRequest{
			Title: github.String(config.Title),
			Body:  github.String(prBody),
		})
		if err != nil {
			wrappedErr := fmt.Errorf("failed to update PR: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
			return result, wrappedErr
		}

		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "updated_pr",
			Description: fmt.Sprintf("Updated PR #%d", *existingPR.Number),
			Metadata: map[string]string{
				"pr_number": strconv.FormatInt(int64(*existingPR.Number), 10),
				"pr_url":    updatedPR.GetHTMLURL(),
			},
		})
	} else {
		// Create new PR
		newPR, _, err := githubClient.PullRequests.Create(ctx, prRef.Owner, prRef.Repo, &github.NewPullRequest{
			Title:               github.String(config.Title),
			Head:                github.String(config.BranchName),
			Base:                github.String(prRef.BaseBranch),
			Body:                github.String(prBody),
			MaintainerCanModify: github.Bool(true),
		})
		if err != nil {
			wrappedErr := fmt.Errorf("failed to create PR: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
			return result, wrappedErr
		}

		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "created_pr",
			Description: fmt.Sprintf("Created PR #%d", *newPR.Number),
			Metadata: map[string]string{
				"pr_number": strconv.FormatInt(int64(*newPR.Number), 10),
				"pr_url":    newPR.GetHTMLURL(),
			},
		})
	}

	result.PublishedAt = time.Now()
	return result, nil
}

// findPRByBranch finds an existing PR for the given head branch.
func (p *PRPublisher) findPRByBranch(ctx context.Context, client *github.Client, prRef PRRef, branchName string) (*github.PullRequest, error) {
	// List open PRs
	opts := &github.PullRequestListOptions{
		State: "open",
		ListOptions: github.ListOptions{
			PerPage: 100,
		},
	}

	for {
		prs, resp, err := client.PullRequests.List(ctx, prRef.Owner, prRef.Repo, opts)
		if err != nil {
			return nil, fmt.Errorf("failed to list PRs: %w", err)
		}

		// Search for PR with matching head branch
		for _, pr := range prs {
			headRef := pr.GetHead()
			if headRef != nil && headRef.GetRef() == branchName {
				return pr, nil
			}
		}

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	return nil, nil
}

// createGitHubClient creates a new GitHub client with the given token.
func (p *PRPublisher) createGitHubClient(ctx context.Context, token string) *github.Client {
	ts := oauth2.StaticTokenSource(&oauth2.Token{AccessToken: token})
	tc := oauth2.NewClient(ctx, ts)
	return github.NewClient(tc)
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
		if issue, ok := metadata[IssueFlag].(string); ok {
			config.IssueID = issue
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

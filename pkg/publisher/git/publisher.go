package git

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	holonGit "github.com/holon-run/holon/pkg/git"
	"github.com/holon-run/holon/pkg/publisher"
)

const (
	// DefaultCommitMessage is the default commit message.
	DefaultCommitMessage = "Apply changes from Holon"

	// DefaultRemote is the default remote name.
	DefaultRemote = "origin"

	// WorkspaceEnv is the environment variable for workspace directory.
	WorkspaceEnv = "HOLON_WORKSPACE"

	// GitTokenEnv is the environment variable for git authentication token.
	GitTokenEnv = "GIT_TOKEN"
)

// Publisher publishes Holon outputs to git.
type Publisher struct{}

// NewPublisher creates a new git publisher instance.
func NewPublisher() *Publisher {
	return &Publisher{}
}

// Name returns the provider name.
func (p *Publisher) Name() string {
	return "git"
}

// Validate checks if the request is valid for this publisher.
func (p *Publisher) Validate(req publisher.PublishRequest) error {
	// Build configuration to validate
	config := p.buildConfig(req.Manifest)

	// Validate that push requires commit
	if config.Push && !config.Commit {
		return fmt.Errorf("push requires commit to be enabled")
	}

	// Determine whether we need to validate that the workspace is a git repository.
	needsGitValidation := false

	// Check for diff.patch artifact (it's ok if it doesn't exist or is empty)
	// We'll handle that in Publish as a no-op
	if patchPath, ok := req.Artifacts["diff.patch"]; ok {
		needsGitValidation = true

		// Check that patch file can be read (but don't fail if it doesn't exist)
		if _, err := os.Stat(patchPath); err != nil && !os.IsNotExist(err) {
			return fmt.Errorf("failed to access patch file: %w", err)
		}
	}

	// Also validate the workspace as a git repository whenever metadata
	// indicates that git operations (branch selection, commit, or push)
	// will be performed.
	if config.Branch != "" || config.Commit || config.Push {
		needsGitValidation = true
	}

	if needsGitValidation {
		workspaceDir := p.getWorkspaceDir()
		gitClient := NewGitClient(workspaceDir, "")

		if err := gitClient.EnsureRepository(); err != nil {
			return fmt.Errorf("workspace validation failed: %w", err)
		}
	}

	return nil
}

// Publish sends Holon outputs to git.
//
// Note: If branch creation succeeds but a subsequent step (patch application,
// commit, or push) fails, the repository will be left on the newly created/checked
// out branch. This behavior is intentional to preserve state for debugging.
func (p *Publisher) Publish(req publisher.PublishRequest) (publisher.PublishResult, error) {
	ctx := context.Background()

	// Build configuration from manifest metadata
	config := p.buildConfig(req.Manifest)

	// Get workspace directory
	workspaceDir := p.getWorkspaceDir()

	// Get authentication token (optional)
	token := os.Getenv(GitTokenEnv)

	// Initialize result
	result := publisher.PublishResult{
		Provider: p.Name(),
		Target:   req.Target,
		Actions:  []publisher.PublishAction{},
		Errors:   []publisher.PublishError{},
		Success:  true,
	}

	// Check if diff.patch exists
	patchPath := req.Artifacts["diff.patch"]
	if patchPath == "" {
		// No patch file - this is a no-op
		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "no_op",
			Description: "No diff.patch artifact found, nothing to apply",
		})
		result.PublishedAt = time.Now()
		return result, nil
	}

	// Initialize Git client
	gitClient := NewGitClient(workspaceDir, token)

	// Ensure workspace is a git repository
	if err := gitClient.EnsureRepository(); err != nil {
		wrappedErr := fmt.Errorf("workspace validation failed: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	// Create/checkout branch FIRST (before applying patch)
	// This avoids issues with staged changes being reset during branch checkout
	if config.Branch != "" {
		if err := gitClient.CreateBranch(ctx, config.Branch); err != nil {
			wrappedErr := fmt.Errorf("failed to create branch: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
			return result, wrappedErr
		}

		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "created_branch",
			Description: fmt.Sprintf("Created/checked out branch: %s", config.Branch),
			Metadata: map[string]string{
				"branch": config.Branch,
			},
		})
	}

	// Apply patch
	applied, err := gitClient.ApplyPatch(ctx, patchPath)
	if err != nil {
		wrappedErr := fmt.Errorf("failed to apply patch: %w", err)
		result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
		result.Success = false
		return result, wrappedErr
	}

	if !applied {
		// Patch was empty or missing - no-op
		if len(result.Actions) == 0 {
			result.Actions = append(result.Actions, publisher.PublishAction{
				Type:        "no_op",
				Description: "diff.patch is empty or missing, nothing to apply",
			})
		}
		result.PublishedAt = time.Now()
		return result, nil
	}

	result.Actions = append(result.Actions, publisher.PublishAction{
		Type:        "applied_patch",
		Description: "Applied diff.patch to workspace",
	})

	// Commit changes if requested
	if config.Commit {
		commitMessage := config.CommitMessage
		if commitMessage == "" {
			commitMessage = DefaultCommitMessage
		}

		commitHash, err := gitClient.CommitChanges(ctx, commitMessage)
		if err != nil {
			wrappedErr := fmt.Errorf("failed to commit changes: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
			return result, wrappedErr
		}

		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "committed",
			Description: fmt.Sprintf("Committed changes: %s", commitHash),
			Metadata: map[string]string{
				"commit": commitHash,
			},
		})
	}

	// Push to remote if requested
	if config.Push {
		remoteName := config.Remote
		if remoteName == "" {
			remoteName = DefaultRemote
		}

		// Determine branch name for push
		branchName := config.Branch
		if branchName == "" {
			// Get current branch name using system git
			gitClient := holonGit.NewClient(workspaceDir)
			output, err := gitClient.ExecCommand(ctx, "rev-parse", "--abbrev-ref", "HEAD")
			if err != nil {
				wrappedErr := fmt.Errorf("failed to get current branch: %w", err)
				result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
				result.Success = false
				return result, wrappedErr
			}
			branchName = strings.TrimSpace(string(output))
		}

		if err := gitClient.Push(branchName, remoteName); err != nil {
			wrappedErr := fmt.Errorf("failed to push: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
			return result, wrappedErr
		}

		result.Actions = append(result.Actions, publisher.PublishAction{
			Type:        "pushed",
			Description: fmt.Sprintf("Pushed to %s/%s", remoteName, branchName),
			Metadata: map[string]string{
				"remote": remoteName,
				"branch": branchName,
			},
		})
	}

	result.PublishedAt = time.Now()
	return result, nil
}

// getWorkspaceDir gets the workspace directory from environment or uses current directory.
func (p *Publisher) getWorkspaceDir() string {
	workspaceDir := os.Getenv(WorkspaceEnv)
	if workspaceDir == "" {
		workspaceDir = "."
	}
	return workspaceDir
}

// buildConfig builds publisher configuration from manifest metadata.
func (p *Publisher) buildConfig(manifest map[string]interface{}) GitPublisherConfig {
	config := GitPublisherConfig{}

	// Extract configuration from manifest metadata
	if metadata, ok := manifest["metadata"].(map[string]interface{}); ok {
		if branch, ok := metadata["branch"].(string); ok {
			config.Branch = branch
		}
		if commitMsg, ok := metadata["commit_message"].(string); ok {
			config.CommitMessage = commitMsg
		}
		if remote, ok := metadata["remote"].(string); ok {
			config.Remote = remote
		}
		if commit, ok := metadata["commit"].(bool); ok {
			config.Commit = commit
		}
		if push, ok := metadata["push"].(bool); ok {
			config.Push = push
		}
	}

	return config
}

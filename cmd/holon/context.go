package main

import (
	"context"
	"fmt"

	ghcontext "github.com/holon-run/holon/pkg/context/github"
	"github.com/spf13/cobra"
)

var (
	contextOwner          string
	contextRepo           string
	contextPRNumber       int
	contextToken          string
	contextOutputDir      string
	contextUnresolvedOnly bool
	contextIncludeDiff    bool
	contextFromEnv        bool
)

var contextCmd = &cobra.Command{
	Use:   "context",
	Short: "Manage context preparation for Holon executions",
}

var collectPRCmd = &cobra.Command{
	Use:   "collect-pr",
	Short: "Collect GitHub PR review context",
	Long: `Collect GitHub PR context including review threads and diff.

This command fetches PR information, review comments, and optionally the diff,
and writes them to a standardized directory structure for use by Holon agents.

The output directory will contain:
  - github/pr.json: Pull request metadata
  - github/review_threads.json: Review comment threads
  - github/pr.diff: Unified diff (optional)
  - github/review.md: Human-readable summary

Examples:
  # Collect context for a specific PR
  holon context collect-pr --owner holon-run --repo holon --pr 42 --token $GITHUB_TOKEN --out ./context

  # Collect context from GitHub Actions environment
  holon context collect-pr --from-env --out ./holon-input/context

  # Collect only unresolved review threads
  holon context collect-pr --owner holon-run --repo holon --pr 42 --unresolved-only --out ./context
`,
	RunE: func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()

		if contextFromEnv {
			// Use environment variables (GitHub Actions mode)
			return ghcontext.CollectFromEnv(ctx, contextOutputDir)
		}

		// Validate required flags
		if contextOwner == "" {
			return fmt.Errorf("--owner is required")
		}
		if contextRepo == "" {
			return fmt.Errorf("--repo is required")
		}
		if contextPRNumber == 0 {
			return fmt.Errorf("--pr is required")
		}
		if contextToken == "" {
			return fmt.Errorf("--token is required (or use --from-env)")
		}

		config := ghcontext.CollectorConfig{
			Owner:          contextOwner,
			Repo:           contextRepo,
			PRNumber:       contextPRNumber,
			Token:          contextToken,
			OutputDir:      contextOutputDir,
			UnresolvedOnly: contextUnresolvedOnly,
			IncludeDiff:    contextIncludeDiff,
		}

		collector := ghcontext.NewCollector(config)
		return collector.Collect(ctx)
	},
}

func init() {
	// collect-pr command flags
	collectPRCmd.Flags().StringVar(&contextOwner, "owner", "", "GitHub repository owner")
	collectPRCmd.Flags().StringVar(&contextRepo, "repo", "", "GitHub repository name")
	collectPRCmd.Flags().IntVar(&contextPRNumber, "pr", 0, "Pull request number")
	collectPRCmd.Flags().StringVar(&contextToken, "token", "", "GitHub token")
	collectPRCmd.Flags().StringVar(&contextOutputDir, "out", "./holon-input/context", "Output directory for context files")
	collectPRCmd.Flags().BoolVar(&contextUnresolvedOnly, "unresolved-only", false, "Only collect unresolved review threads")
	collectPRCmd.Flags().BoolVar(&contextIncludeDiff, "include-diff", true, "Include PR diff")
	collectPRCmd.Flags().BoolVar(&contextFromEnv, "from-env", false, "Read configuration from environment variables (GitHub Actions mode)")

	contextCmd.AddCommand(collectPRCmd)
}

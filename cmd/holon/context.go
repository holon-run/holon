package main

import (
	"context"
	"fmt"
	"os"
	"strings"

	ghcontext "github.com/holon-run/holon/pkg/context/github"
	"github.com/holon-run/holon/pkg/context/collector"
	"github.com/holon-run/holon/pkg/context/provider/github"
	"github.com/holon-run/holon/pkg/context/registry"
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

	// New collect command flags
	collectKind      string
	collectRef       string
	collectRepo      string
	collectProvider  string
	collectToken     string
	collectOut       string
	collectNoDiff    bool
	collectUnresolved bool
)

var contextCmd = &cobra.Command{
	Use:   "context",
	Short: "Manage context preparation for Holon executions",
}

// collectCmd is the new unified collect command
var collectCmd = &cobra.Command{
	Use:   "collect <kind> <ref>",
	Short: "Collect context from various providers",
	Long: `Collect context from various providers (GitHub, etc.) for issues and pull requests.

This command provides a unified interface for collecting context from different
providers. The output is written to a standardized directory structure with a
manifest.json file describing the collected artifacts.

Supported kinds:
  - issue: Collect issue context
  - pr: Collect pull request context

Reference formats:
  - "#123" (requires --repo)
  - "owner/repo#123"
  - "https://github.com/owner/repo/pull/123"
  - "https://github.com/owner/repo/issues/123"

Provider selection:
  - If ref contains "github.com", defaults to "github"
  - Otherwise, use --provider flag (default: "github")

Examples:
  # Collect PR context using URL
  holon context collect pr https://github.com/holon-run/holon/pull/42 --out ./context

  # Collect issue context using owner/repo format
  holon context collect issue holon-run/holon#123 --out ./context

  # Collect with explicit provider and repo
  holon context collect pr "#42" --repo holon-run/holon --provider github --out ./context

  # Collect from environment variables (GitHub Actions)
  holon context collect pr --from-env --out ./holon-input/context
`,
	RunE: func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()

		// Handle --from-env mode (GitHub Actions integration)
		if cmd.Flags().Changed("from-env") && contextFromEnv {
			return ghcontext.CollectFromEnv(ctx, collectOut)
		}

		// Parse arguments
		if len(args) < 2 {
			return fmt.Errorf("requires <kind> and <ref> arguments\n\nUsage: holon context collect <kind> <ref> [flags]")
		}

		kindStr := args[0]
		ref := args[1]

		// Parse kind
		var kind collector.Kind
		switch kindStr {
		case "issue", "issues":
			kind = collector.KindIssue
		case "pr", "pull", "pullrequest":
			kind = collector.KindPR
		default:
			return fmt.Errorf("unknown kind: %s (must be 'issue' or 'pr')", kindStr)
		}

		// Auto-detect provider from URL if not specified
		providerName := collectProvider
		if providerName == "" {
			if strings.Contains(ref, "github.com") {
				providerName = "github"
			} else {
				providerName = "github" // Default to github for MVP
			}
		}

		// Get provider from registry
		prov := registry.Get(providerName)
		if prov == nil {
			return fmt.Errorf("provider '%s' not found (registered providers: %v)",
				providerName, registry.List())
		}

		// Build request
		req := collector.CollectRequest{
			Kind:      kind,
			Ref:       ref,
			RepoHint:  collectRepo,
			OutputDir: collectOut,
			Options: collector.Options{
				Token:          collectToken,
				IncludeDiff:    !collectNoDiff,
				UnresolvedOnly: collectUnresolved,
			},
		}

		// Get token from environment if not specified
		if req.Options.Token == "" {
			req.Options.Token = os.Getenv("GITHUB_TOKEN")
			if req.Options.Token == "" {
				req.Options.Token = os.Getenv("GH_TOKEN")
			}
		}

		// Validate request
		if err := prov.Validate(req); err != nil {
			return fmt.Errorf("validation failed: %w", err)
		}

		// Collect
		result, err := prov.Collect(ctx, req)
		if err != nil {
			return fmt.Errorf("collection failed: %w", err)
		}

		// Print summary
		fmt.Println("\nCollection summary:")
		fmt.Printf("  Provider: %s\n", result.Provider)
		fmt.Printf("  Kind: %s\n", result.Kind)
		fmt.Printf("  Repository: %s/%s#%d\n", result.Owner, result.Repo, result.Number)
		fmt.Printf("  Collected at: %s\n", result.CollectedAt.Format("2006-01-02 15:04:05"))
		fmt.Printf("  Files written: %d\n", len(result.Files))
		for _, f := range result.Files {
			fmt.Printf("    - %s\n", f.Path)
		}
		fmt.Printf("  Output directory: %s/\n", collectOut)

		return nil
	},
}

// collectPRCmd is the legacy command for backward compatibility
var collectPRCmd = &cobra.Command{
	Use:   "collect-pr",
	Short: "Collect GitHub PR review context (legacy)",
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

Note: This is a legacy command. Use 'holon context collect' for new workflows.
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
	// Register built-in providers
	githubProvider := github.NewProvider()
	if err := registry.Register(githubProvider); err != nil {
		panic(fmt.Sprintf("failed to register github provider: %v", err))
	}

	// collect command flags (new unified command)
	collectCmd.Flags().StringVar(&collectRepo, "repo", "", "Repository hint (e.g., 'owner/repo') when ref doesn't include it")
	collectCmd.Flags().StringVarP(&collectProvider, "provider", "p", "", "Provider name (default: auto-detect from ref)")
	collectCmd.Flags().StringVar(&collectToken, "token", "", "Authentication token (defaults to GITHUB_TOKEN env var)")
	collectCmd.Flags().StringVarP(&collectOut, "out", "o", "./holon-input/context", "Output directory for context files")
	collectCmd.Flags().BoolVar(&collectNoDiff, "no-diff", false, "Exclude PR diff")
	collectCmd.Flags().BoolVar(&collectUnresolved, "unresolved-only", false, "Only collect unresolved review threads")
	collectCmd.Flags().BoolVar(&contextFromEnv, "from-env", false, "Read configuration from environment variables (GitHub Actions mode)")

	contextCmd.AddCommand(collectCmd)

	// collect-pr command flags (legacy, for backward compatibility)
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

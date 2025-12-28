package main

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/holon-run/holon/pkg/context/collector"
	"github.com/holon-run/holon/pkg/context/provider/github"
	"github.com/holon-run/holon/pkg/context/registry"
	"github.com/spf13/cobra"
)

// collectFromEnv parses environment variables for GitHub Actions mode and returns
// the collect request and output directory. Returns an error if required environment
// variables are not set.
func collectFromEnv() (collector.CollectRequest, string, error) {
	// Get provider from registry
	prov := registry.Get("github")
	if prov == nil {
		return collector.CollectRequest{}, "", fmt.Errorf("github provider not found in registry")
	}

	// Parse repository from GITHUB_REPOSITORY env var
	repo := os.Getenv("GITHUB_REPOSITORY")
	if repo == "" {
		return collector.CollectRequest{}, "", fmt.Errorf("GITHUB_REPOSITORY environment variable not set")
	}

	// Get PR number from event
	prNumberStr := os.Getenv("PR_NUMBER")
	if prNumberStr == "" {
		return collector.CollectRequest{}, "", fmt.Errorf("PR_NUMBER environment variable not set")
	}

	ref := fmt.Sprintf("%s#%s", repo, prNumberStr)

	// Get token from environment
	token := os.Getenv("GITHUB_TOKEN")
	if token == "" {
		token = os.Getenv("GH_TOKEN")
	}
	if token == "" {
		return collector.CollectRequest{}, "", fmt.Errorf("GITHUB_TOKEN or GH_TOKEN environment variable not set")
	}

	// Check if we should only include unresolved threads
	unresolvedOnly := os.Getenv("UNRESOLVED_ONLY") == "true"

	// Check if we should include diff
	includeDiff := os.Getenv("INCLUDE_DIFF") != "false" // Default to true

	// Check if we should include CI/checks
	includeChecks := os.Getenv("INCLUDE_CHECKS") != "false" // Default to true

	// Check if we should only include failed checks
	checksOnlyFailed := os.Getenv("CHECKS_ONLY_FAILED") == "true"

	// Get max checks (default 200)
	checksMax := 200
	if maxStr := os.Getenv("CHECKS_MAX"); maxStr != "" {
		if n, err := fmt.Sscanf(maxStr, "%d", &checksMax); !(n == 1 && err == nil) {
			fmt.Fprintf(os.Stderr, "Warning: invalid CHECKS_MAX value %q, using default %d\n", maxStr, checksMax)
		}
	}

	// Get output directory from environment or use default
	outDir := os.Getenv("HOLON_CONTEXT_OUT")
	if outDir == "" {
		outDir = "./holon-input/context"
	}

	return collector.CollectRequest{
		Kind:      collector.KindPR,
		Ref:       ref,
		OutputDir: outDir,
		Options: collector.Options{
			Token:           token,
			IncludeDiff:     includeDiff,
			UnresolvedOnly:  unresolvedOnly,
			IncludeChecks:   includeChecks,
			ChecksOnlyFailed: checksOnlyFailed,
			ChecksMax:       checksMax,
		},
	}, outDir, nil
}

// printCollectionSummary prints a formatted summary of the collection result.
func printCollectionSummary(result collector.CollectResult, outputDir string) {
	fmt.Println("\nCollection summary:")
	fmt.Printf("  Provider: %s\n", result.Provider)
	fmt.Printf("  Kind: %s\n", result.Kind)
	fmt.Printf("  Repository: %s/%s#%d\n", result.Owner, result.Repo, result.Number)
	fmt.Printf("  Collected at: %s\n", result.CollectedAt.Format("2006-01-02 15:04:05"))
	fmt.Printf("  Files written: %d\n", len(result.Files))
	for _, f := range result.Files {
		fmt.Printf("    - %s\n", f.Path)
	}
	fmt.Printf("  Output directory: %s/\n", outputDir)
}

var (
	contextFromEnv bool

	// New collect command flags
	collectKind       string
	collectRef        string
	collectRepo       string
	collectProvider   string
	collectToken      string
	collectOut        string
	collectNoDiff     bool
	collectUnresolved bool
	collectNoChecks   bool
	collectChecksOnlyFailed bool
	collectChecksMax  int
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
			// Validate that no positional arguments are provided with --from-env
			if len(args) > 0 {
				return fmt.Errorf("positional arguments are not allowed when using --from-env\n\nUsage: holon context collect --from-env --out ./holon-input/context")
			}

			// Parse environment variables and build request
			req, outDir, err := collectFromEnv()
			if err != nil {
				return err
			}

			// Override output directory if explicitly set via flag
			if cmd.Flags().Changed("out") {
				req.OutputDir = collectOut
				outDir = collectOut
			}

			// Get provider from registry
			prov := registry.Get("github")
			if prov == nil {
				return fmt.Errorf("github provider not found in registry")
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
			printCollectionSummary(result, outDir)

			return nil
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
				Token:            collectToken,
				IncludeDiff:      !collectNoDiff,
				UnresolvedOnly:   collectUnresolved,
				IncludeChecks:    !collectNoChecks,
				ChecksOnlyFailed: collectChecksOnlyFailed,
				ChecksMax:        collectChecksMax,
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
		printCollectionSummary(result, collectOut)

		return nil
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
	collectCmd.Flags().StringVarP(&collectOut, "output", "O", "./holon-input/context", "Output directory for context files")
	_ = collectCmd.Flags().MarkDeprecated("out", "use --output instead")
	collectCmd.Flags().StringVarP(&collectOut, "out", "o", "./holon-input/context", "Deprecated: use --output")
	collectCmd.Flags().BoolVar(&collectNoDiff, "no-diff", false, "Exclude PR diff")
	collectCmd.Flags().BoolVar(&collectUnresolved, "unresolved-only", false, "Only collect unresolved review threads")
	collectCmd.Flags().BoolVar(&collectNoChecks, "no-checks", false, "Exclude CI/check results")
	collectCmd.Flags().BoolVar(&collectChecksOnlyFailed, "checks-only-failed", false, "Only include failed checks")
	collectCmd.Flags().IntVar(&collectChecksMax, "checks-max", 200, "Maximum number of check runs to fetch (0 = unlimited)")
	collectCmd.Flags().BoolVar(&contextFromEnv, "from-env", false, "Read configuration from environment variables (GitHub Actions mode)")

	contextCmd.AddCommand(collectCmd)
}

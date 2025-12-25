package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/holon-run/holon/pkg/context/issue"
	prcontext "github.com/holon-run/holon/pkg/context/github"
	pkggithub "github.com/holon-run/holon/pkg/github"
	"github.com/holon-run/holon/pkg/publisher"
	"github.com/holon-run/holon/pkg/runtime/docker"
	"github.com/spf13/cobra"
)

var (
	solveRepo     string
	solveBase     string
	solveOutDir   string
	solveContext  string
	solveAgent    string
	solveImage    string
	solveMode     string
	solveRole     string
	solveLogLevel string
	solveDryRun   bool
)

// solveCmd is the parent solve command
var solveCmd = &cobra.Command{
	Use:   "solve <ref>",
	Short: "Solve a GitHub Issue or PR reference",
	Long: `Solve a GitHub Issue or PR reference by collecting context, running Holon, and publishing results.

This is a high-level command that orchestrates the full workflow:
1. Collect context from the GitHub Issue or PR
2. Run Holon with the collected context
3. Publish results (create PR for issues, or push/fix for PRs)

Supported Reference Formats:
  - Full URLs:
    - Issue: https://github.com/<owner>/<repo>/issues/<n>
    - PR:    https://github.com/<owner>/<repo>/pull/<n>
  - Short forms:
    - <owner>/<repo>#<n>
    - #<n> (when --repo <owner>/<repo> is provided)

Examples:
  # Solve an issue (creates/updates a PR)
  holon solve https://github.com/holon-run/holon/issues/123

  # Solve a PR (fixes review comments)
  holon solve https://github.com/holon-run/holon/pull/456

  # Short form
  holon solve holon-run/holon#789

  # Numeric reference (requires --repo)
  holon solve 123 --repo holon-run/holon
  holon solve #123 --repo holon-run/holon`,
	RunE: func(cmd *cobra.Command, args []string) error {
		if len(args) == 0 {
			return fmt.Errorf("reference argument is required")
		}
		return runSolve(context.Background(), args[0], "")
	},
}

// solveIssueCmd is the explicit "solve issue" subcommand
var solveIssueCmd = &cobra.Command{
	Use:   "issue <ref>",
	Short: "Solve a GitHub Issue (creates/updates a PR)",
	Long: `Solve a GitHub Issue by collecting context, running Holon, and creating/updating a PR.

The workflow:
1. Collect issue context (title, body, comments)
2. Run Holon in "solve" mode
3. If diff.patch exists: create/commit/push to a new branch
4. Create or update a PR with the summary

Examples:
  holon solve issue https://github.com/holon-run/holon/issues/123
  holon solve issue holon-run/holon#456
  holon solve issue 123 --repo holon-run/holon`,
	RunE: func(cmd *cobra.Command, args []string) error {
		if len(args) == 0 {
			return fmt.Errorf("reference argument is required")
		}
		return runSolve(context.Background(), args[0], "issue")
	},
}

// solvePRCmd is the explicit "solve pr" subcommand
var solvePRCmd = &cobra.Command{
	Use:   "pr <ref>",
	Short: "Solve a GitHub PR (fixes review comments)",
	Long: `Solve a GitHub PR by collecting context, running Holon, and fixing review comments.

The workflow:
1. Collect PR context (diff, review threads, checks)
2. Run Holon in "pr-fix" mode
3. If diff.patch exists: apply/push to PR branch
4. Publish replies based on pr-fix.json

Examples:
  holon solve pr https://github.com/holon-run/holon/pull/123
  holon solve pr holon-run/holon#456
  holon solve pr 123 --repo holon-run/holon`,
	RunE: func(cmd *cobra.Command, args []string) error {
		if len(args) == 0 {
			return fmt.Errorf("reference argument is required")
		}
		return runSolve(context.Background(), args[0], "pr")
	},
}

// runSolve implements the main solve logic
func runSolve(ctx context.Context, refStr, explicitType string) error {
	// Get GitHub token early for validation
	token, err := getGitHubToken()
	if err != nil {
		return err
	}

	// Parse the reference
	solveRef, err := pkggithub.ParseSolveRef(refStr, solveRepo)
	if err != nil {
		return err
	}

	fmt.Printf("Parsed reference: %s\n", solveRef.String())

	// If explicit type is provided, use it; otherwise determine via API
	refType := explicitType
	if refType == "" {
		// Determine type by checking if it's a PR via API
		refType, err = determineRefType(ctx, solveRef, token)
		if err != nil {
			return fmt.Errorf("failed to determine reference type: %w", err)
		}
	} else {
		// Use the explicitly specified type
		if refType == "issue" {
			solveRef.Type = pkggithub.SolveRefTypeIssue
		} else if refType == "pr" {
			solveRef.Type = pkggithub.SolveRefTypePR
		}
	}

	fmt.Printf("Detected type: %s\n", refType)

	// Create output directory
	outDir := solveOutDir
	if outDir == "" {
		outDir = "./holon-output"
	}
	if err := os.MkdirAll(outDir, 0755); err != nil {
		return fmt.Errorf("failed to create output directory: %w", err)
	}

	// Collect context based on type
	contextDir := filepath.Join(outDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		return fmt.Errorf("failed to create context directory: %w", err)
	}

	if refType == "pr" {
		// Collect PR context
		prCollector := prcontext.NewCollector(prcontext.CollectorConfig{
			Owner:          solveRef.Owner,
			Repo:           solveRef.Repo,
			PRNumber:       solveRef.Number,
			Token:          token,
			OutputDir:      contextDir,
			UnresolvedOnly: true,
			IncludeDiff:    true,
		})
		if err := prCollector.Collect(ctx); err != nil {
			return fmt.Errorf("failed to collect PR context: %w", err)
		}
		// Only override mode if user hasn't explicitly set it
		if solveMode == "" {
			solveMode = "pr-fix"
		}
	} else {
		// Collect issue context
		issueCollector := issue.NewCollector(issue.CollectorConfig{
			Owner:     solveRef.Owner,
			Repo:      solveRef.Repo,
			IssueNum:  solveRef.Number,
			Token:     token,
			OutputDir: contextDir,
		})
		if err := issueCollector.Collect(ctx); err != nil {
			return fmt.Errorf("failed to collect issue context: %w", err)
		}
		// Only override mode if user hasn't explicitly set it
		if solveMode == "" {
			solveMode = "solve"
		}
	}

	// Get workspace directory
	workspace := "."
	if solveRepo != "" {
		// Try to clone the repo if not already present
		// For now, we'll use the current directory as workspace
		// In the future, we could support auto-cloning
	}

	// Determine goal from the reference
	goal := buildGoal(solveRef, refType)

	// Run holon
	fmt.Println("\n" + strings.Repeat("=", 60))
	fmt.Println("Running Holon...")
	fmt.Println(strings.Repeat("=", 60))

	rt, err := docker.NewRuntime()
	if err != nil {
		return fmt.Errorf("failed to initialize runtime: %w", err)
	}

	runner := NewRunner(rt)
	err = runner.Run(ctx, RunnerConfig{
		GoalStr:       goal,
		TaskName:      fmt.Sprintf("solve-%s-%d", refType, solveRef.Number),
		BaseImage:     solveImage,
		AgentBundle:   solveAgent,
		WorkspacePath: workspace,
		ContextPath:   contextDir,
		OutDir:        outDir,
		RoleName:      solveRole,
		LogLevel:      solveLogLevel,
		Mode:          solveMode,
	})

	if err != nil {
		return fmt.Errorf("holon execution failed: %w", err)
	}

	// Publish results
	fmt.Println("\n" + strings.Repeat("=", 60))
	fmt.Println("Publishing results...")
	fmt.Println(strings.Repeat("=", 60))

	if err := publishResults(ctx, solveRef, refType, outDir); err != nil {
		return fmt.Errorf("failed to publish results: %w", err)
	}

	fmt.Println("\n" + strings.Repeat("=", 60))
	fmt.Println("Solve completed successfully!")
	fmt.Println(strings.Repeat("=", 60))

	return nil
}

// determineRefType determines if a reference is an issue or PR via GitHub API
func determineRefType(ctx context.Context, ref *pkggithub.SolveRef, token string) (string, error) {
	client := pkggithub.NewClient(token)

	// Try to fetch as PR - if successful, it's a PR
	_, err := client.FetchPRInfo(ctx, ref.Owner, ref.Repo, ref.Number)
	if err == nil {
		ref.Type = pkggithub.SolveRefTypePR
		return "pr", nil
	}

	// If PR fetch failed, try as issue
	_, err = client.FetchIssueInfo(ctx, ref.Owner, ref.Repo, ref.Number)
	if err == nil {
		ref.Type = pkggithub.SolveRefTypeIssue
		return "issue", nil
	}

	return "", fmt.Errorf("reference is neither a valid PR nor issue")
}

// getGitHubToken retrieves the GitHub token from environment variables
func getGitHubToken() (string, error) {
	token := os.Getenv("GITHUB_TOKEN")
	if token == "" {
		token = os.Getenv("GH_TOKEN")
	}
	if token == "" {
		return "", fmt.Errorf("GITHUB_TOKEN or GH_TOKEN environment variable is required")
	}
	return token, nil
}

// buildGoal builds a goal description from the reference
func buildGoal(ref *pkggithub.SolveRef, refType string) string {
	if refType == "pr" {
		return fmt.Sprintf("Fix the review comments and issues in PR %s. Address all unresolved review comments and make necessary code changes.", ref.URL())
	}
	return fmt.Sprintf("Implement a solution for the issue described in %s. Create a PR with your changes.", ref.URL())
}

// publishResults publishes the holon execution results
func publishResults(ctx context.Context, ref *pkggithub.SolveRef, refType string, outDir string) error {
	// Read manifest
	manifestPath := filepath.Join(outDir, "manifest.json")
	manifestData, err := os.ReadFile(manifestPath)
	if err != nil {
		return fmt.Errorf("failed to read manifest.json: %w", err)
	}

	// Parse manifest
	var manifestMap map[string]interface{}
	if err := json.Unmarshal(manifestData, &manifestMap); err != nil {
		return fmt.Errorf("failed to parse manifest.json: %w", err)
	}

	// Ensure metadata exists
	if manifestMap["metadata"] == nil {
		manifestMap["metadata"] = make(map[string]interface{})
	}
	metadata := manifestMap["metadata"].(map[string]interface{})
	metadata["issue"] = fmt.Sprintf("%s/%s#%d", ref.Owner, ref.Repo, ref.Number)
	if refType == "issue" {
		metadata["issue_id"] = fmt.Sprintf("%d", ref.Number)
	}

	// Read artifacts
	artifacts := make(map[string]string)
	artifacts["manifest.json"] = manifestPath

	// Check for diff.patch
	if patchPath := filepath.Join(outDir, "diff.patch"); fileExists(patchPath) {
		artifacts["diff.patch"] = patchPath
	}

	// Check for summary.md
	if summaryPath := filepath.Join(outDir, "summary.md"); fileExists(summaryPath) {
		artifacts["summary.md"] = summaryPath
	}

	// Check for pr-fix.json
	if prFixPath := filepath.Join(outDir, "pr-fix.json"); fileExists(prFixPath) {
		artifacts["pr-fix.json"] = prFixPath
	}

	req := publisher.PublishRequest{
		OutputDir: outDir,
		Manifest:  manifestMap,
		Artifacts: artifacts,
	}

	var target string
	if refType == "pr" {
		target = fmt.Sprintf("%s/%s/pr/%d", ref.Owner, ref.Repo, ref.Number)
	} else {
		// For issues, target is the repo for creating a PR
		if solveBase != "" {
			target = fmt.Sprintf("%s/%s:%s", ref.Owner, ref.Repo, solveBase)
		} else {
			target = fmt.Sprintf("%s/%s:main", ref.Owner, ref.Repo)
		}
	}

	req.Target = target

	// Get publisher
	var pub publisher.Publisher
	if refType == "pr" {
		pub = publisher.Get("github")
	} else {
		pub = publisher.Get("github-pr")
	}

	if pub == nil {
		return fmt.Errorf("publisher '%s' not found", map[string]string{"pr": "github", "issue": "github-pr"}[refType])
	}

	// Validate
	if err := pub.Validate(req); err != nil {
		return fmt.Errorf("validation failed: %w", err)
	}

	// Publish
	result, err := pub.Publish(req)
	if err != nil {
		return fmt.Errorf("publish failed: %w", err)
	}

	// Write result
	if err := publisher.WriteResult(outDir, result); err != nil {
		return fmt.Errorf("failed to write publish result: %w", err)
	}

	// Print summary
	if result.Success {
		fmt.Printf("Successfully published to %s\n", result.Target)
		for _, action := range result.Actions {
			fmt.Printf("  - %s\n", action.Description)
		}
	} else {
		fmt.Printf("Publish completed with errors\n")
		for _, e := range result.Errors {
			fmt.Printf("  - %s\n", e.Message)
		}
	}

	return nil
}

// fileExists checks if a file exists
func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func init() {
	solveCmd.Flags().StringVar(&solveRepo, "repo", "", "Default repository in owner/repo format (for numeric references)")
	solveCmd.Flags().StringVar(&solveBase, "base", "main", "Base branch for PR creation (issue mode only)")
	solveCmd.Flags().StringVarP(&solveOutDir, "out", "o", "./holon-output", "Output directory")
	solveCmd.Flags().StringVarP(&solveContext, "context", "c", "", "Additional context directory (deprecated)")
	solveCmd.Flags().StringVar(&solveAgent, "agent", "", "Agent bundle reference")
	solveCmd.Flags().StringVarP(&solveImage, "image", "i", "golang:1.22", "Docker base image")
	solveCmd.Flags().StringVar(&solveMode, "mode", "", "Execution mode (default: auto-detect from ref type)")
	solveCmd.Flags().StringVarP(&solveRole, "role", "r", "", "Role to assume")
	solveCmd.Flags().StringVar(&solveLogLevel, "log-level", "progress", "Log level")
	solveCmd.Flags().BoolVar(&solveDryRun, "dry-run", false, "Validate without running (not yet implemented)")

	// Add subcommands
	solveCmd.AddCommand(solveIssueCmd)
	solveCmd.AddCommand(solvePRCmd)

	// Add solve command to root
	rootCmd.AddCommand(solveCmd)

	// Add fix as an alias
	fixCmd := *solveCmd
	fixCmd.Use = "fix <ref>"
	fixCmd.Short = "Alias for 'solve' - resolve a GitHub Issue or PR reference"
	rootCmd.AddCommand(&fixCmd)
}

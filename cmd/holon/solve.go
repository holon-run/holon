package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/holon-run/holon/pkg/config"
	"github.com/holon-run/holon/pkg/context/issue"
	prcontext "github.com/holon-run/holon/pkg/context/github"
	"github.com/holon-run/holon/pkg/git"
	pkggithub "github.com/holon-run/holon/pkg/github"
	"github.com/holon-run/holon/pkg/image"
	"github.com/holon-run/holon/pkg/publisher"
	"github.com/holon-run/holon/pkg/runtime/docker"
	"github.com/holon-run/holon/pkg/workspace"
	"github.com/spf13/cobra"
)

var (
	solveRepo            string
	solveBase            string
	solveOutDir          string
	solveContext         string
	solveInput           string
	solveCleanup         string
	solveAgent           string
	solveImage           string
	solveImageAutoDetect bool
	solveMode            string
	solveRole            string
	solveLogLevel        string
	solveDryRun          bool
	solveWorkspace       string
	solveWorkspaceHistory  string
	solveWorkspaceRef      string
	solveFetchRemote       bool
	solveAgentConfigMode   string
)

// solveCmd is the parent solve command
var solveCmd = &cobra.Command{
	Use:   "solve <ref>",
	Short: "Solve a GitHub Issue or PR reference",
	Long: `Solve a GitHub Issue or PR reference by collecting context, running Holon, and publishing results.

This is a high-level command that orchestrates the full workflow:
1. Prepare workspace using smart workspace preparation
2. Collect context from the GitHub Issue or PR
3. Run Holon with the collected context
4. Publish results (create PR for issues, or push/fix for PRs)

Workspace Preparation:
  The workspace is prepared automatically based on the context:
  - If --workspace PATH is provided: uses the existing directory (no cloning)
  - If current directory matches the ref repo: creates a clean temp workspace via git-clone (using --local)
  - Otherwise: clones from the remote repository into a temp directory (shallow by default)

  Temporary workspaces are automatically cleaned up after execution.

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
  holon solve #123 --repo holon-run/holon

  # Use specific workspace path
  holon solve holon-run/holon#123 --workspace /path/to/workspace

  # Control workspace history mode
  holon solve holon-run/holon#123 --workspace-history full`,
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

// workspacePreparation holds the workspace preparation result
type workspacePreparation struct {
	path       string
	preparer   workspace.Preparer
	cleanupNeeded bool
}

// prepareWorkspaceForSolve prepares a workspace based on the solve reference and flags
// Decision logic:
// 1) If --workspace PATH is provided: use preparer "existing" on PATH (no clone/copy)
// 2) If not provided and current dir is a git repo matching the ref owner/repo:
//    prepare a clean temp workspace via git-clone with Source=current repo (using --local internally)
// 3) Otherwise: prepare via git-clone from the ref's repo URL into a temp dir (shallow by default)
//
// Note: The token parameter is currently unused for authentication during git clone operations.
// This means that private repositories may fail to clone if they require authentication.
// This is intentional for the initial implementation to avoid embedding credentials in git URLs.
// Future enhancement could add git credential helper integration or SSH-based authentication.
func prepareWorkspaceForSolve(ctx context.Context, solveRef *pkggithub.SolveRef, token string) (*workspacePreparation, error) {
	var workspacePath string
	var preparer workspace.Preparer
	var cleanupNeeded bool
	var source string

	// Decision 1: If --workspace PATH is provided, use "existing" strategy
	if solveWorkspace != "" {
		source = solveWorkspace
		workspacePath = solveWorkspace
		preparer = workspace.NewExistingPreparer()
		cleanupNeeded = false

		fmt.Printf("Workspace mode: existing (user-provided path)\n")
		fmt.Printf("  Path: %s\n", workspacePath)

		// Prepare using existing strategy
		_, err := preparer.Prepare(ctx, workspace.PrepareRequest{
			Source:  source,
			Dest:    workspacePath,
			Ref:     solveWorkspaceRef,
			History: workspace.HistoryFull, // History doesn't matter for existing
		})
		if err != nil {
			return nil, fmt.Errorf("failed to prepare workspace using existing strategy: %w", err)
		}
	} else {
		// Decision 2: Check if current directory matches the ref owner/repo
		currentDir, err := os.Getwd()
		if err != nil {
			return nil, fmt.Errorf("failed to get current directory: %w", err)
		}

		currentDirRepo, err := getGitRepoOrigin(ctx, currentDir)
		if err == nil && currentDirRepo == fmt.Sprintf("%s/%s", solveRef.Owner, solveRef.Repo) {
			// Current dir matches the ref repo - use git-clone with --local from current repo
			source = currentDir

			// Create a temp directory for the workspace
			tempDir, err := os.MkdirTemp("", "holon-solve-workspace-*")
			if err != nil {
				return nil, fmt.Errorf("failed to create temp workspace directory: %w", err)
			}
			workspacePath = tempDir
			preparer = workspace.NewGitClonePreparer()
			cleanupNeeded = true

			fmt.Printf("Workspace mode: git-clone (from local repo matching ref)\n")
			fmt.Printf("  Source: %s\n", source)
			fmt.Printf("  Dest: %s\n", workspacePath)

			// Determine history mode
			historyMode := solveWorkspaceHistory
			if historyMode == "" {
				historyMode = "full" // Default to full for local clones
			}

			// Prepare using git-clone strategy
			_, err = preparer.Prepare(ctx, workspace.PrepareRequest{
				Source:  source,
				Dest:    workspacePath,
				Ref:     solveWorkspaceRef,
				History: workspace.HistoryMode(historyMode),
				CleanDest: true,
			})
			if err != nil {
				os.RemoveAll(workspacePath)
				return nil, fmt.Errorf("failed to prepare workspace using git-clone from local repo: %w", err)
			}
		} else {
			// Decision 3: Clone from remote repository
			source = fmt.Sprintf("https://github.com/%s/%s.git", solveRef.Owner, solveRef.Repo)

			// Create a temp directory for the workspace
			tempDir, err := os.MkdirTemp("", "holon-solve-workspace-*")
			if err != nil {
				return nil, fmt.Errorf("failed to create temp workspace directory: %w", err)
			}
			workspacePath = tempDir
			preparer = workspace.NewGitClonePreparer()
			cleanupNeeded = true

			fmt.Printf("Workspace mode: git-clone (from remote)\n")
			fmt.Printf("  Source: %s\n", source)
			fmt.Printf("  Dest: %s\n", workspacePath)

			// Determine history mode
			historyMode := solveWorkspaceHistory
			if historyMode == "" {
				historyMode = "shallow" // Default to shallow for remote clones
			}

			// Prepare using git-clone strategy
			result, err := preparer.Prepare(ctx, workspace.PrepareRequest{
				Source:  source,
				Dest:    workspacePath,
				Ref:     solveWorkspaceRef,
				History: workspace.HistoryMode(historyMode),
				CleanDest: true,
			})
			if err != nil {
				os.RemoveAll(workspacePath)
				return nil, fmt.Errorf("failed to prepare workspace using git-clone from remote: %w", err)
			}

			// Log preparation details
			fmt.Printf("  Strategy: %s\n", result.Strategy)
			if result.HeadSHA != "" {
				fmt.Printf("  HEAD: %s\n", result.HeadSHA)
			}
			if result.HasHistory {
				if result.IsShallow {
					fmt.Printf("  History: shallow\n")
				} else {
					fmt.Printf("  History: full\n")
				}
			} else {
				fmt.Printf("  History: none\n")
			}
		}
	}

	// Optional: fetch remote updates if requested (available for all workspace modes)
	if solveFetchRemote {
		fmt.Println("Fetching remote updates...")
		if err := fetchRemoteUpdates(ctx, workspacePath); err != nil {
			fmt.Printf("Warning: failed to fetch remote updates: %v\n", err)
		}
	}

	return &workspacePreparation{
		path:         workspacePath,
		preparer:     preparer,
		cleanupNeeded: cleanupNeeded,
	}, nil
}

// getGitRepoOrigin gets the owner/repo from a git repository's remote origin
func getGitRepoOrigin(ctx context.Context, dir string) (string, error) {
	client := git.NewClient(dir)

	// Get remote origin URL
	originURL, err := client.ConfigGet(ctx, "remote.origin.url")
	if err != nil {
		return "", fmt.Errorf("failed to get remote origin URL: %w", err)
	}

	// Parse the URL to extract owner/repo
	// Support both HTTPS and SSH URLs
	// HTTPS: https://github.com/owner/repo.git or https://github.com/owner/repo
	// SSH: git@github.com:owner/repo.git

	// Remove .git suffix if present
	url := strings.TrimSuffix(originURL, ".git")

	// Handle SSH format: git@github.com:owner/repo
	if strings.HasPrefix(url, "git@") {
		parts := strings.SplitN(url, ":", 2)
		if len(parts) == 2 {
			url = parts[1]
		}
	}

	// Handle HTTPS format: https://github.com/owner/repo
	if strings.HasPrefix(url, "https://") {
		parts := strings.Split(url, "/")
		// After splitting by "/", we expect at least: ["https:", "", "github.com", "owner", "repo"]
		if len(parts) >= 5 {
			// Remove trailing slashes and empty parts
			owner := strings.TrimSuffix(parts[len(parts)-2], "/")
			repo := strings.TrimSuffix(parts[len(parts)-1], "/")
			if owner != "" && repo != "" {
				return fmt.Sprintf("%s/%s", owner, repo), nil
			}
		}
	}

	// Handle remaining format: owner/repo
	// This also handles the case where SSH URL was converted to owner/repo
	parts := strings.Split(url, "/")
	if len(parts) >= 2 {
		// Remove trailing slashes and empty parts
		owner := strings.TrimSuffix(parts[len(parts)-2], "/")
		repo := strings.TrimSuffix(parts[len(parts)-1], "/")
		if owner != "" && repo != "" {
			return fmt.Sprintf("%s/%s", owner, repo), nil
		}
	}

	return "", fmt.Errorf("unable to parse owner/repo from URL: %s", originURL)
}

// fetchRemoteUpdates fetches updates from the remote repository
func fetchRemoteUpdates(ctx context.Context, dir string) error {
	// Fetch from origin
	cmd := exec.CommandContext(ctx, "git", "-C", dir, "fetch", "origin")
	if output, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git fetch failed: %w: %s", err, string(output))
	}

	return nil
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

	// Create input directory (or use user-provided path)
	inputDir := solveInput
	inputIsTemp := false
	if inputDir == "" {
		// Create temporary input directory
		td, err := os.MkdirTemp("", "holon-input-*")
		if err != nil {
			return fmt.Errorf("failed to create temp input dir: %w", err)
		}
		inputDir = td
		inputIsTemp = true
		fmt.Printf("Created temporary input directory: %s\n", inputDir)
	} else {
		fmt.Printf("Using input directory: %s\n", inputDir)
	}

	// Cleanup input directory based on cleanup mode
	cleanupMode := solveCleanup
	if cleanupMode == "" {
		cleanupMode = "auto" // Default to auto cleanup
	}

	// Validate cleanup mode
	if cleanupMode != "auto" && cleanupMode != "none" && cleanupMode != "all" {
		return fmt.Errorf("invalid cleanup mode: %q (must be one of: auto, none, all)", cleanupMode)
	}

	// Cleanup input directory based on mode and whether it's temp
	// For temp input: clean on "auto" or "all"
	// For user input: clean only on "all"
	if (inputIsTemp && (cleanupMode == "auto" || cleanupMode == "all")) ||
		(!inputIsTemp && cleanupMode == "all") {
		defer func() {
			if inputIsTemp {
				fmt.Printf("Cleaning up temporary input directory: %s\n", inputDir)
			} else {
				fmt.Printf("Cleaning up input directory: %s\n", inputDir)
			}
			os.RemoveAll(inputDir)
		}()
	}

	// Create context subdirectory in input directory
	contextDir := filepath.Join(inputDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		return fmt.Errorf("failed to create context directory: %w", err)
	}

	// Collect context based on type
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
			Owner:    solveRef.Owner,
			Repo:     solveRef.Repo,
			IssueNum: solveRef.Number,
			Token:    token,
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

	// Prepare workspace using the workspace prepare abstraction
	fmt.Println("\n" + strings.Repeat("=", 60))
	fmt.Println("Preparing workspace...")
	fmt.Println(strings.Repeat("=", 60))

	workspacePrep, err := prepareWorkspaceForSolve(ctx, solveRef, token)
	if err != nil {
		return fmt.Errorf("failed to prepare workspace: %w", err)
	}

	// Ensure cleanup happens for temporary workspaces
	if workspacePrep != nil && workspacePrep.cleanupNeeded {
		defer func() {
			fmt.Printf("\nCleaning up temporary workspace: %s\n", workspacePrep.path)
			if err := workspacePrep.preparer.Cleanup(workspacePrep.path); err != nil {
				fmt.Printf("Warning: failed to cleanup workspace: %v\n", err)
			}
		}()
	}

	// Set HOLON_WORKSPACE environment variable for publishers
	// This needs to be set before running holon so it's available in the environment
	if err := os.Setenv("HOLON_WORKSPACE", workspacePrep.path); err != nil {
		return fmt.Errorf("failed to set HOLON_WORKSPACE environment variable: %w", err)
	}

	// Resolve base image with auto-detection support
	resolvedImage, err := resolveSolveBaseImage(workspacePrep.path)
	if err != nil {
		return fmt.Errorf("failed to resolve base image: %w", err)
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
		GoalStr:         goal,
		TaskName:        fmt.Sprintf("solve-%s-%d", refType, solveRef.Number),
		BaseImage:       resolvedImage,
		AgentBundle:     solveAgent,
		WorkspacePath:   workspacePrep.path,
		ContextPath:     contextDir,
		InputPath:       inputDir,
		OutDir:          outDir,
		RoleName:        solveRole,
		LogLevel:        solveLogLevel,
		Mode:            solveMode,
		Cleanup:         cleanupMode,
		AgentConfigMode: solveAgentConfigMode,
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

	// Cleanup output directory if requested (after publishing completes)
	if cleanupMode == "all" {
		fmt.Printf("\nCleaning up output directory: %s\n", outDir)
		os.RemoveAll(outDir)
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

// getGitHubToken retrieves the GitHub token from environment variables or gh CLI
func getGitHubToken() (string, error) {
	token, fromGh := pkggithub.GetTokenFromEnv()
	if token == "" {
		return "", fmt.Errorf("GITHUB_TOKEN or HOLON_GITHUB_TOKEN environment variable is required (or use 'gh auth login')")
	}
	if fromGh {
		fmt.Fprintln(os.Stderr, "Using GitHub token from 'gh auth token' (GITHUB_TOKEN not set)")
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

// resolveSolveBaseImage resolves the base image for solve command with auto-detection.
// Precedence: CLI flag > project config > auto-detect > default.
func resolveSolveBaseImage(workspace string) (string, error) {
	// Load project config from workspace
	projectCfg, err := config.Load(workspace)
	if err != nil {
		fmt.Printf("Warning: failed to load project config: %v\n", err)
		projectCfg = &config.ProjectConfig{}
	}

	// Check CLI flag first
	if solveImage != "" {
		fmt.Printf("Config: base_image = %q (source: cli)\n", solveImage)
		return solveImage, nil
	}

	// Check project config (if not auto-detect)
	if projectCfg.BaseImage != "" && !projectCfg.ShouldAutoDetectImage() {
		fmt.Printf("Config: base_image = %q (source: config)\n", projectCfg.BaseImage)
		return projectCfg.BaseImage, nil
	}

	// Check if auto-detect is disabled
	if !solveImageAutoDetect && !projectCfg.ShouldAutoDetectImage() {
		// Use default
		fmt.Printf("Config: base_image = %q (source: default)\n", image.DefaultImage)
		return image.DefaultImage, nil
	}

	// Auto-detect from workspace
	detectResult := image.Detect(workspace)
	fmt.Printf("Config: %s\n", image.FormatResult(detectResult))
	return detectResult.Image, nil
}

func init() {
	solveCmd.Flags().StringVar(&solveRepo, "repo", "", "Default repository in owner/repo format (for numeric references)")
	solveCmd.Flags().StringVar(&solveBase, "base", "main", "Base branch for PR creation (issue mode only)")
	solveCmd.Flags().StringVarP(&solveOutDir, "out", "o", "./holon-output", "Output directory")
	solveCmd.Flags().StringVarP(&solveContext, "context", "c", "", "Additional context directory (deprecated)")
	solveCmd.Flags().StringVar(&solveInput, "input", "", "Input directory path (default: creates temp dir, auto-cleaned)")
	solveCmd.Flags().StringVar(&solveCleanup, "cleanup", "auto", "Cleanup mode: auto (clean temp input), none (keep all), all (clean input+output)")
	solveCmd.Flags().StringVar(&solveAgent, "agent", "", "Agent bundle reference")
	solveCmd.Flags().StringVarP(&solveImage, "image", "i", "", "Docker base image (default: auto-detect from workspace)")
	solveCmd.Flags().BoolVar(&solveImageAutoDetect, "image-auto-detect", true, "Enable automatic base image detection (default: true)")
	solveCmd.Flags().StringVar(&solveMode, "mode", "", "Execution mode (default: auto-detect from ref type)")
	solveCmd.Flags().StringVarP(&solveRole, "role", "r", "", "Role to assume")
	solveCmd.Flags().StringVar(&solveLogLevel, "log-level", "progress", "Log level")
	solveCmd.Flags().StringVar(&solveAgentConfigMode, "agent-config-mode", "auto", "Agent config mount mode: auto (mount if ~/.claude exists), yes (always mount, warn if missing), no (never mount)")
	solveCmd.Flags().BoolVar(&solveDryRun, "dry-run", false, "Validate without running (not yet implemented)")

	// Workspace preparation flags
	solveCmd.Flags().StringVar(&solveWorkspace, "workspace", "", "Workspace path (uses existing directory, no cloning)")
	solveCmd.Flags().StringVar(&solveWorkspaceRef, "workspace-ref", "", "Git ref to checkout (branch, tag, or SHA)")
	solveCmd.Flags().StringVar(&solveWorkspaceHistory, "workspace-history", "", "Git history mode: full, shallow, or none (default: full for local, shallow for remote)")
	solveCmd.Flags().BoolVar(&solveFetchRemote, "fetch-remote", false, "Fetch remote updates before solving (default: false)")

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

package main

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/holon-run/holon/pkg/github"
	"github.com/spf13/cobra"
	"gopkg.in/yaml.v3"
)

var (
	setupRepo      string
	setupOrg       string
	setupNonInteractive bool
	setupDryRun    bool
)

var setupGithubCmd = &cobra.Command{
	Use:   "github",
	Short: "Bootstrap GitHub integration for Holon",
	Long: `Setup GitHub integration for Holon by installing holonbot app, configuring workflows,
and setting up required secrets.

IMPORTANT: This command creates workflow files in the current directory under .github/workflows/
You should run this command from within your git repository directory so the files can be
committed and pushed to the repository.

This command guides you through:
1. Verifying prerequisites (gh CLI authentication, repo access)
2. Installing the holonbot GitHub App
3. Creating workflow files from templates
4. Configuring required secrets/vars
5. Validating the setup

Example usage:
  holon setup github                    # Interactive setup for current repo
  holon setup github --repo owner/repo  # Setup for specific repo
  holon setup github --org myorg        # Setup for organization
  holon setup github --non-interactive  # Automated setup (requires admin rights)
  holon setup github --dry-run          # Show commands without executing`,
	RunE: func(cmd *cobra.Command, args []string) error {
		return runSetupGithub(cmd.Context())
	},
}

// setupConfig holds the configuration for GitHub setup
type setupConfig struct {
	owner         string
	repo          string
	isOrg         bool
	workflowPath  string
	installURL    string
	appInstalled  bool
	dryRun        bool
	nonInteractive bool
}

// workflowTemplate represents the structure of the workflow template
type workflowTemplate struct {
	Name       string                 `yaml:"name"`
	On         map[string]interface{} `yaml:"on"`
	Permission map[string]string      `yaml:"permissions"`
	Jobs       map[string]interface{} `yaml:"jobs"`
}

// requiredWorkflowPermissions defines the required permissions for the holon workflow
var requiredWorkflowPermissions = map[string]string{
	"contents":      "write",
	"issues":        "write",
	"pull-requests": "write",
	"id-token":      "write",
}

func runSetupGithub(ctx context.Context) error {
	// Initialize config
	cfg := &setupConfig{
		dryRun:         setupDryRun,
		nonInteractive: setupNonInteractive,
		installURL:     "https://github.com/apps/holonbot",
	}

	// Step 1: Verify prerequisites
	if err := verifyPrerequisites(cfg); err != nil {
		return fmt.Errorf("prerequisite check failed: %w", err)
	}

	// Step 2: Detect/resolve owner and repo
	if err := resolveRepoInfo(cfg); err != nil {
		return fmt.Errorf("failed to resolve repo info: %w", err)
	}

	// Step 3: Check holonbot installation
	if err := checkHolonbotInstallation(ctx, cfg); err != nil {
		return fmt.Errorf("failed to check holonbot installation: %w", err)
	}

	// Step 4: Create workflow file
	if err := createWorkflowFile(cfg); err != nil {
		return fmt.Errorf("failed to create workflow file: %w", err)
	}

	// Step 5: Configure secrets
	if err := configureSecrets(ctx, cfg); err != nil {
		return fmt.Errorf("failed to configure secrets: %w", err)
	}

	// Step 6: Validate setup
	if err := validateSetup(ctx, cfg); err != nil {
		return fmt.Errorf("setup validation failed: %w", err)
	}

	// Print summary
	printSetupSummary(cfg)

	return nil
}

// verifyPrerequisites checks that gh CLI is installed and authenticated
func verifyPrerequisites(cfg *setupConfig) error {
	fmt.Println("✓ Step 1: Verifying prerequisites")

	// Check if gh CLI is installed
	_, err := exec.LookPath("gh")
	if err != nil {
		return fmt.Errorf("gh CLI not found: please install from https://cli.github.com/")
	}

	// Check gh auth status
	cmd := exec.Command("gh", "auth", "status")
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("gh CLI not authenticated: run 'gh auth login' first\nOutput: %s", string(output))
	}

	fmt.Println("  ✓ gh CLI installed and authenticated")
	fmt.Printf("  %s\n", strings.TrimSpace(string(output)))

	return nil
}

// resolveRepoInfo determines the owner and repo from flags or git remote
func resolveRepoInfo(cfg *setupConfig) error {
	fmt.Println("\n✓ Step 2: Resolving repository information")

	// If --repo flag is provided
	if setupRepo != "" {
		parts := strings.Split(setupRepo, "/")
		if len(parts) != 2 {
			return fmt.Errorf("invalid repo format: expected 'owner/repo', got '%s'", setupRepo)
		}
		cfg.owner = parts[0]
		cfg.repo = parts[1]
		fmt.Printf("  ✓ Using specified repo: %s/%s\n", cfg.owner, cfg.repo)
		return nil
	}

	// If --org flag is provided, we'll use it for org-level setup
	if setupOrg != "" {
		cfg.owner = setupOrg
		cfg.isOrg = true
		fmt.Printf("  ✓ Using org: %s (org-level setup)\n", cfg.owner)
		// For org-level setup, we still need a repo for workflow files
		// Try to get it from git remote
		repo, err := getRepoFromGitRemote()
		if err != nil {
			return fmt.Errorf("org-level setup requires --repo flag or git repo: %w", err)
		}
		// Parse owner/repo to extract only the repo name
		parts := strings.Split(repo, "/")
		if len(parts) != 2 {
			return fmt.Errorf("invalid repo format from git remote: %s", repo)
		}
		// Keep cfg.owner as the specified org; only use remote to infer repo name
		cfg.repo = parts[1]
		return nil
	}

	// Try to get from git remote
	repo, err := getRepoFromGitRemote()
	if err != nil {
		return fmt.Errorf("could not determine repo: please specify --repo owner/repo or run from a git repo")
	}

	// Parse owner/repo from git remote
	parts := strings.Split(repo, "/")
	if len(parts) != 2 {
		return fmt.Errorf("invalid repo format from git remote: %s", repo)
	}

	cfg.owner = parts[0]
	cfg.repo = parts[1]
	fmt.Printf("  ✓ Detected repo from git remote: %s/%s\n", cfg.owner, cfg.repo)

	return nil
}

// getRepoFromGitRemote tries to get the repo from git remote
func getRepoFromGitRemote() (string, error) {
	cmd := exec.Command("git", "remote", "get-url", "origin")
	output, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("not in a git repo or no origin remote: %w", err)
	}

	remote := strings.TrimSpace(string(output))

	// Convert SSH URL to HTTPS format if needed
	// git@github.com:owner/repo.git -> https://github.com/owner/repo
	remote = strings.TrimPrefix(remote, "git@github.com:")
	remote = strings.TrimPrefix(remote, "https://github.com/")
	remote = strings.TrimPrefix(remote, "http://github.com/")
	remote = strings.TrimSuffix(remote, ".git")

	if !strings.Contains(remote, "/") {
		return "", fmt.Errorf("could not parse repo from remote: %s", remote)
	}

	return remote, nil
}

// checkHolonbotInstallation checks if holonbot is installed and guides installation if needed
func checkHolonbotInstallation(ctx context.Context, cfg *setupConfig) error {
	fmt.Println("\n✓ Step 3: Checking holonbot installation")

	// Try to get a GitHub client to check installation
	client, err := github.NewClientFromEnv()
	if err != nil {
		return fmt.Errorf("failed to create GitHub client: %w", err)
	}

	// Check if app is installed by trying to get app info
	// We'll use a simple check: try to get the repo which requires auth
	// Note: This is an authentication check, not a holonbot verification.
	// We verify that the user has access to the repo (authentication),
	// but we don't actually check if holonbot is installed here.
	_, _, err = client.GitHubClient().Repositories.Get(ctx, cfg.owner, cfg.repo)
	if err != nil {
		return fmt.Errorf("failed to access repo %s/%s: %w (check permissions)", cfg.owner, cfg.repo, err)
	}

	// For now, we'll ask the user to verify
	// In a future enhancement, we could use the GitHub API to check installations
	fmt.Println("  ℹ Holonbot app installation check")
	fmt.Printf("  ℹ Install URL: %s\n", cfg.installURL)

	if cfg.nonInteractive {
		fmt.Println("  ✓ Skipping interactive check (non-interactive mode)")
		cfg.appInstalled = true // Assume installed in non-interactive mode
		return nil
	}

	fmt.Printf("\n  Has holonbot been installed for %s/%s? [Y/n] ", cfg.owner, cfg.repo)
	reader := bufio.NewReader(os.Stdin)
	input, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("failed to read input: %w", err)
	}

	input = strings.TrimSpace(strings.ToLower(input))
	if input == "" || input == "y" || input == "yes" {
		cfg.appInstalled = true
		fmt.Println("  ✓ Holonbot installed")
	} else {
		cfg.appInstalled = false
		fmt.Println("  ⚠ Holonbot not installed")
		fmt.Printf("  Please install from: %s\n", cfg.installURL)
		fmt.Println("  Then run this command again to continue setup.")
		return fmt.Errorf("holonbot not installed")
	}

	return nil
}

// createWorkflowFile creates the workflow file from template
func createWorkflowFile(cfg *setupConfig) error {
	fmt.Println("\n✓ Step 4: Creating workflow file")

	// Note: Workflow files are created in the current directory under .github/workflows/
	// Users should run this command from within their git repository directory
	// so the workflow file can be committed and pushed to the repository.

	// Define workflow paths
	workflowDir := ".github/workflows"
	workflowFile := filepath.Join(workflowDir, "holon-trigger.yml")

	// Check if file exists
	if _, err := os.Stat(workflowFile); err == nil {
		fmt.Printf("  ⚠ Workflow file already exists: %s\n", workflowFile)

		if cfg.nonInteractive {
			fmt.Println("  ⚠ Skipping: existing file will not be overwritten in non-interactive mode. Re-run without --non-interactive (and optionally with --dry-run) to review or overwrite.")
			return nil
		}

		fmt.Printf("  Options: [s]kip, [o]verwrite? ")
		reader := bufio.NewReader(os.Stdin)
		input, err := reader.ReadString('\n')
		if err != nil {
			return fmt.Errorf("failed to read input: %w", err)
		}

		input = strings.TrimSpace(strings.ToLower(input))
		switch input {
		case "s", "skip":
			fmt.Println("  ✓ Skipping workflow file creation")
			return nil
		case "o", "overwrite":
			fmt.Println("  ⚠ Will overwrite existing file")
		default:
			fmt.Println("  ✓ Skipping workflow file creation")
			return nil
		}
	}

	// Read template from examples
	templatePath := filepath.Join("examples", "workflows", "holon-trigger.yml")
	templateContent, err := os.ReadFile(templatePath)
	if err != nil {
		return fmt.Errorf("failed to read workflow template from %s: %w", templatePath, err)
	}

	// Validate that it's a proper YAML with required permissions
	var workflow workflowTemplate
	if err := yaml.Unmarshal(templateContent, &workflow); err != nil {
		return fmt.Errorf("failed to parse workflow template: %w", err)
	}

	// Check for required permissions
	requiredPerms := requiredWorkflowPermissions

	missingPerms := []string{}
	for perm, access := range requiredPerms {
		if workflow.Permission[perm] != access {
			missingPerms = append(missingPerms, fmt.Sprintf("%s: %s", perm, access))
		}
	}

	if len(missingPerms) > 0 {
		return fmt.Errorf("workflow template missing required permissions: %v", missingPerms)
	}

	// Create workflow directory
	if err := os.MkdirAll(workflowDir, 0755); err != nil {
		return fmt.Errorf("failed to create workflow directory: %w", err)
	}

	// Dry run: just print what would be done
	if cfg.dryRun {
		fmt.Printf("  [DRY-RUN] Would create: %s\n", workflowFile)
		fmt.Printf("  [DRY-RUN] Content:\n%s\n", string(templateContent))
		return nil
	}

	// Write workflow file
	if err := os.WriteFile(workflowFile, templateContent, 0644); err != nil {
		return fmt.Errorf("failed to write workflow file: %w", err)
	}

	// Only set workflowPath when the file is actually created or updated
	cfg.workflowPath = workflowFile
	fmt.Printf("  ✓ Created workflow file: %s\n", workflowFile)

	return nil
}

// configureSecrets sets up required secrets and vars
func configureSecrets(ctx context.Context, cfg *setupConfig) error {
	fmt.Println("\n✓ Step 5: Configuring secrets")

	// Define required and optional secrets
	secrets := []struct {
		name     string
		required bool
		prompt   string
	}{
		{"ANTHROPIC_AUTH_TOKEN", true, "Anthropic API token"},
		{"ANTHROPIC_BASE_URL", false, "Anthropic base URL (optional)"},
	}

	vars := []struct {
		name     string
		required bool
		prompt   string
	}{
		{"HOLON_MODEL", false, "Holon model (optional)"},
		{"HOLON_FALLBACK_MODEL", false, "Holon fallback model (optional)"},
	}

	// Process secrets
	for _, secret := range secrets {
		if err := configureSecretOrVar(cfg, secret.name, secret.required, secret.prompt, "secret"); err != nil {
			return err
		}
	}

	// Process vars
	for _, v := range vars {
		if err := configureSecretOrVar(cfg, v.name, v.required, v.prompt, "var"); err != nil {
			return err
		}
	}

	return nil
}

// configureSecretOrVar configures a single secret or var
func configureSecretOrVar(cfg *setupConfig, name string, required bool, prompt, kind string) error {
	fmt.Printf("\n  Configuring %s: %s\n", kind, name)
	fmt.Printf("  Description: %s\n", prompt)

	if !required {
		fmt.Printf("  This is optional. Press Enter to skip.\n")
	}

	if cfg.nonInteractive {
		fmt.Printf("  Skipping in non-interactive mode\n")
		fmt.Printf("  To set manually, run:\n")
		if kind == "secret" {
			fmt.Printf("    echo \"your-value\" | gh secret set %s --repo %s/%s\n", name, cfg.owner, cfg.repo)
		} else {
			fmt.Printf("    gh variable set %s --repo %s/%s --body \"your-value\"\n", name, cfg.owner, cfg.repo)
		}
		return nil
	}

	fmt.Printf("  Enter value (or Enter to skip): ")
	reader := bufio.NewReader(os.Stdin)
	value, err := reader.ReadString('\n')
	if err != nil {
		return fmt.Errorf("failed to read input: %w", err)
	}

	value = strings.TrimSpace(value)

	if value == "" {
		if required {
			return fmt.Errorf("%s is required but was not provided", name)
		}
		fmt.Printf("  ✓ Skipped (optional)\n")
		return nil
	}

	// Dry run: just print the command
	if cfg.dryRun {
		if kind == "secret" {
			fmt.Printf("  [DRY-RUN] Would set secret: gh secret set %s --repo %s/%s\n", name, cfg.owner, cfg.repo)
		} else {
			fmt.Printf("  [DRY-RUN] Would set var: gh variable set %s --repo %s/%s\n", name, cfg.owner, cfg.repo)
		}
		return nil
	}

	// Set the secret or var using gh CLI
	var cmd *exec.Cmd
	if kind == "secret" {
		cmd = exec.Command("gh", "secret", "set", name, "--repo", fmt.Sprintf("%s/%s", cfg.owner, cfg.repo))
	} else {
		cmd = exec.Command("gh", "variable", "set", name, "--repo", fmt.Sprintf("%s/%s", cfg.owner, cfg.repo))
	}

	// Set input
	cmd.Stdin = strings.NewReader(value)
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("failed to set %s %s: %w\nOutput: %s", kind, name, err, string(output))
	}

	fmt.Printf("  ✓ Set %s: %s\n", kind, name)

	return nil
}

// validateSetup performs validation checks on the setup
func validateSetup(ctx context.Context, cfg *setupConfig) error {
	fmt.Println("\n✓ Step 6: Validating setup")

	issues := []string{}

	// Check workflow file exists and validate it only if a workflow path was configured
	if cfg.workflowPath != "" {
		if _, err := os.Stat(cfg.workflowPath); err != nil {
			issues = append(issues, fmt.Sprintf("workflow file missing: %s", cfg.workflowPath))
		} else {
			fmt.Println("  ✓ Workflow file present")

			// Validate workflow file permissions
			if content, err := os.ReadFile(cfg.workflowPath); err == nil {
				var workflow workflowTemplate
				if err := yaml.Unmarshal(content, &workflow); err != nil {
					issues = append(issues, "workflow file has invalid YAML")
				} else {
					allPresent := true
					for perm, access := range requiredWorkflowPermissions {
						if workflow.Permission[perm] != access {
							allPresent = false
							issues = append(issues, fmt.Sprintf("workflow missing permission: %s: %s", perm, access))
						}
					}
					if allPresent {
						fmt.Println("  ✓ Workflow permissions correct")
					}
				}
			}
		}
	} else {
		fmt.Println("  ℹ No workflow file was configured; skipping workflow validation")
	}

	// Check holonbot installation
	if !cfg.appInstalled {
		issues = append(issues, "holonbot app not installed")
	} else {
		fmt.Println("  ✓ Holonbot app installed")
	}

	// Note: We can't easily check if secrets are set without trying to use them
	// In non-interactive mode, we trust the user set them correctly
	fmt.Println("  ℹ Secrets/vars: verify manually with 'gh secret list' and 'gh variable list'")

	if len(issues) > 0 {
		fmt.Printf("\n  ⚠ Validation found %d issue(s):\n", len(issues))
		for _, issue := range issues {
			fmt.Printf("    - %s\n", issue)
		}
		return fmt.Errorf("validation failed with %d issue(s)", len(issues))
	}

	fmt.Println("  ✓ Setup validation complete")
	return nil
}

// printSetupSummary prints a summary of what was done
func printSetupSummary(cfg *setupConfig) {
	fmt.Println("\n" + strings.Repeat("=", 60))
	fmt.Println("✓ Setup complete!")
	fmt.Println(strings.Repeat("=", 60))

	fmt.Printf("\nRepository: %s/%s\n", cfg.owner, cfg.repo)
	fmt.Printf("Workflow: %s\n", cfg.workflowPath)
	fmt.Printf("Holonbot: %s\n", map[bool]string{true: "Installed", false: "Not installed"}[cfg.appInstalled])

	fmt.Println("\nNext steps:")
	fmt.Println("  1. Commit the workflow file:")
	fmt.Println("     git add .github/workflows/holon-trigger.yml")
	fmt.Println("     git commit -m 'Add holon-trigger workflow'")
	fmt.Println("     git push")

	fmt.Println("\n  2. Verify secrets are set:")
	fmt.Printf("     gh secret list --repo %s/%s\n", cfg.owner, cfg.repo)

	fmt.Println("\n  3. Trigger a run by commenting '@holonbot' on an issue or PR")

	fmt.Printf("\nFor more information, see: https://github.com/holon-run/holon\n")
}

func init() {
	// Add setup command
	setupCmd := &cobra.Command{
		Use:   "setup",
		Short: "Setup Holon integrations",
	}
	setupCmd.AddCommand(setupGithubCmd)

	// Flags for setup github
	setupGithubCmd.Flags().StringVar(&setupRepo, "repo", "",
		"Repository in 'owner/repo' format")
	setupGithubCmd.Flags().StringVar(&setupOrg, "org", "",
		"Organization for org-level setup")
	setupGithubCmd.Flags().BoolVar(&setupNonInteractive, "non-interactive", false,
		"Run without prompting (requires all configuration to be pre-set)")
	setupGithubCmd.Flags().BoolVar(&setupDryRun, "dry-run", false,
		"Show what would be done without making changes")

	// Register setup command with root
	rootCmd.AddCommand(setupCmd)
}

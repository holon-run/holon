package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"

	v1 "github.com/holon-run/holon/pkg/api/v1"
	"github.com/holon-run/holon/pkg/publisher"
	"github.com/holon-run/holon/pkg/publisher/providers"
	"github.com/spf13/cobra"
)

var publishOutDir string
var publishDryRun bool
var publishResultPath string

var publishCmd = &cobra.Command{
	Use:   "publish <provider> <target>",
	Short: "Publish Holon outputs to external systems",
	Long: `Publish Holon outputs to external systems like GitHub, git repositories, etc.

The publish command takes the results from a Holon execution and publishes them to the
specified target using the chosen provider. Each provider may require different files
from the output directory.

The command validates that the output directory exists. Individual providers are
responsible for validating their required files (e.g., a git provider may require
diff.patch, while a GitHub PR provider may require summary.md for posting as a comment).

Examples:
  holon publish mock example-target --out ./holon-output
  holon publish git origin/main --out ./holon-output
  holon publish github https://github.com/owner/repo/pull/123 --out ./holon-output

Available providers:
  - mock: Mock publisher for testing (no actual changes)
  - git: Apply diff and push to git branch (see #174)
  - github: Publish to GitHub PRs and issues (see #175)

The publish command writes a publish-result.json file in the output directory
containing details about the actions taken and any errors that occurred.`,
	Args: cobra.ExactArgs(2),
	RunE: func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()

		providerName := args[0]
		target := args[1]

		// Create registry with mock provider for now
		// Real providers will be registered once implemented
		registry := publisher.NewRegistry()

		// Register mock provider for testing
		mockPub := providers.NewMockPublisher("mock")
		if err := registry.Register(mockPub); err != nil {
			return fmt.Errorf("failed to register mock publisher: %w", err)
		}

		// Register stub providers (will return "not implemented" errors)
		// These are here for documentation purposes and to validate provider names
		if err := registry.Register(providers.NewGitPublisher()); err != nil {
			fmt.Fprintf(os.Stderr, "warning: failed to register git publisher: %v\n", err)
		}
		if err := registry.Register(providers.NewGitHubPublisher()); err != nil {
			fmt.Fprintf(os.Stderr, "warning: failed to register github publisher: %v\n", err)
		}

		// Resolve output directory
		outDir := publishOutDir
		if outDir == "" {
			outDir = "./holon-output"
		}

		absOutDir, err := filepath.Abs(outDir)
		if err != nil {
			return fmt.Errorf("failed to resolve output directory: %w", err)
		}

		// Verify output directory exists
		if _, err := os.Stat(absOutDir); err != nil {
			if os.IsNotExist(err) {
				return fmt.Errorf("output directory does not exist: %s", absOutDir)
			}
			return fmt.Errorf("failed to access output directory: %w", err)
		}

		// Create publish request
		req := v1.PublishRequest{
			Provider:  providerName,
			Target:    target,
			OutputDir: absOutDir,
			DryRun:    publishDryRun,
		}

		// Execute publish
		fmt.Printf("Publishing to %s using provider '%s'...\n", target, providerName)
		if publishDryRun {
			fmt.Println("Dry-run mode: no actual changes will be made")
		}

		result, err := registry.Publish(ctx, req)
		if err != nil {
			return fmt.Errorf("publish failed: %w", err)
		}

		// Determine result path
		resultPath := publishResultPath
		if resultPath == "" {
			resultPath = filepath.Join(absOutDir, "publish-result.json")
		}

		// Write publish result
		resultData, err := json.MarshalIndent(result, "", "  ")
		if err != nil {
			return fmt.Errorf("failed to marshal publish result: %w", err)
		}

		if err := os.WriteFile(resultPath, resultData, 0644); err != nil {
			return fmt.Errorf("failed to write publish result: %w", err)
		}

		// Print summary
		fmt.Printf("\nPublish completed at %s\n", result.PublishedAt.Format(time.RFC3339))
		fmt.Printf("Provider: %s\n", result.Provider)
		fmt.Printf("Target: %s\n", result.Target)
		fmt.Printf("Result written to: %s\n", resultPath)

		if len(result.Actions) > 0 {
			fmt.Printf("\nActions taken (%d):\n", len(result.Actions))
			for i, action := range result.Actions {
				fmt.Printf("  %d. %s: %s\n", i+1, action.Type, action.Description)
			}
		}

		if len(result.Errors) > 0 {
			fmt.Printf("\nErrors (%d):\n", len(result.Errors))
			for i, e := range result.Errors {
				fmt.Printf("  %d. %s", i+1, e.Message)
				if e.Context != "" {
					fmt.Printf(" (context: %s)", e.Context)
				}
				fmt.Println()
			}
		}

		return nil
	},
}

func init() {
	publishCmd.Flags().StringVarP(&publishOutDir, "out", "o", "./holon-output", "Path to Holon output directory")
	publishCmd.Flags().BoolVar(&publishDryRun, "dry-run", false, "Simulate publish without making changes")
	publishCmd.Flags().StringVar(&publishResultPath, "result-path", "", "Path to write publish-result.json (default: <out-dir>/publish-result.json)")
}

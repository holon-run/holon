package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"

	"github.com/holon-run/holon/pkg/api/v1"
	"github.com/holon-run/holon/pkg/publisher"
	"github.com/spf13/cobra"
)

var (
	publishProvider string
	publishTarget   string
	publishOutDir   string
	publishDryRun   bool
)

var publishCmd = &cobra.Command{
	Use:   "publish",
	Short: "Publish Holon execution outputs",
	Long: `Publish Holon execution outputs to external systems like GitHub, git, etc.

This command reads the output from a Holon run (manifest.json and artifacts)
and publishes them using the specified provider.

Examples:
  holon publish --provider github --target holon-run/holon/pr/123
  holon publish --provider git --target origin/main --out ./holon-output
  holon publish --provider github --target holon-run/holon/pr/123 --dry-run`,
	RunE: func(cmd *cobra.Command, args []string) error {
		// Validate required flags
		if publishProvider == "" {
			return fmt.Errorf("\"provider\" not set")
		}
		if publishTarget == "" {
			return fmt.Errorf("\"target\" not set")
		}

		// Get the publisher
		p := publisher.Get(publishProvider)
		if p == nil {
			// Check if there are any publishers at all
			list := publisher.List()
			if len(list) == 0 {
				return fmt.Errorf("no publishers available")
			}
			return fmt.Errorf("publisher '%s' not found", publishProvider)
		}

		// Read manifest from output directory
		manifestPath := filepath.Join(publishOutDir, "manifest.json")
		manifestData, err := os.ReadFile(manifestPath)
		if err != nil {
			return fmt.Errorf("failed to read manifest.json: %w", err)
		}

		var manifest v1.HolonManifest
		if err := json.Unmarshal(manifestData, &manifest); err != nil {
			return fmt.Errorf("failed to parse manifest.json: %w", err)
		}

		// Build artifacts map from manifest
		artifacts := make(map[string]string)
		for _, artifactPath := range manifest.Artifacts {
			fullPath := filepath.Join(publishOutDir, artifactPath)
			// Convert to absolute path to handle cases where working directory changes
			absPath, err := filepath.Abs(fullPath)
			if err != nil {
				// Fallback to relative path if absolute path conversion fails
				absPath = fullPath
			}
			artifacts[artifactPath] = absPath
		}

		// Convert manifest to map[string]interface{} for request
		manifestMap := make(map[string]interface{})
		manifestData, _ = json.Marshal(manifest)
		if err := json.Unmarshal(manifestData, &manifestMap); err != nil {
			return fmt.Errorf("failed to convert manifest: %w", err)
		}

		// Create publish request
		req := publisher.PublishRequest{
			Target:   publishTarget,
			OutputDir: publishOutDir,
			Manifest:  manifestMap,
			Artifacts: artifacts,
		}

		// Validate the request
		if err := p.Validate(req); err != nil {
			return fmt.Errorf("validation failed: %w", err)
		}

		// If dry-run, just validate and exit
		if publishDryRun {
			fmt.Println("Dry run: validation passed")
			return nil
		}

		// Publish
		result, err := p.Publish(req)
		if err != nil {
			return fmt.Errorf("publish failed: %w", err)
		}

		// Write result to file
		if err := publisher.WriteResult(publishOutDir, result); err != nil {
			return fmt.Errorf("failed to write publish result: %w", err)
		}

		// Print summary
		if result.Success {
			fmt.Printf("Successfully published to %s using provider '%s'\n", result.Target, result.Provider)
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
	},
}

var publishListCmd = &cobra.Command{
	Use:   "list",
	Short: "List available publishers",
	Long: `List all registered publishers. Publishers are plugins that can
publish Holon outputs to external systems.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		list := publisher.List()
		if len(list) == 0 {
			fmt.Println("No publishers are currently registered")
			return nil
		}

		// Sort alphabetically
		sort.Strings(list)

		fmt.Println("Available publishers:")
		for _, name := range list {
			fmt.Printf("  - %s\n", name)
		}

		return nil
	},
}

var publishInfoCmd = &cobra.Command{
	Use:   "info <provider>",
	Short: "Show information about a publisher",
	Long: `Show detailed information about a specific publisher, including
its name and capabilities.`,
	Args: cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]
		p := publisher.Get(name)
		if p == nil {
			return fmt.Errorf("publisher '%s' not found", name)
		}

		fmt.Printf("Publisher: %s\n", p.Name())
		fmt.Println("\nThis publisher can be used with:")
		fmt.Printf("  holon publish --provider %s --target <target>\n", name)

		return nil
	},
}

func init() {
	publishCmd.Flags().StringVar(&publishProvider, "provider", "", "Publisher name (required)")
	publishCmd.Flags().StringVar(&publishTarget, "target", "", "Publish target (required)")
	publishCmd.Flags().StringVarP(&publishOutDir, "output", "O", "./holon-output", "Output directory")
	_ = publishCmd.Flags().MarkDeprecated("out", "use --output instead")
	publishCmd.Flags().StringVarP(&publishOutDir, "out", "o", "./holon-output", "Deprecated: use --output")
	publishCmd.Flags().BoolVar(&publishDryRun, "dry-run", false, "Validate without publishing")

	publishCmd.AddCommand(publishListCmd)
	publishCmd.AddCommand(publishInfoCmd)
}

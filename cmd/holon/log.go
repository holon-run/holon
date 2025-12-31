package main

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/holon-run/holon/pkg/logview"
	"github.com/spf13/cobra"
)

var (
	logViewOutput string
	logViewPath   string
)

var logCmd = &cobra.Command{
	Use:   "log",
	Short: "View and analyze execution logs",
	Long: `View and analyze execution logs from Holon runs.

This command provides structured viewing of execution logs with agent-specific
parsing for better readability.`,
}

var logViewCmd = &cobra.Command{
	Use:   "view",
	Short: "View execution log in a readable format",
	Long: `View execution log in a readable, structured format.

The command loads the manifest.json from the output directory to determine
which agent was used, then applies the appropriate parser for that agent.

If the agent parser is not available, it falls back to raw text output.

Examples:
  holon log view --output ./holon-output
  holon log view --path ./holon-output/evidence/execution.log
  holon log view -O ./my-run-output`,
	RunE: func(cmd *cobra.Command, args []string) error {
		// Determine which path to use
		var logContent string
		var err error

		if logViewPath != "" {
			// Direct log path - use raw parsing
			logContent, err = parseLogDirect(logViewPath)
			if err != nil {
				return fmt.Errorf("failed to parse log: %w", err)
			}
		} else if logViewOutput != "" {
			// Output directory - use manifest-based parsing
			logContent, err = parseLogFromOutput(logViewOutput)
			if err != nil {
				return fmt.Errorf("failed to parse log: %w", err)
			}
		} else {
			return fmt.Errorf("either --output or --path must be specified")
		}

		// Output to stdout
		fmt.Print(logContent)
		return nil
	},
}

// parseLogFromOutput parses the log using manifest-based agent detection
func parseLogFromOutput(outputDir string) (string, error) {
	// Resolve to absolute path
	absOutput, err := filepath.Abs(outputDir)
	if err != nil {
		return "", fmt.Errorf("failed to resolve output path: %w", err)
	}

	// Check if manifest exists
	manifestPath := filepath.Join(absOutput, "manifest.json")
	if _, err := os.Stat(manifestPath); err != nil {
		return "", fmt.Errorf("manifest not found at %s", manifestPath)
	}

	// Use the log parser package
	return logview.ParseLog(manifestPath)
}

// parseLogDirect parses the log file directly (raw output)
func parseLogDirect(logPath string) (string, error) {
	// Resolve to absolute path
	absPath, err := filepath.Abs(logPath)
	if err != nil {
		return "", fmt.Errorf("failed to resolve log path: %w", err)
	}

	// Check if file exists
	if _, err := os.Stat(absPath); err != nil {
		return "", fmt.Errorf("log file not found: %s", absPath)
	}

	// Use the log parser package (raw mode)
	return logview.ParseLogFromPath(absPath)
}

func init() {
	logViewCmd.Flags().StringVarP(&logViewOutput, "output", "O", "",
		"Path to output directory containing manifest.json")
	logViewCmd.Flags().StringVarP(&logViewPath, "path", "p", "",
		"Direct path to execution.log file")

	logCmd.AddCommand(logViewCmd)
}

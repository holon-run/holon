package main

import (
	"fmt"

	"github.com/charmbracelet/bubbletea"
	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/tui"
	"github.com/spf13/cobra"
)

var (
	tuiRPCURL string
)

var tuiCmd = &cobra.Command{
	Use:   "tui",
	Short: "Terminal UI for holon serve control plane",
	Long: `TUI provides a generic operator interface for holon serve.

The TUI connects to a running holon serve instance via its JSON-RPC control plane
and displays runtime status, logs, and controls for pause/resume operations.

This is a generic client that does not depend on any specific role or webhook configuration.`,
	Example: `  # Connect to local serve instance
  holon tui --rpc http://127.0.0.1:8080/rpc

  # Connect to remote instance
  holon tui --rpc http://example.com:8080/rpc`,
	RunE: func(cmd *cobra.Command, args []string) error {
		// Initialize logger for any errors
		logCfg := holonlog.Config{
			Level:  holonlog.LogLevel("info"),
			Format: "console",
		}
		if err := holonlog.Init(logCfg); err != nil {
			return fmt.Errorf("failed to initialize logger: %w", err)
		}
		defer holonlog.Sync()

		// Validate RPC URL
		if tuiRPCURL == "" {
			return fmt.Errorf("--rpc flag is required (e.g., --rpc http://127.0.0.1:8080/rpc)")
		}

		// Create RPC client
		client := tui.NewRPCClient(tuiRPCURL)

		// Test connection
		if err := client.TestConnection(); err != nil {
			return fmt.Errorf("failed to connect to RPC endpoint %s: %w", tuiRPCURL, err)
		}

		holonlog.Info("connected to serve", "url", tuiRPCURL)

		// Create and run TUI app
		app := tui.NewApp(client)
		p := tea.NewProgram(app, tea.WithAltScreen())

		if _, err := p.Run(); err != nil {
			return fmt.Errorf("failed to run TUI: %w", err)
		}

		return nil
	},
}

func init() {
	tuiCmd.Flags().StringVar(&tuiRPCURL, "rpc", "", "RPC endpoint URL (e.g., http://127.0.0.1:8080/rpc)")
	rootCmd.AddCommand(tuiCmd)
}

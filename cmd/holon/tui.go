package main

import (
	"fmt"

	"github.com/charmbracelet/bubbletea"
	"github.com/holon-run/holon/pkg/serve"
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
	Example: `  # Connect to local serve instance (default RPC URL)
  holon tui

  # Connect to local serve instance explicitly
  holon tui --rpc http://127.0.0.1:8080/rpc

  # Connect to remote instance
  holon tui --rpc http://example.com:8080/rpc`,
	RunE: func(cmd *cobra.Command, args []string) error {
		// Set default RPC URL if not provided
		if tuiRPCURL == "" {
			tuiRPCURL = fmt.Sprintf("http://127.0.0.1:%d/rpc", serve.DefaultPort)
		}

		// Create RPC client
		client := tui.NewRPCClient(tuiRPCURL)

		// Test connection
		if err := client.TestConnection(); err != nil {
			return fmt.Errorf("failed to connect to RPC endpoint %s: %w", tuiRPCURL, err)
		}

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
	tuiCmd.Flags().StringVar(&tuiRPCURL, "rpc", "", fmt.Sprintf("RPC endpoint URL (default: http://127.0.0.1:%d/rpc)", serve.DefaultPort))
	rootCmd.AddCommand(tuiCmd)
}

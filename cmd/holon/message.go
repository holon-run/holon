package main

import (
	"fmt"
	"strings"

	"github.com/holon-run/holon/pkg/serve"
	"github.com/holon-run/holon/pkg/tui"
	"github.com/spf13/cobra"
)

var (
	messageRPCURL   string
	messageThreadID string
	messageBody     string
)

var messageCmd = &cobra.Command{
	Use:   "message",
	Short: "Send messages to a running holon serve instance",
}

var messageSendCmd = &cobra.Command{
	Use:   "send",
	Short: "Send one user message to a thread via turn/start",
	Long: `Send a single user message to a running holon serve instance over JSON-RPC.

This command is useful for automation and non-interactive testing when TUI is unavailable.`,
	Example: `  # Send a message to default local endpoint/thread
  holon message send -m "summarize current status"

  # Send to custom RPC endpoint and thread
  holon message send --rpc http://127.0.0.1:18081/rpc --thread main -m "hello"`,
	RunE: func(cmd *cobra.Command, args []string) error {
		body := strings.TrimSpace(messageBody)
		if body == "" {
			return fmt.Errorf("--message is required")
		}

		rpcURL := strings.TrimSpace(messageRPCURL)
		if rpcURL == "" {
			rpcURL = fmt.Sprintf("http://127.0.0.1:%d/rpc", serve.DefaultPort)
		}

		threadID := strings.TrimSpace(messageThreadID)
		if threadID == "" {
			threadID = "main"
		}

		client := tui.NewRPCClient(rpcURL)
		if err := client.TestConnection(); err != nil {
			return fmt.Errorf("failed to connect to RPC endpoint %s: %w", rpcURL, err)
		}

		resp, err := client.StartTurn(threadID, body)
		if err != nil {
			return fmt.Errorf("failed to send message to thread %s: %w", threadID, err)
		}

		fmt.Printf("turn started: thread=%s turn_id=%s state=%s started_at=%s\n", threadID, resp.TurnID, resp.State, resp.StartedAt)
		return nil
	},
}

func init() {
	messageSendCmd.Flags().StringVar(&messageRPCURL, "rpc", "", fmt.Sprintf("RPC endpoint URL (default: http://127.0.0.1:%d/rpc)", serve.DefaultPort))
	messageSendCmd.Flags().StringVar(&messageThreadID, "thread", "main", "Thread/session key")
	messageSendCmd.Flags().StringVarP(&messageBody, "message", "m", "", "User message text")
	_ = messageSendCmd.MarkFlagRequired("message")

	messageCmd.AddCommand(messageSendCmd)
	rootCmd.AddCommand(messageCmd)
}

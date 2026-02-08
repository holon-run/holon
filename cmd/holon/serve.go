package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"

	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/serve"
	"github.com/spf13/cobra"
)

var (
	serveRepo      string
	serveInput     string
	serveStateDir  string
	serveMaxEvents int
	serveDryRun    bool
	serveLogLevel  string
)

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Run proactive event-driven controller",
	Long: `Run an event-driven proactive controller loop.

The command reads event JSON (one object per line) from stdin by default,
normalizes events into an internal envelope, writes controller logs, decides
actions, and dispatches solve/review/fix runs.`,
	RunE: func(cmd *cobra.Command, _ []string) error {
		logCfg := holonlog.Config{
			Level:  holonlog.LogLevel(serveLogLevel),
			Format: "console",
		}
		if err := holonlog.Init(logCfg); err != nil {
			return fmt.Errorf("failed to initialize logger: %w", err)
		}
		defer holonlog.Sync()

		stateDir := serveStateDir
		if stateDir == "" {
			stateDir = filepath.Join(".holon", "serve-state")
		}
		absStateDir, err := filepath.Abs(stateDir)
		if err != nil {
			return fmt.Errorf("failed to resolve state dir: %w", err)
		}

		reader, closer, err := openServeInput(serveInput)
		if err != nil {
			return err
		}
		if closer != nil {
			defer closer.Close()
		}

		executor, err := newCLISolveExecutor(serveRepo, serveDryRun)
		if err != nil {
			return err
		}

		svc, err := serve.New(serve.Config{
			RepoHint: serveRepo,
			StateDir: absStateDir,
			Exec:     executor,
		})
		if err != nil {
			return err
		}
		defer svc.Close()

		holonlog.Info("serve started", "repo", serveRepo, "state_dir", absStateDir, "input", serveInput)
		return svc.Run(context.Background(), reader, serveMaxEvents)
	},
}

func openServeInput(input string) (io.Reader, io.Closer, error) {
	if input == "" || input == "-" {
		return os.Stdin, nil, nil
	}
	f, err := os.Open(input)
	if err != nil {
		return nil, nil, fmt.Errorf("failed to open serve input: %w", err)
	}
	return f, f, nil
}

type cliSolveExecutor struct {
	execPath string
	repoHint string
	dryRun   bool
}

func newCLISolveExecutor(repoHint string, dryRun bool) (*cliSolveExecutor, error) {
	execPath, err := os.Executable()
	if err != nil {
		return nil, fmt.Errorf("failed to resolve current executable: %w", err)
	}
	return &cliSolveExecutor{
		execPath: execPath,
		repoHint: repoHint,
		dryRun:   dryRun,
	}, nil
}

func (e *cliSolveExecutor) Execute(ctx context.Context, action serve.ActionIntent) error {
	if action.Type == "wait" || action.Type == "skip" {
		return nil
	}
	targetID, err := strconv.Atoi(action.Target.ID)
	if err != nil {
		return fmt.Errorf("invalid target id %q: %w", action.Target.ID, err)
	}
	ref := fmt.Sprintf("%s#%d", action.Target.Repo, targetID)

	args := []string{}
	switch action.Type {
	case "run_issue_solve":
		args = []string{"solve", "issue", ref, "--skill", "github-issue-solve"}
	case "run_pr_review":
		args = []string{"solve", "pr", ref, "--skill", "github-review"}
	case "run_pr_fix":
		args = []string{"solve", "pr", ref, "--skill", "github-pr-fix"}
	default:
		return fmt.Errorf("unsupported action type: %s", action.Type)
	}

	if e.repoHint != "" {
		args = append(args, "--repo", e.repoHint)
	}
	if e.dryRun {
		holonlog.Info("serve dry-run action", "type", action.Type, "args", args)
		return nil
	}

	cmd := exec.CommandContext(ctx, e.execPath, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Stdin = nil
	return cmd.Run()
}

func init() {
	serveCmd.Flags().StringVar(&serveRepo, "repo", "", "Default repository in owner/repo format")
	serveCmd.Flags().StringVar(&serveInput, "input", "-", "Input source for events ('-' for stdin, or path to file)")
	serveCmd.Flags().StringVar(&serveStateDir, "state-dir", "", "State directory (default: .holon/serve-state)")
	serveCmd.Flags().IntVar(&serveMaxEvents, "max-events", 0, "Stop after processing N events (0 = unlimited)")
	serveCmd.Flags().BoolVar(&serveDryRun, "dry-run", false, "Log actions without executing solve commands")
	serveCmd.Flags().StringVar(&serveLogLevel, "log-level", "progress", "Log level: debug, info, progress, minimal")
	rootCmd.AddCommand(serveCmd)
}

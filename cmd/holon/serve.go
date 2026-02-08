package main

import (
	"context"
	"encoding/json"
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
	serveRepo            string
	serveInput           string
	serveStateDir        string
	serveMaxEvents       int
	serveDryRun          bool
	serveLogLevel        string
	serveControllerSkill string
)

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Run proactive event-driven controller",
	Long: `Run an event-driven proactive controller loop.

The command reads event JSON (one object per line) from stdin by default,
normalizes events into an internal envelope, writes controller logs, and
forwards each event to a controller skill. Business decisions and follow-up
actions are handled by the agent inside that skill.`,
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

		handler, err := newCLIControllerHandler(serveRepo, absStateDir, serveControllerSkill, serveDryRun)
		if err != nil {
			return err
		}

		svc, err := serve.New(serve.Config{
			RepoHint: serveRepo,
			StateDir: absStateDir,
			Handler:  handler,
		})
		if err != nil {
			return err
		}
		defer svc.Close()

		holonlog.Info("serve started", "repo", serveRepo, "state_dir", absStateDir, "input", serveInput, "controller_skill", serveControllerSkill)
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

type cliControllerHandler struct {
	execPath        string
	repoHint        string
	stateDir        string
	controllerSkill string
	dryRun          bool
}

func newCLIControllerHandler(repoHint, stateDir, controllerSkill string, dryRun bool) (*cliControllerHandler, error) {
	execPath, err := os.Executable()
	if err != nil {
		return nil, fmt.Errorf("failed to resolve current executable: %w", err)
	}
	if controllerSkill == "" {
		controllerSkill = filepath.Join("skills", "github-controller")
	}
	return &cliControllerHandler{
		execPath:        execPath,
		repoHint:        repoHint,
		stateDir:        stateDir,
		controllerSkill: controllerSkill,
		dryRun:          dryRun,
	}, nil
}

func (h *cliControllerHandler) HandleEvent(ctx context.Context, env serve.EventEnvelope) error {
	ref, err := h.buildRef(env)
	if err != nil {
		return err
	}
	inputDir, cleanup, err := h.buildInputDir(env)
	if err != nil {
		return err
	}
	defer cleanup()
	outputDir, err := os.MkdirTemp("", "holon-serve-output-*")
	if err != nil {
		return fmt.Errorf("failed to create temp output dir: %w", err)
	}
	defer func() { _ = os.RemoveAll(outputDir) }()

	args := []string{
		"solve",
		ref,
		"--skill", h.controllerSkill,
		"--input", inputDir,
		"--state-dir", filepath.Join(h.stateDir, "controller-state"),
		"--output", outputDir,
		"--cleanup", "all",
	}
	if h.repoHint != "" {
		args = append(args, "--repo", h.repoHint)
	}

	if h.dryRun {
		holonlog.Info("serve dry-run forward", "event_id", env.ID, "type", env.Type, "args", args)
		return nil
	}

	cmd := exec.CommandContext(ctx, h.execPath, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Stdin = nil
	cmd.Env = append(os.Environ(), "CLAUDE_CONFIG_DIR=/holon/state/claude-config")
	if sessionID := h.readSessionID(); sessionID != "" {
		cmd.Env = append(cmd.Env, "HOLON_CONTROLLER_SESSION_ID="+sessionID)
	}
	if err := cmd.Run(); err != nil {
		return err
	}
	if err := h.captureSessionID(outputDir); err != nil {
		holonlog.Warn("failed to capture controller session id", "error", err)
	}
	return nil
}

func (h *cliControllerHandler) buildRef(env serve.EventEnvelope) (string, error) {
	repo := env.Scope.Repo
	if repo == "" {
		repo = h.repoHint
	}
	if repo == "" {
		return "", fmt.Errorf("missing repo for event %s", env.ID)
	}
	if env.Subject.ID == "" {
		return "", fmt.Errorf("missing subject id for event %s", env.ID)
	}
	if _, err := strconv.Atoi(env.Subject.ID); err != nil {
		return "", fmt.Errorf("invalid subject id for event %s: %w", env.ID, err)
	}
	return fmt.Sprintf("%s#%s", repo, env.Subject.ID), nil
}

func (h *cliControllerHandler) buildInputDir(env serve.EventEnvelope) (string, func(), error) {
	inputDir, err := os.MkdirTemp("", "holon-serve-input-*")
	if err != nil {
		return "", nil, fmt.Errorf("failed to create temp input dir: %w", err)
	}
	cleanup := func() { _ = os.RemoveAll(inputDir) }

	contextDir := filepath.Join(inputDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		cleanup()
		return "", nil, fmt.Errorf("failed to create context dir: %w", err)
	}

	eventBytes, err := json.MarshalIndent(env, "", "  ")
	if err != nil {
		cleanup()
		return "", nil, fmt.Errorf("failed to serialize event: %w", err)
	}

	if err := os.WriteFile(filepath.Join(contextDir, "event.json"), eventBytes, 0644); err != nil {
		cleanup()
		return "", nil, fmt.Errorf("failed to write event context: %w", err)
	}
	if err := h.copyControllerMemoryToInput(contextDir); err != nil {
		cleanup()
		return "", nil, err
	}

	workflow := map[string]any{
		"trigger": map[string]any{
			"goal_hint": fmt.Sprintf("Controller event: %s. Read /holon/input/context/event.json, decide next action autonomously, and execute via skills.", env.Type),
		},
	}
	workflowBytes, err := json.MarshalIndent(workflow, "", "  ")
	if err != nil {
		cleanup()
		return "", nil, fmt.Errorf("failed to serialize workflow metadata: %w", err)
	}
	if err := os.WriteFile(filepath.Join(inputDir, "workflow.json"), workflowBytes, 0644); err != nil {
		cleanup()
		return "", nil, fmt.Errorf("failed to write workflow metadata: %w", err)
	}

	return inputDir, cleanup, nil
}

func (h *cliControllerHandler) copyControllerMemoryToInput(contextDir string) error {
	src := filepath.Join(h.stateDir, "controller-state", "controller-memory.md")
	data, err := os.ReadFile(src)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("failed to read controller memory: %w", err)
	}
	dst := filepath.Join(contextDir, "controller-memory.md")
	if err := os.WriteFile(dst, data, 0644); err != nil {
		return fmt.Errorf("failed to write input controller memory: %w", err)
	}
	return nil
}

func (h *cliControllerHandler) readSessionID() string {
	type sessionState struct {
		SessionID string `json:"session_id"`
	}
	data, err := os.ReadFile(filepath.Join(h.stateDir, "controller-session.json"))
	if err != nil {
		return ""
	}
	var state sessionState
	if err := json.Unmarshal(data, &state); err != nil {
		return ""
	}
	return state.SessionID
}

func (h *cliControllerHandler) captureSessionID(outputDir string) error {
	type sessionRecord struct {
		SessionID string `json:"session_id"`
	}
	path := filepath.Join(outputDir, "evidence", "session.json")
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("failed to read session artifact: %w", err)
	}
	var record sessionRecord
	if err := json.Unmarshal(data, &record); err != nil {
		return fmt.Errorf("failed to parse session artifact: %w", err)
	}
	if record.SessionID == "" {
		return nil
	}
	stateBytes, err := json.MarshalIndent(record, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal session state: %w", err)
	}
	if err := os.WriteFile(filepath.Join(h.stateDir, "controller-session.json"), stateBytes, 0644); err != nil {
		return fmt.Errorf("failed to write session state: %w", err)
	}
	return nil
}

func init() {
	serveCmd.Flags().StringVar(&serveRepo, "repo", "", "Default repository in owner/repo format")
	serveCmd.Flags().StringVar(&serveInput, "input", "-", "Input source for events ('-' for stdin, or path to file)")
	serveCmd.Flags().StringVar(&serveStateDir, "state-dir", "", "State directory (default: .holon/serve-state)")
	serveCmd.Flags().IntVar(&serveMaxEvents, "max-events", 0, "Stop after processing N events (0 = unlimited)")
	serveCmd.Flags().BoolVar(&serveDryRun, "dry-run", false, "Log forwarded events without running controller skill")
	serveCmd.Flags().StringVar(&serveControllerSkill, "controller-skill", filepath.Join("skills", "github-controller"), "Controller skill path or reference")
	serveCmd.Flags().StringVar(&serveLogLevel, "log-level", "progress", "Log level: debug, info, progress, minimal")
	rootCmd.AddCommand(serveCmd)
}

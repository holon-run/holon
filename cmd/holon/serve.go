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
	"sync"
	"time"

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
		defer handler.Close()

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

	mu                 sync.Mutex
	controllerCmd      *exec.Cmd
	controllerCancel   context.CancelFunc
	controllerDone     <-chan error
	controllerChannel  string
	controllerInputDir string
	controllerOutput   string
	restartAttempts    int
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
	if h.dryRun {
		holonlog.Info("serve dry-run forward", "event_id", env.ID, "type", env.Type)
		return nil
	}

	ref, err := h.buildRef(env)
	if err != nil {
		return err
	}

	h.mu.Lock()
	defer h.mu.Unlock()

	if err := h.ensureControllerLocked(ctx, ref); err != nil {
		return err
	}
	if err := appendJSONLine(h.controllerChannel, env); err != nil {
		return fmt.Errorf("failed to write event to channel: %w", err)
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
		return "", serve.NewSkipEventError(fmt.Sprintf("missing subject id for event %s", env.ID))
	}
	if _, err := strconv.Atoi(env.Subject.ID); err != nil {
		return "", serve.NewSkipEventError(fmt.Sprintf("invalid subject id for event %s: %v", env.ID, err))
	}
	return fmt.Sprintf("%s#%s", repo, env.Subject.ID), nil
}

func (h *cliControllerHandler) buildInputDir() (string, error) {
	inputDir := filepath.Join(h.stateDir, "controller-runtime", "input")
	if err := os.RemoveAll(inputDir); err != nil {
		return "", fmt.Errorf("failed to reset controller input dir: %w", err)
	}
	if err := os.MkdirAll(inputDir, 0755); err != nil {
		return "", fmt.Errorf("failed to create controller input dir: %w", err)
	}

	contextDir := filepath.Join(inputDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		return "", fmt.Errorf("failed to create context dir: %w", err)
	}
	if err := h.copyControllerMemoryToInput(contextDir); err != nil {
		return "", err
	}

	workflow := map[string]any{
		"trigger": map[string]any{
			"goal_hint": "Persistent controller runtime. Read events from HOLON_CONTROLLER_EVENT_CHANNEL and decide actions autonomously using available skills.",
		},
	}
	workflowBytes, err := json.MarshalIndent(workflow, "", "  ")
	if err != nil {
		return "", fmt.Errorf("failed to serialize workflow metadata: %w", err)
	}
	if err := os.WriteFile(filepath.Join(inputDir, "workflow.json"), workflowBytes, 0644); err != nil {
		return "", fmt.Errorf("failed to write workflow metadata: %w", err)
	}

	return inputDir, nil
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

func (h *cliControllerHandler) ensureControllerLocked(ctx context.Context, ref string) error {
	if h.controllerCmd != nil {
		select {
		case err := <-h.controllerDone:
			holonlog.Warn("controller runtime exited", "error", err)
			h.controllerCmd = nil
			h.controllerDone = nil
			h.controllerCancel = nil
		default:
			return nil
		}
	}

	if err := os.MkdirAll(filepath.Join(h.stateDir, "controller-state"), 0755); err != nil {
		return fmt.Errorf("failed to create controller state dir: %w", err)
	}
	channelPath := filepath.Join(h.stateDir, "controller-state", "event-channel.ndjson")
	if err := touchFile(channelPath); err != nil {
		return fmt.Errorf("failed to initialize event channel: %w", err)
	}
	inputDir, err := h.buildInputDir()
	if err != nil {
		return err
	}
	outputDir := filepath.Join(h.stateDir, "controller-runtime", "output")
	if err := os.MkdirAll(outputDir, 0755); err != nil {
		return fmt.Errorf("failed to create controller output dir: %w", err)
	}

	controllerCtx, cancel := context.WithCancel(ctx)
	args := []string{
		"solve",
		ref,
		"--skill", h.controllerSkill,
		"--input", inputDir,
		"--state-dir", filepath.Join(h.stateDir, "controller-state"),
		"--output", outputDir,
		"--cleanup", "none",
		"--env", "HOLON_AGENT_SESSION_MODE=serve",
		"--env", "CLAUDE_CONFIG_DIR=/holon/state/claude-config",
		"--env", "HOLON_CONTROLLER_EVENT_CHANNEL=/holon/state/event-channel.ndjson",
		"--env", "HOLON_CONTROLLER_EVENT_CURSOR=/holon/state/event-channel.cursor",
		"--env", "HOLON_CONTROLLER_SESSION_STATE_PATH=/holon/state/controller-session.json",
	}
	if h.repoHint != "" {
		args = append(args, "--repo", h.repoHint)
	}
	if sessionID := h.readSessionID(); sessionID != "" {
		args = append(args, "--env", "HOLON_CONTROLLER_SESSION_ID="+sessionID)
	}

	cmd := exec.CommandContext(controllerCtx, h.execPath, args...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Stdin = nil
	if err := cmd.Start(); err != nil {
		cancel()
		return fmt.Errorf("failed to start controller runtime: %w", err)
	}

	done := make(chan error, 1)
	go func() {
		done <- cmd.Wait()
	}()

	h.controllerCmd = cmd
	h.controllerCancel = cancel
	h.controllerDone = done
	h.controllerChannel = channelPath
	h.controllerInputDir = inputDir
	h.controllerOutput = outputDir
	h.restartAttempts++

	holonlog.Info(
		"controller runtime connected",
		"pid", cmd.Process.Pid,
		"channel", channelPath,
		"restart_attempt", h.restartAttempts,
	)
	return nil
}

func (h *cliControllerHandler) Close() error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.controllerCancel == nil {
		return nil
	}
	holonlog.Info("stopping controller runtime")
	h.controllerCancel()
	if h.controllerDone != nil {
		select {
		case <-h.controllerDone:
		case <-time.After(5 * time.Second):
			if h.controllerCmd != nil && h.controllerCmd.Process != nil {
				_ = h.controllerCmd.Process.Kill()
			}
		}
	}
	h.controllerCmd = nil
	h.controllerCancel = nil
	h.controllerDone = nil
	return nil
}

func touchFile(path string) error {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return err
	}
	return f.Close()
}

func appendJSONLine(path string, value any) error {
	data, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("failed to marshal line: %w", err)
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return err
	}
	defer f.Close()
	if _, err := f.Write(append(data, '\n')); err != nil {
		return err
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

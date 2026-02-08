package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/runtime/docker"
	"github.com/holon-run/holon/pkg/serve"
	"github.com/spf13/cobra"
)

var (
	serveRepo                string
	serveInput               string
	serveStateDir            string
	serveMaxEvents           int
	serveDryRun              bool
	serveLogLevel            string
	serveControllerSkill     string
	serveControllerWorkspace string
)

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Run proactive event-driven controller",
	Long: `Run an event-driven proactive controller loop.

The command reads event JSON (one object per line) from stdin by default,
normalizes events into an internal envelope, writes controller logs, and
forwards each event to a persistent controller skill session.`,
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

		controllerWorkspace := serveControllerWorkspace
		if controllerWorkspace == "" {
			controllerWorkspace, err = defaultControllerWorkspace()
			if err != nil {
				return err
			}
		}
		controllerWorkspace, err = filepath.Abs(controllerWorkspace)
		if err != nil {
			return fmt.Errorf("failed to resolve controller workspace: %w", err)
		}
		if err := os.MkdirAll(controllerWorkspace, 0755); err != nil {
			return fmt.Errorf("failed to create controller workspace: %w", err)
		}

		reader, closer, err := openServeInput(serveInput)
		if err != nil {
			return err
		}
		if closer != nil {
			defer closer.Close()
		}

		handler, err := newCLIControllerHandler(serveRepo, absStateDir, controllerWorkspace, serveControllerSkill, serveLogLevel, serveDryRun, nil)
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

		holonlog.Info(
			"serve started",
			"repo", serveRepo,
			"state_dir", absStateDir,
			"workspace", controllerWorkspace,
			"input", serveInput,
			"controller_skill", serveControllerSkill,
		)
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
	repoHint            string
	stateDir            string
	controllerWorkspace string
	controllerSkill     string
	logLevel            string
	dryRun              bool
	sessionRunner       SessionRunner
	controllerSession   *docker.SessionHandle
	controllerDone      <-chan error
	controllerChannel   string
	controllerInputDir  string
	controllerOutput    string
	restartAttempts     int
	mu                  sync.Mutex
}

var (
	maxEventChannelSizeBytes int64 = 8 * 1024 * 1024
)

func newCLIControllerHandler(
	repoHint,
	stateDir,
	controllerWorkspace,
	controllerSkill,
	logLevel string,
	dryRun bool,
	sessionRunner SessionRunner,
) (*cliControllerHandler, error) {
	if controllerSkill == "" {
		controllerSkill = filepath.Join("skills", "github-controller")
	}

	if sessionRunner == nil && !dryRun {
		rt, err := docker.NewRuntime()
		if err != nil {
			return nil, fmt.Errorf("failed to initialize runtime: %w", err)
		}
		sessionRunner = newDockerSessionRunner(rt)
	}

	return &cliControllerHandler{
		repoHint:            repoHint,
		stateDir:            stateDir,
		controllerWorkspace: controllerWorkspace,
		controllerSkill:     controllerSkill,
		logLevel:            logLevel,
		dryRun:              dryRun,
		sessionRunner:       sessionRunner,
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
	h.compactChannelBestEffortLocked()
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

func (h *cliControllerHandler) buildInputDir(ref string) (string, error) {
	inputDir := filepath.Join(h.stateDir, "controller-runtime", "input")
	stageDir := inputDir + ".tmp"

	if err := os.RemoveAll(stageDir); err != nil {
		return "", fmt.Errorf("failed to reset controller input staging dir: %w", err)
	}
	if err := os.MkdirAll(stageDir, 0755); err != nil {
		return "", fmt.Errorf("failed to create controller input staging dir: %w", err)
	}
	cleanupStage := true
	defer func() {
		if cleanupStage {
			_ = os.RemoveAll(stageDir)
		}
	}()

	contextDir := filepath.Join(stageDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		return "", fmt.Errorf("failed to create context dir: %w", err)
	}
	if err := h.copyControllerMemoryToInput(contextDir); err != nil {
		return "", err
	}

	workflow := map[string]any{
		"trigger": map[string]any{
			"goal_hint": "Persistent controller runtime. Read events from HOLON_CONTROLLER_EVENT_CHANNEL and decide actions autonomously using available skills.",
			"ref":       ref,
		},
	}
	workflowBytes, err := json.MarshalIndent(workflow, "", "  ")
	if err != nil {
		return "", fmt.Errorf("failed to serialize workflow metadata: %w", err)
	}
	if err := os.WriteFile(filepath.Join(stageDir, "workflow.json"), workflowBytes, 0644); err != nil {
		return "", fmt.Errorf("failed to write workflow metadata: %w", err)
	}

	if err := h.writeControllerSpecAndPrompts(stageDir); err != nil {
		return "", err
	}

	if err := os.RemoveAll(inputDir); err != nil {
		return "", fmt.Errorf("failed to reset controller input dir: %w", err)
	}
	if err := os.Rename(stageDir, inputDir); err != nil {
		return "", fmt.Errorf("failed to activate controller input dir: %w", err)
	}
	cleanupStage = false

	return inputDir, nil
}

func (h *cliControllerHandler) writeControllerSpecAndPrompts(inputDir string) error {
	specContent := fmt.Sprintf(`version: "v1"
kind: Holon
metadata:
  name: "github-controller-session"
  skills:
    - %q
goal:
  description: "Run as a persistent GitHub controller. Read events from HOLON_CONTROLLER_EVENT_CHANNEL and decide actions autonomously using available skills."
output:
  artifacts:
    - path: "manifest.json"
      required: true
`, h.controllerSkill)

	if err := os.WriteFile(filepath.Join(inputDir, "spec.yaml"), []byte(specContent), 0644); err != nil {
		return fmt.Errorf("failed to write controller spec: %w", err)
	}

	promptsDir := filepath.Join(inputDir, "prompts")
	if err := os.MkdirAll(promptsDir, 0755); err != nil {
		return fmt.Errorf("failed to create prompts dir: %w", err)
	}

	systemPrompt := strings.TrimSpace(`
You are Holon's persistent GitHub controller agent.

Operate continuously in session mode. For each incoming event, decide the best next action and execute it with available skills/tools.
Prioritize keeping delivery flow moving: create/advance PRs, request fixes, review updates, and report clear outcomes.
`)

	userPrompt := strings.TrimSpace(`
Controller runtime contract:
1. The event stream is available at HOLON_CONTROLLER_EVENT_CHANNEL.
2. Cursor state is persisted at HOLON_CONTROLLER_EVENT_CURSOR.
3. Session identity is persisted at HOLON_CONTROLLER_SESSION_STATE_PATH.
4. For each event, decide and execute actions autonomously. Keep responses concise and action-oriented.
`)

	if err := os.WriteFile(filepath.Join(promptsDir, "system.md"), []byte(systemPrompt+"\n"), 0644); err != nil {
		return fmt.Errorf("failed to write system prompt: %w", err)
	}
	if err := os.WriteFile(filepath.Join(promptsDir, "user.md"), []byte(userPrompt+"\n"), 0644); err != nil {
		return fmt.Errorf("failed to write user prompt: %w", err)
	}
	return nil
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
	data, err := os.ReadFile(h.sessionStatePath())
	if err != nil {
		return ""
	}
	var state sessionState
	if err := json.Unmarshal(data, &state); err != nil {
		return ""
	}
	return state.SessionID
}

func (h *cliControllerHandler) sessionStatePath() string {
	return filepath.Join(h.stateDir, "controller-state", "controller-session.json")
}

func (h *cliControllerHandler) ensureControllerLocked(ctx context.Context, ref string) error {
	if h.sessionRunner == nil {
		return fmt.Errorf("session runner is not initialized")
	}

	if h.controllerSession != nil {
		select {
		case err := <-h.controllerDone:
			holonlog.Warn("controller runtime exited", "error", err)
			h.controllerSession = nil
			h.controllerDone = nil
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
	inputDir, err := h.buildInputDir(ref)
	if err != nil {
		return err
	}
	outputDir := filepath.Join(h.stateDir, "controller-runtime", "output")
	if err := os.MkdirAll(outputDir, 0755); err != nil {
		return fmt.Errorf("failed to create controller output dir: %w", err)
	}

	env := map[string]string{
		"HOLON_AGENT_SESSION_MODE":            "serve",
		"CLAUDE_CONFIG_DIR":                   "/holon/state/claude-config",
		"HOLON_CONTROLLER_EVENT_CHANNEL":      "/holon/state/event-channel.ndjson",
		"HOLON_CONTROLLER_EVENT_CURSOR":       "/holon/state/event-channel.cursor",
		"HOLON_CONTROLLER_SESSION_STATE_PATH": "/holon/state/controller-session.json",
	}
	for k, v := range resolveServeLLMEnv() {
		env[k] = v
	}
	if sessionID := h.readSessionID(); sessionID != "" {
		env["HOLON_CONTROLLER_SESSION_ID"] = sessionID
	}

	session, err := h.sessionRunner.Start(ctx, ControllerSessionConfig{
		Workspace:       h.controllerWorkspace,
		InputPath:       inputDir,
		OutputPath:      outputDir,
		StateDir:        filepath.Join(h.stateDir, "controller-state"),
		ControllerSkill: h.controllerSkill,
		LogLevel:        h.logLevel,
		Env:             env,
	})
	if err != nil {
		return fmt.Errorf("failed to start controller runtime: %w", err)
	}

	done := make(chan error, 1)
	go func() {
		done <- h.sessionRunner.Wait(context.Background(), session)
	}()

	h.controllerSession = session
	h.controllerDone = done
	h.controllerChannel = channelPath
	h.controllerInputDir = inputDir
	h.controllerOutput = outputDir
	h.restartAttempts++

	holonlog.Info(
		"controller runtime connected",
		"container_id", session.ContainerID,
		"channel", channelPath,
		"restart_attempt", h.restartAttempts,
	)
	return nil
}

func resolveServeLLMEnv() map[string]string {
	result := map[string]string{}

	// Priority: current process env first, then ~/.claude/settings.json fallback.
	if v := strings.TrimSpace(os.Getenv("ANTHROPIC_AUTH_TOKEN")); v != "" {
		result["ANTHROPIC_AUTH_TOKEN"] = v
	}
	if v := strings.TrimSpace(os.Getenv("ANTHROPIC_BASE_URL")); v != "" {
		result["ANTHROPIC_BASE_URL"] = v
	}
	if v := strings.TrimSpace(os.Getenv("ANTHROPIC_API_KEY")); v != "" {
		result["ANTHROPIC_API_KEY"] = v
	}
	if v := strings.TrimSpace(os.Getenv("ANTHROPIC_API_URL")); v != "" {
		result["ANTHROPIC_API_URL"] = v
	}

	if len(result) > 0 {
		return result
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return result
	}
	settingsPath := filepath.Join(home, ".claude", "settings.json")
	fallback, err := readAnthropicEnvFromClaudeSettings(settingsPath)
	if err != nil {
		holonlog.Debug("failed to read Anthropic fallback from Claude settings", "path", settingsPath, "error", err)
		return result
	}
	for k, v := range fallback {
		result[k] = v
	}
	return result
}

func readAnthropicEnvFromClaudeSettings(path string) (map[string]string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var payload struct {
		Env map[string]string `json:"env"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return nil, fmt.Errorf("failed to parse settings.json: %w", err)
	}

	result := map[string]string{}
	for _, key := range []string{
		"ANTHROPIC_AUTH_TOKEN",
		"ANTHROPIC_BASE_URL",
		"ANTHROPIC_API_KEY",
		"ANTHROPIC_API_URL",
	} {
		if v := strings.TrimSpace(payload.Env[key]); v != "" {
			result[key] = v
		}
	}
	return result, nil
}

func (h *cliControllerHandler) compactChannelBestEffortLocked() {
	info, err := os.Stat(h.controllerChannel)
	if err != nil || info.Size() <= maxEventChannelSizeBytes {
		return
	}
	cursorPath := filepath.Join(h.stateDir, "controller-state", "event-channel.cursor")
	rawCursor, err := os.ReadFile(cursorPath)
	if err != nil {
		return
	}
	cursor, err := strconv.ParseInt(strings.TrimSpace(string(rawCursor)), 10, 64)
	if err != nil || cursor <= 0 || cursor >= info.Size() {
		return
	}
	data, err := os.ReadFile(h.controllerChannel)
	if err != nil {
		return
	}
	if cursor >= int64(len(data)) {
		return
	}
	remaining := data[cursor:]
	if err := os.WriteFile(h.controllerChannel, remaining, 0644); err != nil {
		holonlog.Warn("failed to compact event channel", "error", err)
		return
	}
	if err := os.WriteFile(cursorPath, []byte("0"), 0644); err != nil {
		holonlog.Warn("failed to reset event channel cursor", "error", err)
		return
	}
	holonlog.Info("compacted event channel", "old_bytes", info.Size(), "new_bytes", len(remaining))
}

func (h *cliControllerHandler) Close() error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.controllerSession == nil {
		return nil
	}
	if h.sessionRunner == nil {
		h.controllerSession = nil
		h.controllerDone = nil
		return nil
	}

	holonlog.Info("stopping controller runtime")
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := h.sessionRunner.Stop(ctx, h.controllerSession); err != nil {
		return err
	}
	h.controllerSession = nil
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
	buf := append(data, '\n')
	for len(buf) > 0 {
		n, writeErr := f.Write(buf)
		if writeErr != nil {
			if closeErr := f.Close(); closeErr != nil {
				return fmt.Errorf("write error: %w; close error: %v", writeErr, closeErr)
			}
			return writeErr
		}
		if n == 0 {
			_ = f.Close()
			return io.ErrShortWrite
		}
		buf = buf[n:]
	}
	if err := f.Close(); err != nil {
		return err
	}
	return nil
}

func init() {
	serveCmd.Flags().StringVar(&serveRepo, "repo", "", "Default repository in owner/repo format")
	serveCmd.Flags().StringVar(&serveInput, "input", "-", "Input source for events ('-' for stdin, or path to file)")
	serveCmd.Flags().StringVar(&serveStateDir, "state-dir", "", "State directory (default: .holon/serve-state)")
	serveCmd.Flags().StringVar(&serveControllerWorkspace, "controller-workspace", "", "Controller workspace path (default: $HOME/.holon/workspace)")
	serveCmd.Flags().IntVar(&serveMaxEvents, "max-events", 0, "Stop after processing N events (0 = unlimited)")
	serveCmd.Flags().BoolVar(&serveDryRun, "dry-run", false, "Log forwarded events without running controller skill")
	serveCmd.Flags().StringVar(&serveControllerSkill, "controller-skill", filepath.Join("skills", "github-controller"), "Controller skill path or reference")
	serveCmd.Flags().StringVar(&serveLogLevel, "log-level", "progress", "Log level: debug, info, progress, minimal")
	rootCmd.AddCommand(serveCmd)
}

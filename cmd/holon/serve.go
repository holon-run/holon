package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/runtime/docker"
	"github.com/holon-run/holon/pkg/serve"
	"github.com/spf13/cobra"
)

var (
	serveRepo                  string
	serveInput                 string
	serveAgentID               string
	serveAgentHome             string
	serveMaxEvents             int
	serveDryRun                bool
	serveLogLevel              string
	serveWebhookPort           int
	serveWebhookMode           bool
	serveTickInterval          time.Duration
	serveNoSubscriptions       bool
	serveRuntimeMode           string
	serveRuntimeDevAgentSource string
)

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Run proactive event-driven controller",
	Long: `Run an event-driven proactive controller loop.

The command reads event JSON (one object per line) from stdin by default,
normalizes events into an internal envelope, writes controller logs, and
forwards each event to a persistent controller skill session.

Webhook Mode:
When --webhook-port is specified, serve runs an HTTP server on that port
to receive GitHub webhook events directly. This integrates with 'gh webhook forward'
for local development and testing.`,
	RunE: func(cmd *cobra.Command, _ []string) error {
		logCfg := holonlog.Config{
			Level:  holonlog.LogLevel(serveLogLevel),
			Format: "console",
		}
		if err := holonlog.Init(logCfg); err != nil {
			return fmt.Errorf("failed to initialize logger: %w", err)
		}
		defer holonlog.Sync()

		serveWebhookMode = serveWebhookPort > 0
		parentCtx := cmd.Context()
		if parentCtx == nil {
			parentCtx = context.Background()
		}

		agentResolution, err := resolveAgentHome("serve", serveAgentID, serveAgentHome, false)
		if err != nil {
			return err
		}
		releaseLock, err := acquireServeAgentLock(agentResolution.AgentHome)
		if err != nil {
			return err
		}
		defer releaseLock()
		absStateDir := stateDirForAgentHome(agentResolution.AgentHome)
		if err := os.MkdirAll(absStateDir, 0755); err != nil {
			return fmt.Errorf("failed to create state dir: %w", err)
		}
		roleLabel, err := loadControllerRole(agentResolution.AgentHome)
		if err != nil {
			return err
		}
		controllerWorkspace, err := resolveControllerWorkspace(agentResolution.AgentHome)
		if err != nil {
			return err
		}
		resolvedRuntimeMode, err := resolveRuntimeMode(serveRuntimeMode)
		if err != nil {
			return err
		}
		resolvedRuntimeDevAgentSource, err := resolveRuntimeDevAgentSource(resolvedRuntimeMode, serveRuntimeDevAgentSource)
		if err != nil {
			return err
		}

		handler, err := newCLIControllerHandler(
			serveRepo,
			absStateDir,
			agentResolution.AgentHome,
			controllerWorkspace,
			roleLabel,
			serveLogLevel,
			resolvedRuntimeMode,
			resolvedRuntimeDevAgentSource,
			serveDryRun,
			nil,
		)
		if err != nil {
			return err
		}
		defer handler.Close()
		turnDispatcher := func(ctx context.Context, req serve.TurnStartRequest, turnID string) error {
			return handler.HandleTurnStart(ctx, req, turnID)
		}

		// Use subscription manager if subscriptions are enabled.
		// A non-zero --webhook-port acts as an override for subscription ingress port.
		if !serveNoSubscriptions && (serveInput == "" || serveInput == "-") {
			subMgr, err := serve.NewSubscriptionManager(serve.ManagerConfig{
				AgentHome:      agentResolution.AgentHome,
				StateDir:       absStateDir,
				Handler:        handler,
				WebhookPort:    serveWebhookPort,
				TurnDispatcher: turnDispatcher,
			})
			if err != nil {
				return fmt.Errorf("failed to create subscription manager: %w", err)
			}
			defer subMgr.Stop()

			tickCtx, tickCancel := context.WithCancel(parentCtx)
			defer tickCancel()

			// Determine repos for tick interval from CLI override or subscription config.
			repos := subMgr.SubscribedRepos()
			if serveRepo != "" {
				repos = []string{serveRepo}
			}
			if serveTickInterval > 0 && len(repos) == 0 {
				return fmt.Errorf("at least one repo is required when --tick-interval is enabled (set subscriptions.github.repos in agent.yaml or pass --repo)")
			}

			if serveTickInterval > 0 && len(repos) > 0 {
				for _, repo := range repos {
					startServeTickEmitter(tickCtx, serveTickInterval, repo, func(ctx context.Context, env serve.EventEnvelope) error {
						return subMgr.InjectEvent(ctx, env)
					})
				}
			}

			// Start subscription manager
			if err := subMgr.Start(tickCtx); err != nil {
				return fmt.Errorf("failed to start subscription manager: %w", err)
			}

			holonlog.Info(
				"serve started (subscription mode)",
				"agent_id", agentResolution.AgentID,
				"agent_home", agentResolution.AgentHome,
				"state_dir", absStateDir,
				"workspace", controllerWorkspace,
				"controller_role", roleLabel,
				"role_source", filepath.Join(agentResolution.AgentHome, "ROLE.md"),
				"tick_interval", serveTickInterval,
				"webhook_port", subMgr.GetWebhookPort(),
			)

			// Wait for context cancellation
			<-tickCtx.Done()
			return nil
		}

		// Webhook mode (legacy, for backward compatibility)
		if serveWebhookMode {
			if serveTickInterval > 0 && strings.TrimSpace(serveRepo) == "" {
				return fmt.Errorf("--repo is required when --tick-interval is set in webhook mode")
			}
			webhookSrv, err := serve.NewWebhookServer(serve.WebhookConfig{
				Port:           serveWebhookPort,
				RepoHint:       serveRepo,
				StateDir:       absStateDir,
				Handler:        handler,
				TurnDispatcher: turnDispatcher,
			})
			if err != nil {
				return fmt.Errorf("failed to create webhook server: %w", err)
			}
			defer webhookSrv.Close()

			tickCtx, tickCancel := context.WithCancel(parentCtx)
			defer tickCancel()
			if serveTickInterval > 0 {
				startServeTickEmitter(tickCtx, serveTickInterval, serveRepo, func(ctx context.Context, env serve.EventEnvelope) error {
					return webhookSrv.InjectEvent(ctx, env)
				})
			}

			holonlog.Info(
				"serve started (webhook mode)",
				"repo", serveRepo,
				"state_dir", absStateDir,
				"agent_id", agentResolution.AgentID,
				"agent_home", agentResolution.AgentHome,
				"workspace", controllerWorkspace,
				"port", serveWebhookPort,
				"controller_role", roleLabel,
				"role_source", filepath.Join(agentResolution.AgentHome, "ROLE.md"),
				"tick_interval", serveTickInterval,
			)
			return webhookSrv.Start(tickCtx)
		}

		// Stdin/File mode
		reader, closer, err := openServeInput(serveInput)
		if err != nil {
			return err
		}
		if closer != nil {
			defer closer.Close()
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

		tickCtx, tickCancel := context.WithCancel(parentCtx)
		defer tickCancel()
		if serveTickInterval > 0 {
			if serveRepo == "" {
				return fmt.Errorf("--repo is required when --tick-interval is enabled")
			}
			startServeTickEmitter(tickCtx, serveTickInterval, serveRepo, func(ctx context.Context, env serve.EventEnvelope) error {
				return svc.InjectEvent(ctx, env)
			})
		}

		holonlog.Info(
			"serve started",
			"repo", serveRepo,
			"state_dir", absStateDir,
			"agent_id", agentResolution.AgentID,
			"agent_home", agentResolution.AgentHome,
			"workspace", controllerWorkspace,
			"input", serveInput,
			"controller_role", roleLabel,
			"role_source", filepath.Join(agentResolution.AgentHome, "ROLE.md"),
			"tick_interval", serveTickInterval,
		)
		return svc.Run(tickCtx, reader, serveMaxEvents)
	},
}

func loadControllerRole(agentHome string) (string, error) {
	rolePath := filepath.Join(agentHome, "ROLE.md")
	info, err := os.Stat(rolePath)
	if err != nil {
		return "", fmt.Errorf("failed to stat %s: %w", rolePath, err)
	}
	if !info.Mode().IsRegular() {
		return "", fmt.Errorf("role prompt path is not a regular file: %s", rolePath)
	}
	data, err := os.ReadFile(rolePath)
	if err != nil {
		return "", fmt.Errorf("failed to read role prompt %s: %w", rolePath, err)
	}
	content := strings.TrimSpace(string(data))
	if content == "" {
		return "", fmt.Errorf("role prompt file is empty: %s (please add a role definition, e.g., '# ROLE: PM')", rolePath)
	}
	return inferControllerRole(content), nil
}

func inferControllerRole(content string) string {
	lower := strings.ToLower(content)
	if role := inferRoleFromFrontMatter(lower); role != "" {
		return role
	}
	switch {
	case strings.Contains(lower, "role: dev"),
		strings.Contains(lower, "role dev"),
		strings.Contains(lower, "developer"),
		strings.Contains(lower, "software engineer"):
		return "dev"
	case strings.Contains(lower, "role: pm"),
		strings.Contains(lower, "role pm"),
		strings.Contains(lower, "product manager"):
		return "pm"
	default:
		return "pm"
	}
}

func inferRoleFromFrontMatter(lower string) string {
	trimmed := strings.TrimSpace(lower)
	if !strings.HasPrefix(trimmed, "---\n") {
		return ""
	}
	lines := strings.Split(trimmed, "\n")
	for i := 1; i < len(lines); i++ {
		line := strings.TrimSpace(lines[i])
		if line == "---" {
			return ""
		}
		if !strings.HasPrefix(line, "role:") {
			continue
		}
		role := strings.TrimSpace(strings.TrimPrefix(line, "role:"))
		switch role {
		case "pm", "product-manager", "product_manager":
			return "pm"
		case "dev", "developer", "engineer":
			return "dev"
		default:
			return ""
		}
	}
	return ""
}

func startServeTickEmitter(ctx context.Context, interval time.Duration, repo string, sink func(context.Context, serve.EventEnvelope) error) {
	if interval <= 0 {
		return
	}
	go func() {
		ticker := time.NewTicker(interval)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case now := <-ticker.C:
				env := buildTickEvent(repo, now, interval)
				if err := sink(ctx, env); err != nil {
					holonlog.Warn("failed to inject timer tick", "error", err, "event_id", env.ID)
					return
				}
				if ctx.Err() != nil {
					return
				}
			}
		}
	}()
}

func buildTickEvent(repo string, at time.Time, interval time.Duration) serve.EventEnvelope {
	if interval <= 0 {
		interval = time.Minute
	}
	bucket := at.UTC().Truncate(interval)
	bucketID := strconv.FormatInt(bucket.Unix(), 10)
	return serve.EventEnvelope{
		ID:     fmt.Sprintf("tick_%d", at.UTC().UnixNano()),
		Source: "timer",
		Type:   "timer.tick",
		At:     at.UTC(),
		Scope: serve.EventScope{
			Repo: repo,
		},
		Subject: serve.EventSubject{
			Kind: "timer",
			ID:   bucketID,
		},
		DedupeKey: fmt.Sprintf("timer:%s:%s", repo, bucketID),
	}
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

func acquireServeAgentLock(agentHome string) (func(), error) {
	lockPath := filepath.Join(agentHome, "agent.lock")
	content := []byte(fmt.Sprintf("%d\n", os.Getpid()))

	for attempt := 0; attempt < 2; attempt++ {
		f, err := os.OpenFile(lockPath, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0644)
		if err != nil {
			if !os.IsExist(err) {
				return nil, fmt.Errorf("failed to create agent lock file: %w", err)
			}
			stale, staleErr := removeStaleServeAgentLock(lockPath)
			if staleErr != nil {
				return nil, staleErr
			}
			if stale {
				continue
			}
			return nil, fmt.Errorf("agent home is already locked by another serve process: %s", lockPath)
		}
		if _, err := f.Write(content); err != nil {
			_ = f.Close()
			_ = os.Remove(lockPath)
			return nil, fmt.Errorf("failed to write agent lock file: %w", err)
		}
		if err := f.Close(); err != nil {
			_ = os.Remove(lockPath)
			return nil, fmt.Errorf("failed to close agent lock file: %w", err)
		}
		return func() {
			_ = os.Remove(lockPath)
		}, nil
	}

	return nil, fmt.Errorf("agent home is already locked by another serve process: %s", lockPath)
}

func removeStaleServeAgentLock(lockPath string) (bool, error) {
	data, err := os.ReadFile(lockPath)
	if err != nil {
		if os.IsNotExist(err) {
			return false, nil
		}
		return false, fmt.Errorf("failed to read agent lock file: %w", err)
	}

	pidText := strings.TrimSpace(string(data))
	pid, err := strconv.Atoi(pidText)
	if err != nil || pid <= 0 {
		return false, fmt.Errorf("agent lock file is invalid: %s", lockPath)
	}

	running, runErr := isProcessRunning(pid)
	if runErr != nil {
		return false, fmt.Errorf("failed to inspect lock holder process %d: %w", pid, runErr)
	}
	if running {
		return false, nil
	}

	if err := os.Remove(lockPath); err != nil && !os.IsNotExist(err) {
		return false, fmt.Errorf("failed to remove stale agent lock: %w", err)
	}
	return true, nil
}

func isProcessRunning(pid int) (bool, error) {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false, err
	}
	if runtime.GOOS == "windows" {
		return true, nil
	}
	err = proc.Signal(syscall.Signal(0))
	if err == nil {
		return true, nil
	}
	if errors.Is(err, os.ErrProcessDone) {
		return false, nil
	}
	if errors.Is(err, syscall.ESRCH) {
		return false, nil
	}
	if errors.Is(err, syscall.EPERM) {
		return true, nil
	}
	return false, nil
}

type cliControllerHandler struct {
	repoHint              string
	stateDir              string
	agentHome             string
	controllerWorkspace   string
	controllerRoleLabel   string
	logLevel              string
	runtimeMode           string
	runtimeDevAgentSource string
	dryRun                bool
	sessionRunner         SessionRunner
	controllerSession     *docker.SessionHandle
	controllerDone        <-chan error
	controllerChannel     string
	controllerInputDir    string
	controllerOutput      string
	restartAttempts       int
	mu                    sync.Mutex
}

var (
	maxEventChannelSizeBytes int64 = 8 * 1024 * 1024
)

func newCLIControllerHandler(
	repoHint,
	stateDir,
	agentHome,
	controllerWorkspace,
	controllerRoleLabel,
	logLevel string,
	runtimeMode string,
	runtimeDevAgentSource string,
	dryRun bool,
	sessionRunner SessionRunner,
) (*cliControllerHandler, error) {
	if sessionRunner == nil && !dryRun {
		rt, err := docker.NewRuntime()
		if err != nil {
			return nil, fmt.Errorf("failed to initialize runtime: %w", err)
		}
		sessionRunner = newDockerSessionRunner(rt)
	}

	return &cliControllerHandler{
		repoHint:              repoHint,
		stateDir:              stateDir,
		agentHome:             agentHome,
		controllerWorkspace:   controllerWorkspace,
		controllerRoleLabel:   controllerRoleLabel,
		logLevel:              logLevel,
		runtimeMode:           runtimeMode,
		runtimeDevAgentSource: runtimeDevAgentSource,
		dryRun:                dryRun,
		sessionRunner:         sessionRunner,
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

func (h *cliControllerHandler) HandleTurnStart(ctx context.Context, req serve.TurnStartRequest, turnID string) error {
	payload := map[string]any{
		"turn_id":          turnID,
		"thread_id":        req.ThreadID,
		"input":            req.Input,
		"extended_context": req.ExtendedContext,
	}
	payloadRaw, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("failed to marshal rpc turn payload: %w", err)
	}
	repo := strings.TrimSpace(h.repoHint)
	env := serve.EventEnvelope{
		ID:     fmt.Sprintf("rpc_turn_%d", time.Now().UTC().UnixNano()),
		Source: "rpc",
		Type:   "rpc.turn.input",
		At:     time.Now().UTC(),
		Scope: serve.EventScope{
			Repo: repo,
		},
		Subject: serve.EventSubject{
			Kind: "thread",
			ID:   strings.TrimSpace(req.ThreadID),
		},
		DedupeKey: fmt.Sprintf("rpc:turn:%s:%s", strings.TrimSpace(req.ThreadID), turnID),
		Payload:   payloadRaw,
	}
	return h.HandleEvent(ctx, env)
}

func (h *cliControllerHandler) buildRef(env serve.EventEnvelope) (string, error) {
	repo := env.Scope.Repo
	if repo == "" {
		repo = h.repoHint
	}
	if env.Type == "rpc.turn.input" {
		if repo == "" {
			repo = "local/rpc"
		}
		return fmt.Sprintf("%s#0", repo), nil
	}
	if repo == "" {
		return "", fmt.Errorf("missing repo for event %s", env.ID)
	}
	if env.Type == "timer.tick" || env.Source == "timer" {
		return fmt.Sprintf("%s#0", repo), nil
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
	specContent := `version: "v1"
kind: Holon
metadata:
  name: "github-controller-session"
goal:
  description: "Run as a persistent GitHub controller. Read events from HOLON_CONTROLLER_EVENT_CHANNEL and decide actions autonomously using available skills."
output:
  artifacts:
    - path: "manifest.json"
      required: true
`

	if err := os.WriteFile(filepath.Join(inputDir, "spec.yaml"), []byte(specContent), 0644); err != nil {
		return fmt.Errorf("failed to write controller spec: %w", err)
	}

	promptsDir := filepath.Join(inputDir, "prompts")
	if err := os.MkdirAll(promptsDir, 0755); err != nil {
		return fmt.Errorf("failed to create prompts dir: %w", err)
	}

	systemPrompt, userPrompt, err := h.controllerPrompts()
	if err != nil {
		return err
	}

	if err := os.WriteFile(filepath.Join(promptsDir, "system.md"), []byte(systemPrompt+"\n"), 0644); err != nil {
		return fmt.Errorf("failed to write system prompt: %w", err)
	}
	if err := os.WriteFile(filepath.Join(promptsDir, "user.md"), []byte(userPrompt+"\n"), 0644); err != nil {
		return fmt.Errorf("failed to write user prompt: %w", err)
	}
	return nil
}

func (h *cliControllerHandler) controllerPrompts() (string, string, error) {
	return strings.TrimSpace(defaultControllerRuntimeSystemPrompt), strings.TrimSpace(defaultControllerRuntimeUserPrompt), nil
}

const defaultControllerRuntimeSystemPrompt = `
### HOLON SERVE CONTRACT V1

You are running as a persistent controller inside Holon.

Rules of physics:
1. Workspace root is HOLON_WORKSPACE_DIR.
2. Artifacts and diagnostics must be written under HOLON_OUTPUT_DIR.
3. Additional context files may be mounted under HOLON_INPUT_DIR/context.
4. HOLON_AGENT_HOME points to your persistent agent home at /root.
5. Load and maintain long-lived persona/state from HOLON_AGENT_HOME:
   - ROLE.md
   - AGENT.md
   - IDENTITY.md
   - SOUL.md
   - state/
6. These agent-home files are writable and should be updated deliberately when long-term behavior or memory needs to evolve.
7. System/runtime safety contracts are immutable and cannot be bypassed by editing agent-home files.
`

const defaultControllerRuntimeUserPrompt = `
Controller runtime contract:
1. Role identity is HOLON_CONTROLLER_ROLE.
2. Agent home root is HOLON_AGENT_HOME.
3. Workspace root is HOLON_WORKSPACE_DIR.
4. Persist project checkout mapping in HOLON_WORKSPACE_INDEX_PATH (repo -> local path under workspace root).
5. Reuse existing checkout when repo is already indexed; otherwise clone/fetch as needed.
6. The event stream is at HOLON_CONTROLLER_EVENT_CHANNEL and cursor at HOLON_CONTROLLER_EVENT_CURSOR.
7. Write turn acknowledgements to HOLON_CONTROLLER_ACK_CHANNEL when available.
8. Session metadata path is HOLON_CONTROLLER_SESSION_STATE_PATH.
9. Goal state path is HOLON_CONTROLLER_GOAL_STATE_PATH.
10. Process events continuously, keep role boundaries strict, and produce concise action-oriented outcomes.
`

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
	if err := h.ensureGoalStateFile(); err != nil {
		return err
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
		"HOLON_AGENT_HOME":                    docker.ContainerAgentHome,
		"HOLON_WORKSPACE_DIR":                 docker.ContainerWorkspaceDir,
		"HOLON_WORKSPACE_INDEX_PATH":          "/root/state/workspace-index.json",
		"HOLON_INPUT_DIR":                     docker.ContainerInputDir,
		"HOLON_OUTPUT_DIR":                    docker.ContainerOutputDir,
		"HOLON_STATE_DIR":                     docker.ContainerStateDir,
		"CLAUDE_CONFIG_DIR":                   "/state/claude-config",
		"HOLON_CONTROLLER_ROLE":               h.controllerRoleLabel,
		"HOLON_CONTROLLER_EVENT_CHANNEL":      "/state/event-channel.ndjson",
		"HOLON_CONTROLLER_EVENT_CURSOR":       "/state/event-channel.cursor",
		"HOLON_CONTROLLER_ACK_CHANNEL":        "/state/ack-channel.ndjson",
		"HOLON_CONTROLLER_SESSION_STATE_PATH": "/state/controller-session.json",
		"HOLON_CONTROLLER_GOAL_STATE_PATH":    "/state/goal-state.json",
	}
	for k, v := range resolveServeRuntimeEnv(ctx) {
		env[k] = v
	}
	if sessionID := h.readSessionID(); sessionID != "" {
		env["HOLON_CONTROLLER_SESSION_ID"] = sessionID
	}

	session, err := h.sessionRunner.Start(ctx, ControllerSessionConfig{
		Workspace:             h.controllerWorkspace,
		InputPath:             inputDir,
		OutputPath:            outputDir,
		StateDir:              filepath.Join(h.stateDir, "controller-state"),
		AgentHome:             h.agentHome,
		LogLevel:              h.logLevel,
		Env:                   env,
		RuntimeMode:           h.runtimeMode,
		RuntimeDevAgentSource: h.runtimeDevAgentSource,
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

func (h *cliControllerHandler) ensureGoalStateFile() error {
	path := filepath.Join(h.stateDir, "controller-state", "goal-state.json")
	if _, err := os.Stat(path); err == nil {
		return nil
	} else if !os.IsNotExist(err) {
		return fmt.Errorf("failed to stat goal state file: %w", err)
	}

	now := time.Now().UTC().Format(time.RFC3339Nano)
	state := map[string]any{
		"version":          1,
		"goal":             "",
		"milestones":       []any{},
		"active_issue_ids": []any{},
		"updated_at":       now,
	}
	data, err := json.MarshalIndent(state, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal initial goal state: %w", err)
	}
	if err := os.WriteFile(path, append(data, '\n'), 0644); err != nil {
		return fmt.Errorf("failed to write initial goal state: %w", err)
	}
	return nil
}

func resolveServeRuntimeEnv(ctx context.Context) map[string]string {
	result := map[string]string{}
	applyRuntimeAutoEnv(ctx, result, runtimeEnvOptions{
		IncludeClaudeSettingsFallback: true,
		IncludeGitHubActorIdentity:    false,
		IncludeHolonClaudeConfig:      false,
	})
	return result
}

func resolveControllerWorkspace(agentHome string) (string, error) {
	workspace := filepath.Join(agentHome, "workspace")
	if err := os.MkdirAll(workspace, 0755); err != nil {
		return "", fmt.Errorf("failed to create controller workspace at %s: %w", workspace, err)
	}
	return workspace, nil
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
	serveCmd.Flags().StringVar(&serveRepo, "repo", "", "Default repository in owner/repo format (optional repo hint)")
	serveCmd.Flags().StringVar(&serveInput, "input", "-", "Input source for events ('-' for stdin, or path to file)")
	serveCmd.Flags().StringVar(&serveAgentID, "agent-id", "main", "Agent ID (default: main)")
	serveCmd.Flags().StringVar(&serveAgentHome, "agent-home", "", "Agent home directory (overrides --agent-id)")
	serveCmd.Flags().IntVar(&serveMaxEvents, "max-events", 0, "Stop after processing N events (0 = unlimited, not supported in webhook mode)")
	serveCmd.Flags().BoolVar(&serveDryRun, "dry-run", false, "Log forwarded events without starting the controller runtime session")
	serveCmd.Flags().DurationVar(&serveTickInterval, "tick-interval", 0, "Emit timer.tick events periodically (e.g. 5m)")
	serveCmd.Flags().StringVar(&serveLogLevel, "log-level", "progress", "Log level: debug, info, progress, minimal")
	serveCmd.Flags().StringVar(&serveRuntimeMode, "runtime-mode", "prod", "Runtime mode: prod (default), dev (mount local agent dist)")
	serveCmd.Flags().StringVar(&serveRuntimeDevAgentSource, "runtime-dev-agent-source", "", "Local agent source directory for --runtime-mode=dev (defaults: HOLON_RUNTIME_DEV_AGENT_SOURCE, HOLON_DEV_AGENT_SOURCE, ./agents/claude)")
	serveCmd.Flags().IntVar(&serveWebhookPort, "webhook-port", 0, "Override ingress webhook port for subscription mode; with --no-subscriptions, enables legacy webhook mode")
	serveCmd.Flags().BoolVar(&serveNoSubscriptions, "no-subscriptions", false, "Disable agent.yaml subscriptions and use stdin/file input instead")
	rootCmd.AddCommand(serveCmd)
}

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
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
	controllerSocketPath  string
	controllerHTTPClient  *http.Client
	controllerInputDir    string
	controllerOutput      string
	restartAttempts       int
	eventQueue            chan controllerEvent
	turnAckCh             chan serve.TurnAckRecord
	stopCh                chan struct{}
	workerDone            chan struct{}
	handlerCtx            context.Context
	handlerCancel         context.CancelFunc
	pumpStarted           bool
	closeOnce             sync.Once
	mu                    sync.Mutex
}

type controllerEvent struct {
	env      serve.EventEnvelope
	turnID   string
	threadID string
}

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

	handler := &cliControllerHandler{
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
		eventQueue:            make(chan controllerEvent, 128),
		turnAckCh:             make(chan serve.TurnAckRecord, 128),
		stopCh:                make(chan struct{}),
		workerDone:            make(chan struct{}),
	}
	handler.handlerCtx, handler.handlerCancel = context.WithCancel(context.Background())
	handler.pumpStarted = true
	go handler.runEventPump()
	return handler, nil
}

func (h *cliControllerHandler) TurnAcks() <-chan serve.TurnAckRecord {
	h.mu.Lock()
	h.ensurePumpStartedLocked()
	ch := h.turnAckCh
	h.mu.Unlock()
	return ch
}

func (h *cliControllerHandler) ensurePumpStartedLocked() {
	if h.eventQueue == nil {
		h.eventQueue = make(chan controllerEvent, 128)
	}
	if h.turnAckCh == nil {
		h.turnAckCh = make(chan serve.TurnAckRecord, 128)
	}
	if h.stopCh == nil {
		h.stopCh = make(chan struct{})
	}
	if h.workerDone == nil {
		h.workerDone = make(chan struct{})
	}
	if h.handlerCtx == nil || h.handlerCancel == nil {
		h.handlerCtx, h.handlerCancel = context.WithCancel(context.Background())
	}
	if h.pumpStarted {
		return
	}
	h.pumpStarted = true
	go h.runEventPump()
}

func (h *cliControllerHandler) HandleEvent(ctx context.Context, env serve.EventEnvelope) error {
	if h.dryRun {
		holonlog.Info("serve dry-run forward", "event_id", env.ID, "type", env.Type)
		return nil
	}
	return h.enqueueEvent(ctx, controllerEvent{env: env})
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
	return h.enqueueEvent(ctx, controllerEvent{
		env:      env,
		turnID:   turnID,
		threadID: strings.TrimSpace(req.ThreadID),
	})
}

func (h *cliControllerHandler) enqueueEvent(ctx context.Context, item controllerEvent) error {
	h.mu.Lock()
	h.ensurePumpStartedLocked()
	stopCh := h.stopCh
	eventQueue := h.eventQueue
	h.mu.Unlock()

	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-stopCh:
		return fmt.Errorf("controller handler is stopping")
	case eventQueue <- item:
		return nil
	}
}

func (h *cliControllerHandler) runEventPump() {
	defer close(h.workerDone)
	for {
		select {
		case <-h.stopCh:
			return
		case item := <-h.eventQueue:
			if err := h.dispatchQueuedEvent(item); err != nil {
				holonlog.Error("failed to dispatch controller event", "error", err, "event_id", item.env.ID, "type", item.env.Type)
				if item.turnID != "" {
					h.publishTurnAck(serve.TurnAckRecord{
						EventID:  item.env.ID,
						TurnID:   item.turnID,
						ThreadID: item.threadID,
						Status:   "failed",
						Message:  err.Error(),
						At:       time.Now().UTC().Format(time.RFC3339Nano),
					})
				}
			}
		}
	}
}

func (h *cliControllerHandler) dispatchQueuedEvent(item controllerEvent) error {
	ref, err := h.buildRef(item.env)
	if err != nil {
		return err
	}
	h.mu.Lock()
	baseCtx := h.handlerCtx
	h.mu.Unlock()
	ctx, cancel := context.WithTimeout(baseCtx, 5*time.Minute)
	defer cancel()

	result, err := h.postEventWithReconnect(ctx, ref, item.env)
	if err != nil {
		return err
	}
	if item.turnID != "" {
		status := strings.TrimSpace(result.Status)
		if status == "" {
			status = "completed"
		}
		h.publishTurnAck(serve.TurnAckRecord{
			EventID:  firstNonEmpty(strings.TrimSpace(result.EventID), item.env.ID),
			TurnID:   item.turnID,
			ThreadID: firstNonEmpty(strings.TrimSpace(result.ThreadID), item.threadID),
			Status:   status,
			Message:  strings.TrimSpace(result.Message),
			At:       time.Now().UTC().Format(time.RFC3339Nano),
		})
	}
	return nil
}

func (h *cliControllerHandler) publishTurnAck(record serve.TurnAckRecord) {
	select {
	case h.turnAckCh <- record:
	default:
		holonlog.Warn("dropping turn ack due to full buffer", "turn_id", record.TurnID, "status", record.Status)
	}
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
			"goal_hint": "Persistent controller runtime. Receive events via HOLON_CONTROLLER_RPC_SOCKET and decide actions autonomously using available skills.",
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
  description: "Run as a persistent GitHub controller. Receive events via HOLON_CONTROLLER_RPC_SOCKET and decide actions autonomously using available skills."
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
6. Receive event RPC requests from HOLON_CONTROLLER_RPC_SOCKET.
7. For each request, execute autonomously and return a terminal status with optional summary message.
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
	socketPath := filepath.Join(h.agentHome, "run", "agent.sock")
	if err := os.MkdirAll(filepath.Dir(socketPath), 0755); err != nil {
		return fmt.Errorf("failed to create controller socket dir: %w", err)
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
		"HOLON_CONTROLLER_RPC_SOCKET":         filepath.Join(docker.ContainerAgentHome, "run", "agent.sock"),
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

	client := newControllerHTTPClient(socketPath)
	if err := waitForControllerRPCReady(ctx, client); err != nil {
		stopCtx, stopCancel := context.WithTimeout(context.Background(), 5*time.Second)
		_ = h.sessionRunner.Stop(stopCtx, session)
		stopCancel()
		return fmt.Errorf("controller rpc not ready: %w", err)
	}
	h.controllerSession = session
	h.controllerDone = done
	h.controllerSocketPath = socketPath
	h.controllerHTTPClient = client
	h.controllerInputDir = inputDir
	h.controllerOutput = outputDir
	h.restartAttempts++

	holonlog.Info(
		"controller runtime connected",
		"container_id", session.ContainerID,
		"socket", socketPath,
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

type controllerRPCEventRequest struct {
	Event serve.EventEnvelope `json:"event"`
}

type controllerRPCEventResponse struct {
	Status   string `json:"status"`
	Message  string `json:"message,omitempty"`
	EventID  string `json:"event_id,omitempty"`
	TurnID   string `json:"turn_id,omitempty"`
	ThreadID string `json:"thread_id,omitempty"`
}

func newControllerHTTPClient(socketPath string) *http.Client {
	transport := &http.Transport{
		DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
			dialer := &net.Dialer{}
			return dialer.DialContext(ctx, "unix", socketPath)
		},
	}
	return &http.Client{
		Transport: transport,
		Timeout:   3 * time.Minute,
	}
}

func waitForControllerRPCReady(ctx context.Context, client *http.Client) error {
	deadline := time.Now().Add(30 * time.Second)
	for {
		if time.Now().After(deadline) {
			return fmt.Errorf("timed out waiting for controller rpc health endpoint")
		}
		reqCtx := ctx
		if reqCtx == nil {
			reqCtx = context.Background()
		}
		healthCtx, cancel := context.WithTimeout(reqCtx, 1500*time.Millisecond)
		req, err := http.NewRequestWithContext(healthCtx, http.MethodGet, "http://unix/health", nil)
		if err != nil {
			cancel()
			return fmt.Errorf("failed to build health request: %w", err)
		}
		resp, err := client.Do(req)
		cancel()
		if err == nil {
			_ = resp.Body.Close()
			if resp.StatusCode == http.StatusOK {
				return nil
			}
		}
		select {
		case <-time.After(300 * time.Millisecond):
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

func (h *cliControllerHandler) postEventWithReconnect(ctx context.Context, ref string, env serve.EventEnvelope) (controllerRPCEventResponse, error) {
	resp, err := h.postEventRPC(ctx, ref, env)
	if err == nil {
		return resp, nil
	}
	if !isRetryableControllerRPCError(err) {
		return controllerRPCEventResponse{}, err
	}

	holonlog.Warn("controller rpc dispatch failed, restarting session", "error", err)

	h.mu.Lock()
	if h.controllerSession != nil && h.sessionRunner != nil {
		stopCtx, stopCancel := context.WithTimeout(context.Background(), 5*time.Second)
		_ = h.sessionRunner.Stop(stopCtx, h.controllerSession)
		stopCancel()
	}
	h.controllerSession = nil
	h.controllerDone = nil
	h.controllerHTTPClient = nil
	h.controllerSocketPath = ""
	restartErr := h.ensureControllerLocked(ctx, ref)
	client := h.controllerHTTPClient
	h.mu.Unlock()
	if restartErr != nil {
		return controllerRPCEventResponse{}, restartErr
	}
	return postEventRPC(ctx, client, env)
}

func (h *cliControllerHandler) postEventRPC(ctx context.Context, ref string, env serve.EventEnvelope) (controllerRPCEventResponse, error) {
	h.mu.Lock()
	if err := h.ensureControllerLocked(ctx, ref); err != nil {
		h.mu.Unlock()
		return controllerRPCEventResponse{}, err
	}
	client := h.controllerHTTPClient
	h.mu.Unlock()
	return postEventRPC(ctx, client, env)
}

func postEventRPC(ctx context.Context, client *http.Client, env serve.EventEnvelope) (controllerRPCEventResponse, error) {
	payload := controllerRPCEventRequest{Event: env}
	body, err := json.Marshal(payload)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to marshal controller rpc request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, "http://unix/v1/controller/events", bytes.NewReader(body))
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to create controller rpc request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("controller rpc request failed: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to read controller rpc response: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return controllerRPCEventResponse{}, fmt.Errorf("controller rpc status %d: %s", resp.StatusCode, strings.TrimSpace(string(respBody)))
	}
	var result controllerRPCEventResponse
	if err := json.Unmarshal(respBody, &result); err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to parse controller rpc response: %w", err)
	}
	if strings.EqualFold(strings.TrimSpace(result.Status), "failed") {
		return controllerRPCEventResponse{}, fmt.Errorf("controller event execution failed: %s", strings.TrimSpace(result.Message))
	}
	return result, nil
}

func isRetryableControllerRPCError(err error) bool {
	lower := strings.ToLower(err.Error())
	return strings.Contains(lower, "no such file") ||
		strings.Contains(lower, "connection refused") ||
		strings.Contains(lower, "broken pipe") ||
		strings.Contains(lower, "connection reset")
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
}

func (h *cliControllerHandler) Close() error {
	h.closeOnce.Do(func() {
		h.mu.Lock()
		h.ensurePumpStartedLocked()
		stopCh := h.stopCh
		workerDone := h.workerDone
		turnAckCh := h.turnAckCh
		cancel := h.handlerCancel
		h.mu.Unlock()

		cancel()
		close(stopCh)
		select {
		case <-workerDone:
			close(turnAckCh)
		case <-time.After(2 * time.Second):
			// Avoid closing turnAckCh while worker may still be in-flight.
		}
	})

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

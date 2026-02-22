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
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
	"unicode"

	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/prompt"
	"github.com/holon-run/holon/pkg/runtime/docker"
	"github.com/holon-run/holon/pkg/serve"
	"github.com/spf13/cobra"
)

var (
	serveRepo                    string
	serveInput                   string
	serveAgentID                 string
	serveAgentHome               string
	serveSessionID               string
	serveNoDefaultSession        bool
	serveLazyController          bool
	serveControllerWarmupTimeout time.Duration
	serveWarmupBestEffort        bool
	serveMaxEvents               int
	serveDryRun                  bool
	serveLogLevel                string
	serveWebhookPort             int
	serveWebhookMode             bool
	serveTickInterval            time.Duration
	serveNoSubscriptions         bool
	serveRuntimeMode             string
	serveRuntimeDevAgentSource   string
)

const controllerRPCSocketPathInContainer = docker.ContainerAgentHome + "/run/agent.sock"
const defaultControllerEventTimeout = 60 * time.Minute
const defaultTurnProgressHeartbeat = 3 * time.Second

var serveCmd = &cobra.Command{
	Use:   "serve",
	Short: "Run proactive event-driven agent loop",
	Long: `Run an event-driven proactive agent loop.

The command reads event JSON (one object per line) from stdin by default,
normalizes events into an internal envelope, writes runtime logs, and
forwards each event to a persistent agent session.

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
		runtimeExtraMounts, runtimeMountDiagnostics, err := resolveRuntimeMounts(agentResolution.AgentHome)
		if err != nil {
			return fmt.Errorf("failed to resolve runtime mounts: %w", err)
		}
		controllerWorkspace, err := resolveControllerWorkspace(agentResolution.AgentHome)
		if err != nil {
			return err
		}
		resolvedRuntimeMode, err := resolveRuntimeMode(serveRuntimeMode)
		if err != nil {
			return err
		}
		resolvedRuntimeDevAgentSource, runtimeDevAgentSourceOrigin, err := resolveRuntimeDevAgentSourceWithOrigin(resolvedRuntimeMode, serveRuntimeDevAgentSource)
		if err != nil {
			return err
		}
		roleSource := filepath.Join(agentResolution.AgentHome, "ROLE.md")

		handler, err := newCLIControllerHandler(
			serveRepo,
			absStateDir,
			agentResolution.AgentHome,
			controllerWorkspace,
			roleLabel,
			serveLogLevel,
			resolvedRuntimeMode,
			resolvedRuntimeDevAgentSource,
			runtimeExtraMounts,
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

		logDefaultSessionStatus := func() {
			if serveNoDefaultSession {
				holonlog.Info("default session disabled (--no-default-session)")
				return
			}
			holonlog.Info("default session ensured", "session_id", firstNonEmpty(strings.TrimSpace(serveSessionID), "main"))
		}

		warmController := func() error {
			if serveDryRun || serveLazyController || serveControllerWarmupTimeout <= 0 {
				return nil
			}
			warmRef := "local/rpc#0"
			if strings.TrimSpace(serveRepo) != "" {
				warmRef = fmt.Sprintf("%s#0", strings.TrimSpace(serveRepo))
			}
			warmCtx, cancel := context.WithTimeout(parentCtx, serveControllerWarmupTimeout)
			defer cancel()
			holonlog.Info("warming serve runtime", "ref", warmRef, "timeout", serveControllerWarmupTimeout.String())
			if err := handler.ensureController(warmCtx, warmRef); err != nil {
				if serveWarmupBestEffort {
					holonlog.Warn("failed to warm serve runtime; continuing without pre-warm", "ref", warmRef, "error", err)
					return nil
				}
				return fmt.Errorf("failed to warm serve runtime: %w", err)
			}
			holonlog.Info("serve runtime ready; idle waiting for triggers")
			return nil
		}

		// Use subscription manager if subscriptions are enabled.
		// A non-zero --webhook-port acts as an override for subscription ingress port.
		if !serveNoSubscriptions && (serveInput == "" || serveInput == "-") {
			subMgr, err := serve.NewSubscriptionManager(serve.ManagerConfig{
				AgentHome:        agentResolution.AgentHome,
				StateDir:         absStateDir,
				Handler:          handler,
				WebhookPort:      serveWebhookPort,
				TurnDispatcher:   turnDispatcher,
				DefaultSessionID: serveSessionID,
				NoDefaultSession: serveNoDefaultSession,
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

			logDefaultSessionStatus()
			// Warm controller before exposing subscription RPC endpoints to avoid
			// accepting interactive turns while startup may still fail hard.
			if err := warmController(); err != nil {
				return err
			}

			// Start subscription manager
			if err := subMgr.Start(tickCtx); err != nil {
				return fmt.Errorf("failed to start subscription manager: %w", err)
			}

			if serveTickInterval > 0 && len(repos) > 0 {
				for _, repo := range repos {
					startServeTickEmitter(tickCtx, serveTickInterval, repo, func(ctx context.Context, env serve.EventEnvelope) error {
						return subMgr.InjectEvent(ctx, env)
					})
				}
			}
			status := subMgr.Status()
			startup := buildServeStartupDiagnostics(serveStartupDiagnosticsInput{
				AgentID:                   agentResolution.AgentID,
				AgentHome:                 agentResolution.AgentHome,
				StateDir:                  absStateDir,
				Workspace:                 controllerWorkspace,
				RoleSource:                roleSource,
				RoleInferred:              roleLabel,
				ConfigSource:              filepath.Join(agentResolution.AgentHome, "agent.yaml"),
				InputMode:                 "subscription",
				SubscriptionEnabled:       true,
				SubscriptionStatus:        status,
				RepoHint:                  serveRepo,
				TickInterval:              serveTickInterval,
				WebhookPort:               subMgr.GetWebhookPort(),
				RuntimeMode:               resolvedRuntimeMode,
				RuntimeDevAgentSource:     resolvedRuntimeDevAgentSource,
				RuntimeDevAgentSourceFrom: runtimeDevAgentSourceOrigin,
				RuntimeMounts:             runtimeMountDiagnostics,
				ServeInput:                serveInput,
				NoSubscriptionsFlag:       serveNoSubscriptions,
			})
			if err := writeServeStartupDiagnostics(absStateDir, startup); err != nil {
				return err
			}
			logServeStartupDiagnostics(startup)

			// Wait for context cancellation
			holonlog.Info("idle: waiting for rpc/events")
			<-tickCtx.Done()
			return nil
		}

		// Webhook mode (legacy, for backward compatibility)
		if serveWebhookMode {
			if serveTickInterval > 0 && strings.TrimSpace(serveRepo) == "" {
				return fmt.Errorf("--repo is required when --tick-interval is set in webhook mode")
			}
			webhookSrv, err := serve.NewWebhookServer(serve.WebhookConfig{
				Port:             serveWebhookPort,
				RepoHint:         serveRepo,
				StateDir:         absStateDir,
				Handler:          handler,
				TurnDispatcher:   turnDispatcher,
				DefaultSessionID: serveSessionID,
				NoDefaultSession: serveNoDefaultSession,
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
			startup := buildServeStartupDiagnostics(serveStartupDiagnosticsInput{
				AgentID:                   agentResolution.AgentID,
				AgentHome:                 agentResolution.AgentHome,
				StateDir:                  absStateDir,
				Workspace:                 controllerWorkspace,
				RoleSource:                roleSource,
				RoleInferred:              roleLabel,
				ConfigSource:              filepath.Join(agentResolution.AgentHome, "agent.yaml"),
				InputMode:                 "webhook_legacy",
				SubscriptionEnabled:       false,
				RepoHint:                  serveRepo,
				TickInterval:              serveTickInterval,
				WebhookPort:               serveWebhookPort,
				RuntimeMode:               resolvedRuntimeMode,
				RuntimeDevAgentSource:     resolvedRuntimeDevAgentSource,
				RuntimeDevAgentSourceFrom: runtimeDevAgentSourceOrigin,
				RuntimeMounts:             runtimeMountDiagnostics,
				ServeInput:                serveInput,
				NoSubscriptionsFlag:       serveNoSubscriptions,
			})
			if err := writeServeStartupDiagnostics(absStateDir, startup); err != nil {
				return err
			}
			logServeStartupDiagnostics(startup)
			logDefaultSessionStatus()
			if err := warmController(); err != nil {
				return err
			}
			holonlog.Info("idle: waiting for rpc/events")
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

		startup := buildServeStartupDiagnostics(serveStartupDiagnosticsInput{
			AgentID:                   agentResolution.AgentID,
			AgentHome:                 agentResolution.AgentHome,
			StateDir:                  absStateDir,
			Workspace:                 controllerWorkspace,
			RoleSource:                roleSource,
			RoleInferred:              roleLabel,
			ConfigSource:              filepath.Join(agentResolution.AgentHome, "agent.yaml"),
			InputMode:                 "stdin_file",
			SubscriptionEnabled:       false,
			RepoHint:                  serveRepo,
			TickInterval:              serveTickInterval,
			RuntimeMode:               resolvedRuntimeMode,
			RuntimeDevAgentSource:     resolvedRuntimeDevAgentSource,
			RuntimeDevAgentSourceFrom: runtimeDevAgentSourceOrigin,
			RuntimeMounts:             runtimeMountDiagnostics,
			ServeInput:                serveInput,
			NoSubscriptionsFlag:       serveNoSubscriptions,
		})
		if err := writeServeStartupDiagnostics(absStateDir, startup); err != nil {
			return err
		}
		logServeStartupDiagnostics(startup)
		logDefaultSessionStatus()
		if err := warmController(); err != nil {
			return err
		}
		holonlog.Info("idle: waiting for input events")
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
	extraMounts           []docker.ExtraMount
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
	maxConcurrent         int
	sessionLocks          map[string]*sessionLockEntry
	turnDispatch          map[string]*turnDispatchState
	sessionEpoch          map[string]uint64
	sessionEpochReason    map[string]string
	turnEventIndex        map[string]string
	turnEventIndexPath    string
	activityLedgerPath    string
	activityLedgerMu      sync.Mutex
	activityEmitter       serve.ActivityEmitter
	sessionQueuedTurns    map[string][]string
	followupPolicy        serveFollowupPolicy
	maxQueuedTurns        int
	closeOnce             sync.Once
	mu                    sync.Mutex
}

type controllerEvent struct {
	env              serve.EventEnvelope
	sessionKey       string
	turnID           string
	threadID         string
	sessionEpoch     uint64
	skipMainAnnounce bool
}

type eventOutcomeRecord struct {
	EventID     string   `json:"event_id"`
	Status      string   `json:"status"`
	HasAction   bool     `json:"has_action"`
	ActionRefs  []string `json:"action_refs,omitempty"`
	Message     string   `json:"message,omitempty"`
	CompletedAt string   `json:"completed_at"`
}

type resolvedWorkspace struct {
	Ref           string
	HostPath      string
	ContainerPath string
}

const defaultServeSessionConcurrency = 4
const defaultServeMaxQueuedTurns = 8
const maxSessionLockEntries = 1024
const sessionLockRetention = 10 * time.Minute

type serveFollowupPolicy string

const (
	serveFollowupPolicyFollowup  serveFollowupPolicy = "followup"
	serveFollowupPolicyInterrupt serveFollowupPolicy = "interrupt"
	serveFollowupPolicyCollect   serveFollowupPolicy = "collect"
)

type sessionLockEntry struct {
	mu       sync.Mutex
	active   int
	lastUsed time.Time
}

type turnDispatchState struct {
	TurnID           string
	ThreadID         string
	SessionKey       string
	EventID          string
	Status           string
	CancelRequested  bool
	CancelReason     string
	LastUpdatedAt    time.Time
	DispatchCancelFn context.CancelFunc
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
	extraMounts []docker.ExtraMount,
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
		extraMounts:           append([]docker.ExtraMount(nil), extraMounts...),
		dryRun:                dryRun,
		sessionRunner:         sessionRunner,
		eventQueue:            make(chan controllerEvent, 128),
		turnAckCh:             make(chan serve.TurnAckRecord, 128),
		stopCh:                make(chan struct{}),
		workerDone:            make(chan struct{}),
		maxConcurrent:         resolveServeSessionConcurrency(),
		sessionLocks:          make(map[string]*sessionLockEntry),
		turnDispatch:          make(map[string]*turnDispatchState),
		sessionEpoch:          make(map[string]uint64),
		sessionEpochReason:    make(map[string]string),
		turnEventIndex:        make(map[string]string),
		turnEventIndexPath:    filepath.Join(stateDir, "controller-state", "turn-event-index.json"),
		activityLedgerPath:    filepath.Join(stateDir, "controller-state", "activity-ledger.ndjson"),
		sessionQueuedTurns:    make(map[string][]string),
		followupPolicy:        resolveServeFollowupPolicy(),
		maxQueuedTurns:        resolveServeMaxQueuedTurns(),
	}
	if err := handler.loadTurnEventIndex(); err != nil {
		return nil, err
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
	if h.maxConcurrent <= 0 {
		h.maxConcurrent = resolveServeSessionConcurrency()
	}
	if h.sessionLocks == nil {
		h.sessionLocks = make(map[string]*sessionLockEntry)
	}
	if h.turnDispatch == nil {
		h.turnDispatch = make(map[string]*turnDispatchState)
	}
	if h.sessionEpoch == nil {
		h.sessionEpoch = make(map[string]uint64)
	}
	if h.sessionEpochReason == nil {
		h.sessionEpochReason = make(map[string]string)
	}
	if h.turnEventIndex == nil {
		h.turnEventIndex = make(map[string]string)
	}
	if h.sessionQueuedTurns == nil {
		h.sessionQueuedTurns = make(map[string][]string)
	}
	if strings.TrimSpace(h.turnEventIndexPath) == "" {
		h.turnEventIndexPath = filepath.Join(h.stateDir, "controller-state", "turn-event-index.json")
	}
	if strings.TrimSpace(h.activityLedgerPath) == "" {
		h.activityLedgerPath = filepath.Join(h.stateDir, "controller-state", "activity-ledger.ndjson")
	}
	if h.followupPolicy == "" {
		h.followupPolicy = resolveServeFollowupPolicy()
	}
	if h.maxQueuedTurns <= 0 {
		h.maxQueuedTurns = resolveServeMaxQueuedTurns()
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
	return h.enqueueEvent(ctx, controllerEvent{
		env:        env,
		sessionKey: routeEventToSessionKey(env),
	})
}

func (h *cliControllerHandler) HandleTurnStart(ctx context.Context, req serve.TurnStartRequest, turnID string) error {
	threadID := normalizeSessionKey(req.ThreadID)
	if threadID == "" {
		threadID = "main"
	}
	payload := map[string]any{
		"turn_id":          turnID,
		"thread_id":        threadID,
		"session_key":      threadID,
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
			Repo:      repo,
			Partition: threadID,
		},
		Subject: serve.EventSubject{
			Kind: "thread",
			ID:   threadID,
		},
		DedupeKey: fmt.Sprintf("rpc:turn:%s:%s", threadID, turnID),
		Payload:   payloadRaw,
	}
	return h.enqueueEvent(ctx, controllerEvent{
		env:        env,
		sessionKey: threadID,
		turnID:     turnID,
		threadID:   threadID,
	})
}

func (h *cliControllerHandler) InterruptTurn(ctx context.Context, turnID, threadID, reason string) error {
	turnID = strings.TrimSpace(turnID)
	if turnID == "" {
		return fmt.Errorf("turn_id is required")
	}
	reason = strings.TrimSpace(reason)
	if reason == "" {
		reason = "user requested interrupt"
	}

	h.mu.Lock()
	h.ensurePumpStartedLocked()
	state, ok := h.turnDispatch[turnID]
	if !ok {
		h.mu.Unlock()
		return fmt.Errorf("turn %s is not tracked in dispatcher", turnID)
	}
	sessionKey := normalizeSessionKey(state.SessionKey)
	if sessionKey == "" {
		sessionKey = normalizeSessionKey(threadID)
	}
	if sessionKey == "" {
		sessionKey = "main"
	}
	h.advanceSessionEpochLocked(sessionKey, "turn interrupted by user request")
	state.CancelRequested = true
	state.CancelReason = reason
	state.Status = "cancel_requested"
	state.LastUpdatedAt = time.Now().UTC()
	threadIDSnapshot := strings.TrimSpace(state.ThreadID)
	eventID := strings.TrimSpace(state.EventID)
	if eventID == "" && h.turnEventIndex != nil {
		eventID = strings.TrimSpace(h.turnEventIndex[turnID])
	}
	h.mu.Unlock()

	h.publishTurnAck(serve.TurnAckRecord{
		EventID:  eventID,
		TurnID:   turnID,
		ThreadID: firstNonEmpty(threadIDSnapshot, threadID),
		Status:   "cancel_requested",
		Message:  reason,
		At:       time.Now().UTC().Format(time.RFC3339Nano),
	})

	if eventID == "" {
		h.publishTurnAck(serve.TurnAckRecord{
			TurnID:   turnID,
			ThreadID: firstNonEmpty(threadIDSnapshot, threadID),
			Status:   "interrupted",
			Message:  reason,
			At:       time.Now().UTC().Format(time.RFC3339Nano),
		})
		h.clearTurnDispatch(turnID)
		return nil
	}

	cancelCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	resp, err := h.cancelEventWithReconnect(cancelCtx, "local/rpc#0", eventID, reason)
	if err != nil {
		return err
	}
	cancelStatus := strings.ToLower(strings.TrimSpace(resp.Status))
	if cancelStatus == "interrupted" || cancelStatus == "failed" || cancelStatus == "completed" {
		h.publishTurnAck(serve.TurnAckRecord{
			EventID:  eventID,
			TurnID:   turnID,
			ThreadID: firstNonEmpty(strings.TrimSpace(resp.ThreadID), threadIDSnapshot, threadID),
			Status:   cancelStatus,
			Message:  firstNonEmpty(strings.TrimSpace(resp.Message), reason),
			At:       time.Now().UTC().Format(time.RFC3339Nano),
		})
		if cancelStatus != "cancel_requested" {
			h.clearTurnDispatch(turnID)
		}
	}
	return nil
}

func (h *cliControllerHandler) enqueueEvent(ctx context.Context, item controllerEvent) error {
	h.mu.Lock()
	h.ensurePumpStartedLocked()
	stopCh := h.stopCh
	eventQueue := h.eventQueue
	sessionKey := normalizeSessionKey(item.sessionKey)
	if sessionKey == "" {
		sessionKey = "main"
	}
	if item.turnID != "" {
		queuedCount := h.queuedTurnsCountLocked(sessionKey)
		if h.maxQueuedTurns > 0 && queuedCount >= h.maxQueuedTurns {
			h.mu.Unlock()
			return fmt.Errorf("session %s queue is full (%d queued turns)", sessionKey, h.maxQueuedTurns)
		}
		switch h.followupPolicy {
		case serveFollowupPolicyCollect:
			if queuedCount > 0 {
				item.sessionEpoch = h.advanceSessionEpochLocked(sessionKey, "superseded by newer turn (collect policy)")
			}
		case serveFollowupPolicyInterrupt:
			if queuedCount > 0 {
				item.sessionEpoch = h.advanceSessionEpochLocked(sessionKey, "superseded by newer turn (interrupt policy)")
			}
		}
		if item.sessionEpoch == 0 {
			item.sessionEpoch = h.sessionEpoch[sessionKey]
		}
		threadID := normalizeSessionKey(item.threadID)
		if threadID == "" {
			threadID = sessionKey
		}
		h.enqueueTurnLocked(sessionKey, item.turnID)
		h.turnDispatch[item.turnID] = &turnDispatchState{
			TurnID:        item.turnID,
			ThreadID:      threadID,
			SessionKey:    sessionKey,
			Status:        "queued",
			LastUpdatedAt: time.Now().UTC(),
		}
	} else if item.sessionEpoch == 0 {
		item.sessionEpoch = h.sessionEpoch[sessionKey]
	}
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
	sem := make(chan struct{}, h.maxConcurrent)
	var wg sync.WaitGroup
	for {
		select {
		case <-h.stopCh:
			wg.Wait()
			return
		case item := <-h.eventQueue:
			wg.Add(1)
			go func(item controllerEvent) {
				defer wg.Done()
				sessionKey := normalizeSessionKey(item.sessionKey)
				if sessionKey == "" {
					sessionKey = "main"
				}
				lockEntry := h.getSessionLock(sessionKey)
				defer h.releaseSessionLock(sessionKey, lockEntry)

				select {
				case <-h.stopCh:
					return
				case sem <- struct{}{}:
				}
				defer func() {
					<-sem
				}()
				lockEntry.mu.Lock()
				defer lockEntry.mu.Unlock()

				if shouldSkip, skipReason := h.shouldSkipQueuedEvent(item, sessionKey); shouldSkip {
					if item.turnID != "" {
						h.publishTurnAck(serve.TurnAckRecord{
							EventID:  item.env.ID,
							TurnID:   item.turnID,
							ThreadID: item.threadID,
							Status:   "interrupted",
							Message:  firstNonEmpty(skipReason, "turn canceled before execution"),
							At:       time.Now().UTC().Format(time.RFC3339Nano),
						})
						h.clearTurnDispatch(item.turnID)
					}
					return
				}

				if err := h.dispatchQueuedEvent(item); err != nil {
					holonlog.Error("failed to dispatch controller event", "error", err, "event_id", item.env.ID, "type", item.env.Type, "session_key", sessionKey)
					if item.turnID != "" {
						h.clearTurnDispatch(item.turnID)
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
			}(item)
		}
	}
}

func (h *cliControllerHandler) getSessionLock(sessionKey string) *sessionLockEntry {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.sessionLocks == nil {
		h.sessionLocks = make(map[string]*sessionLockEntry)
	}
	lock, ok := h.sessionLocks[sessionKey]
	if !ok {
		lock = &sessionLockEntry{}
		h.sessionLocks[sessionKey] = lock
	}
	lock.active++
	return lock
}

func (h *cliControllerHandler) releaseSessionLock(sessionKey string, entry *sessionLockEntry) {
	h.mu.Lock()
	defer h.mu.Unlock()
	current, ok := h.sessionLocks[sessionKey]
	if !ok || current != entry {
		return
	}
	if current.active > 0 {
		current.active--
	}
	current.lastUsed = time.Now().UTC()
	if len(h.sessionLocks) <= maxSessionLockEntries {
		return
	}
	cutoff := time.Now().UTC().Add(-sessionLockRetention)
	for key, candidate := range h.sessionLocks {
		if candidate.active == 0 && !candidate.lastUsed.IsZero() && candidate.lastUsed.Before(cutoff) {
			delete(h.sessionLocks, key)
		}
	}
}

func (h *cliControllerHandler) shouldSkipQueuedEvent(item controllerEvent, sessionKey string) (bool, string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.sessionEpoch == nil {
		return false, ""
	}
	currentEpoch := h.sessionEpoch[sessionKey]
	if item.sessionEpoch < currentEpoch {
		reason := strings.TrimSpace(h.sessionEpochReason[sessionKey])
		if reason == "" {
			reason = "superseded by newer session work"
		}
		return true, reason
	}
	return false, ""
}

func (h *cliControllerHandler) clearTurnDispatch(turnID string) {
	turnID = strings.TrimSpace(turnID)
	if turnID == "" {
		return
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.turnDispatch != nil {
		if existing, ok := h.turnDispatch[turnID]; ok {
			h.dequeueTurnLocked(strings.TrimSpace(existing.SessionKey), turnID)
		}
		delete(h.turnDispatch, turnID)
	}
	if h.turnEventIndex != nil {
		delete(h.turnEventIndex, turnID)
	}
	_ = h.saveTurnEventIndexLocked()
}

func (h *cliControllerHandler) advanceSessionEpochLocked(sessionKey, reason string) uint64 {
	if h.sessionEpoch == nil {
		h.sessionEpoch = make(map[string]uint64)
	}
	if h.sessionEpochReason == nil {
		h.sessionEpochReason = make(map[string]string)
	}
	h.sessionEpoch[sessionKey]++
	h.sessionEpochReason[sessionKey] = strings.TrimSpace(reason)
	return h.sessionEpoch[sessionKey]
}

func (h *cliControllerHandler) queuedTurnsCountLocked(sessionKey string) int {
	if h.sessionQueuedTurns == nil {
		return 0
	}
	return len(h.sessionQueuedTurns[sessionKey])
}

func (h *cliControllerHandler) enqueueTurnLocked(sessionKey, turnID string) {
	if h.sessionQueuedTurns == nil {
		h.sessionQueuedTurns = make(map[string][]string)
	}
	h.sessionQueuedTurns[sessionKey] = append(h.sessionQueuedTurns[sessionKey], turnID)
}

func (h *cliControllerHandler) dequeueTurnLocked(sessionKey, turnID string) {
	if h.sessionQueuedTurns == nil {
		return
	}
	queued := h.sessionQueuedTurns[sessionKey]
	if len(queued) == 0 {
		return
	}
	filtered := make([]string, 0, len(queued))
	for _, existing := range queued {
		if existing == turnID {
			continue
		}
		filtered = append(filtered, existing)
	}
	if len(filtered) == 0 {
		delete(h.sessionQueuedTurns, sessionKey)
		return
	}
	h.sessionQueuedTurns[sessionKey] = filtered
}

func (h *cliControllerHandler) dispatchQueuedEvent(item controllerEvent) (retErr error) {
	sessionKey := normalizeSessionKey(item.sessionKey)
	if sessionKey == "" {
		sessionKey = "main"
	}
	var result controllerRPCEventResponse
	defer func() {
		outcome, outcomeErr := h.recordEventOutcome(item, result, retErr)
		if outcomeErr != nil {
			holonlog.Warn("failed to record event outcome", "event_id", item.env.ID, "error", outcomeErr)
			return
		}
		emit, reason := shouldEmitActivity(outcome)
		if !emit {
			holonlog.Debug(
				"activity_skipped",
				"event_id", outcome.EventID,
				"status", outcome.Status,
				"has_action", outcome.HasAction,
				"gating_reason", reason,
			)
			return
		}
		if emitErr := h.emitActivityForEvent(item, outcome, reason); emitErr != nil {
			holonlog.Warn("failed to emit activity", "event_id", outcome.EventID, "error", emitErr, "gating_reason", reason)
		}
	}()
	resolvedWorkspace, err := h.resolveWorkspaceForEvent(item.env)
	if err != nil {
		return err
	}
	enriched := item.env
	enriched.Scope.WorkspaceRef = resolvedWorkspace.Ref
	enriched.Scope.WorkspacePath = resolvedWorkspace.ContainerPath
	ref, err := h.buildRef(item.env)
	if err != nil {
		return err
	}
	h.mu.Lock()
	baseCtx := h.handlerCtx
	h.mu.Unlock()
	ctx, cancel := context.WithTimeout(baseCtx, resolveControllerEventTimeout())
	defer cancel()
	if item.turnID != "" {
		h.mu.Lock()
		if state, ok := h.turnDispatch[item.turnID]; ok {
			state.DispatchCancelFn = cancel
			state.Status = "running"
			state.LastUpdatedAt = time.Now().UTC()
		}
		h.mu.Unlock()
	}

	holonlog.Info(
		"serve workspace resolved",
		"event_id", item.env.ID,
		"session_key", sessionKey,
		"repo", firstNonEmpty(item.env.Scope.Repo, h.repoHint),
		"workspace_ref", resolvedWorkspace.Ref,
		"workspace_host_path", resolvedWorkspace.HostPath,
		"workspace_container_path", resolvedWorkspace.ContainerPath,
	)
	dispatchStartedAt := time.Now().UTC()
	result, err = h.postEventWithReconnect(ctx, ref, sessionKey, enriched)
	if err != nil {
		return err
	}
	if isControllerEventPendingStatus(result.Status) {
		eventID := firstNonEmpty(strings.TrimSpace(result.EventID), item.env.ID)
		if strings.TrimSpace(eventID) == "" {
			return fmt.Errorf("controller accepted event but did not provide event_id")
		}
		if item.turnID != "" {
			initialStatus := normalizeControllerProgressState(result.Status)
			initialMessage := buildTurnProgressMessage(initialStatus, result.Message, item.env, 0)
			h.mu.Lock()
			if state, ok := h.turnDispatch[item.turnID]; ok {
				state.EventID = eventID
				state.Status = initialStatus
				state.LastUpdatedAt = time.Now().UTC()
			}
			h.mu.Unlock()
			if err := h.setTurnEventIndex(item.turnID, eventID); err != nil {
				return fmt.Errorf("failed to persist turn-event mapping for turn %s: %w", item.turnID, err)
			}
			h.publishTurnAck(serve.TurnAckRecord{
				EventID:  eventID,
				TurnID:   item.turnID,
				ThreadID: firstNonEmpty(strings.TrimSpace(result.ThreadID), item.threadID),
				Status:   initialStatus,
				Message:  initialMessage,
				At:       time.Now().UTC().Format(time.RFC3339Nano),
			})
		}
		result, err = h.waitForControllerEventResult(ctx, ref, sessionKey, eventID, func(progress controllerRPCEventResponse, elapsed time.Duration) {
			if item.turnID == "" {
				return
			}
			progressStatus := normalizeControllerProgressState(progress.Status)
			progressMessage := buildTurnProgressMessage(progressStatus, progress.Message, item.env, elapsed)
			h.mu.Lock()
			if state, ok := h.turnDispatch[item.turnID]; ok {
				state.Status = progressStatus
				state.LastUpdatedAt = time.Now().UTC()
			}
			h.mu.Unlock()
			h.publishTurnAck(serve.TurnAckRecord{
				EventID:  eventID,
				TurnID:   item.turnID,
				ThreadID: firstNonEmpty(strings.TrimSpace(progress.ThreadID), item.threadID),
				Status:   progressStatus,
				Message:  progressMessage,
				At:       dispatchStartedAt.Add(elapsed).Format(time.RFC3339Nano),
			})
		})
		if err != nil {
			return err
		}
	}
	if item.turnID != "" {
		status := strings.TrimSpace(result.Status)
		if status == "" {
			status = "completed"
		}
		h.publishTurnAck(serve.TurnAckRecord{
			EventID:       firstNonEmpty(strings.TrimSpace(result.EventID), item.env.ID),
			TurnID:        item.turnID,
			ThreadID:      firstNonEmpty(strings.TrimSpace(result.ThreadID), item.threadID),
			Status:        status,
			Message:       strings.TrimSpace(result.Message),
			At:            time.Now().UTC().Format(time.RFC3339Nano),
			WorkspaceRef:  resolvedWorkspace.Ref,
			WorkspacePath: resolvedWorkspace.ContainerPath,
		})
		h.clearTurnDispatch(item.turnID)
	}
	return nil
}

func (h *cliControllerHandler) resolveWorkspaceForEvent(env serve.EventEnvelope) (resolvedWorkspace, error) {
	repo := strings.TrimSpace(env.Scope.Repo)
	if repo == "" {
		repo = strings.TrimSpace(h.repoHint)
	}
	if repo == "" {
		repo = "local/rpc"
	}
	ref := h.workspaceRefFromEvent(env)
	hostPath, workspaceRef, err := workspacePathForRepoRef(h.controllerWorkspace, repo, ref)
	if err != nil {
		return resolvedWorkspace{}, err
	}
	if err := os.MkdirAll(hostPath, 0755); err != nil {
		return resolvedWorkspace{}, fmt.Errorf("failed to create workspace for event %s: %w", env.ID, err)
	}
	owner, name, _ := strings.Cut(repo, "/")
	containerPath := filepath.Join(docker.ContainerWorkspaceDir, "repos", sanitizeWorkspaceSegment(owner), sanitizeWorkspaceSegment(name), sanitizeWorkspaceSegment(ref))
	return resolvedWorkspace{
		Ref:           workspaceRef,
		HostPath:      hostPath,
		ContainerPath: containerPath,
	}, nil
}

func (h *cliControllerHandler) workspaceRefFromEvent(env serve.EventEnvelope) string {
	if ref := pullRequestHeadRefFromPayload(env.Payload); ref != "" {
		return ref
	}
	if env.Type == "timer.tick" || env.Type == "rpc.turn.input" {
		return defaultWorkspaceTrack
	}
	return defaultWorkspaceTrack
}

func pullRequestHeadRefFromPayload(payload json.RawMessage) string {
	if len(payload) == 0 {
		return ""
	}
	var envelope struct {
		PullRequest struct {
			Head struct {
				SHA string `json:"sha"`
				Ref string `json:"ref"`
			} `json:"head"`
		} `json:"pull_request"`
	}
	if err := json.Unmarshal(payload, &envelope); err != nil {
		return ""
	}
	if strings.TrimSpace(envelope.PullRequest.Head.SHA) != "" {
		return strings.TrimSpace(envelope.PullRequest.Head.SHA)
	}
	if strings.TrimSpace(envelope.PullRequest.Head.Ref) != "" {
		return strings.TrimSpace(envelope.PullRequest.Head.Ref)
	}
	return ""
}

func (h *cliControllerHandler) publishTurnAck(record serve.TurnAckRecord) {
	select {
	case h.turnAckCh <- record:
	default:
		holonlog.Warn("dropping turn ack due to full buffer", "turn_id", record.TurnID, "status", record.Status)
	}
}

func (h *cliControllerHandler) SetActivityEmitter(emitter serve.ActivityEmitter) {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.activityEmitter = emitter
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
			"goal_hint": "Persistent serve runtime. Receive events via HOLON_RUNTIME_RPC_SOCKET and decide actions autonomously using available skills.",
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
  name: "github-agent-session"
goal:
  description: "Run as a persistent GitHub agent session. Receive events via HOLON_RUNTIME_RPC_SOCKET and decide actions autonomously using available skills."
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
	compiler := prompt.NewCompiler("")
	cfg := prompt.Config{
		Mode:       "serve",
		WorkingDir: docker.ContainerWorkspaceDir,
	}

	systemPrompt, err := compiler.CompileSystemPrompt(cfg)
	if err != nil {
		return "", "", fmt.Errorf("failed to compile serve system prompt: %w", err)
	}

	userPrompt, err := compiler.CompileModeUserPrompt(cfg)
	if err != nil {
		return "", "", fmt.Errorf("failed to compile serve user prompt: %w", err)
	}

	return strings.TrimSpace(systemPrompt), strings.TrimSpace(userPrompt), nil
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

func (h *cliControllerHandler) ensureController(ctx context.Context, ref string) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.ensureControllerLocked(ctx, ref)
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
	if err := h.ensureControllerClaudeConfig(); err != nil {
		return err
	}
	socketPath := filepath.Join(h.agentHome, "run", "agent.sock")
	if err := os.MkdirAll(filepath.Dir(socketPath), 0755); err != nil {
		return fmt.Errorf("failed to create controller socket dir: %w", err)
	}
	if err := removeStaleControllerSocket(socketPath); err != nil {
		return err
	}
	inputDir, err := h.buildInputDir(ref)
	if err != nil {
		return err
	}
	outputDir := filepath.Join(h.stateDir, "controller-runtime", "output")
	if err := os.MkdirAll(outputDir, 0755); err != nil {
		return fmt.Errorf("failed to create controller output dir: %w", err)
	}

	// Keep controller state paths derived from ContainerStateDir so runtime
	// paths remain consistent with the /root-scoped container layout.
	env := map[string]string{
		"HOLON_AGENT_SESSION_MODE":         "serve",
		"HOLON_AGENT_HOME":                 docker.ContainerAgentHome,
		"HOLON_WORKSPACE_DIR":              docker.ContainerWorkspaceDir,
		"HOLON_WORKSPACE_INDEX_PATH":       filepath.Join(docker.ContainerStateDir, "workspace-index.json"),
		"HOLON_INPUT_DIR":                  docker.ContainerInputDir,
		"HOLON_OUTPUT_DIR":                 docker.ContainerOutputDir,
		"GITHUB_OUTPUT_DIR":                docker.ContainerOutputDir,
		"GITHUB_CONTEXT_DIR":               filepath.Join(docker.ContainerOutputDir, "github-context"),
		"HOLON_STATE_DIR":                  docker.ContainerStateDir,
		"CLAUDE_CONFIG_DIR":                filepath.Join(docker.ContainerStateDir, "claude-config"),
		"HOLON_RUNTIME_ROLE":               h.controllerRoleLabel,
		"HOLON_RUNTIME_RPC_SOCKET":         controllerRPCSocketPathInContainer,
		"HOLON_RUNTIME_SESSION_STATE_PATH": filepath.Join(docker.ContainerStateDir, "controller-session.json"),
		"HOLON_RUNTIME_GOAL_STATE_PATH":    filepath.Join(docker.ContainerStateDir, "goal-state.json"),
	}
	for k, v := range resolveServeRuntimeEnv(ctx) {
		env[k] = v
	}
	if sessionID := h.readSessionID(); sessionID != "" {
		env["HOLON_RUNTIME_SESSION_ID"] = sessionID
	}

	session, err := h.sessionRunner.Start(ctx, RuntimeSessionConfig{
		Workspace:             h.controllerWorkspace,
		InputPath:             inputDir,
		OutputPath:            outputDir,
		StateDir:              filepath.Join(h.stateDir, "controller-state"),
		AgentHome:             h.agentHome,
		LogLevel:              h.logLevel,
		Env:                   env,
		RuntimeMode:           h.runtimeMode,
		RuntimeDevAgentSource: h.runtimeDevAgentSource,
		ExtraMounts:           append([]docker.ExtraMount(nil), h.extraMounts...),
	})
	if err != nil {
		return fmt.Errorf("failed to start controller runtime: %w", err)
	}

	done := make(chan error, 1)
	go func() {
		done <- h.sessionRunner.Wait(context.Background(), session)
	}()

	client := newControllerHTTPClient(socketPath)
	if err := waitForControllerRPCReady(ctx, client, session.ContainerID); err != nil {
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

func (h *cliControllerHandler) ensureControllerClaudeConfig() error {
	configDir := filepath.Join(h.stateDir, "controller-state", "claude-config")
	if err := os.MkdirAll(configDir, 0755); err != nil {
		return fmt.Errorf("failed to create controller claude-config dir: %w", err)
	}
	settings := map[string]any{
		"permissionMode": "bypassPermissions",
		"permissions": map[string]any{
			"allow": []string{
				"Bash(gh:*)",
				"Bash(git:*)",
				"Bash(ls:*)",
				"Bash(cat:*)",
				"Bash(pwd)",
				"Bash(mkdir:*)",
				"Bash(cp:*)",
				"Bash(echo:*)",
				"Bash(printenv:*)",
				"Bash(env)",
				"Bash(find:*)",
				"Bash(grep:*)",
				"Bash(sed:*)",
				"Bash(head:*)",
				"Bash(tail:*)",
				"Bash(tr:*)",
				"Bash(jq:*)",
				"Bash(curl:*)",
				"Bash(node:*)",
				"Bash(python3:*)",
				"Read(/root/workspace/**)",
				"Edit(/root/workspace/**)",
				"Write(/root/workspace/**)",
				"Glob(*)",
				"Grep(*)",
				"NotebookEdit(*)",
				"TodoWrite(*)",
				"WebFetch(*)",
				"WebSearch(*)",
				"KillShell(*)",
				"AskUserQuestion(*)",
				"Task(subagent_type:*)",
				"Skill(*)",
				"EnterPlanMode",
				"ExitPlanMode",
				"LSP(*)",
			},
			"deny": []string{},
			"ask":  []string{},
		},
		"skipDangerousModePermissionPrompt": true,
	}
	data, err := json.MarshalIndent(settings, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal controller claude settings: %w", err)
	}
	if err := os.WriteFile(filepath.Join(configDir, "settings.json"), append(data, '\n'), 0644); err != nil {
		return fmt.Errorf("failed to write controller claude settings: %w", err)
	}
	claudeConfig := map[string]any{
		"version":                       "1.0",
		"autoUpdate":                    false,
		"telemetry":                     false,
		"hasCompletedOnboarding":        true,
		"bypassPermissionsModeAccepted": true,
	}
	configData, err := json.MarshalIndent(claudeConfig, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal controller .claude.json: %w", err)
	}
	if err := os.WriteFile(filepath.Join(configDir, ".claude.json"), append(configData, '\n'), 0644); err != nil {
		return fmt.Errorf("failed to write controller .claude.json: %w", err)
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
	return resolveAgentWorkspaceRoot(agentHome)
}

func normalizeSessionKey(raw string) string {
	return strings.TrimSpace(raw)
}

func routeEventToSessionKey(env serve.EventEnvelope) string {
	if explicit := payloadSessionKey(env.Payload); explicit != "" {
		return normalizeSessionKey(explicit)
	}
	if partition := normalizeSessionKey(env.Scope.Partition); partition != "" {
		return "event:" + sanitizeSessionPartition(partition)
	}
	if repo := normalizeSessionKey(env.Scope.Repo); repo != "" {
		return "event:" + sanitizeSessionPartition(repo)
	}
	source := normalizeSessionKey(env.Source)
	subjectKind := normalizeSessionKey(env.Subject.Kind)
	subjectID := normalizeSessionKey(env.Subject.ID)
	if source != "" && subjectKind != "" && subjectID != "" {
		return "event:" + sanitizeSessionPartition(source+":"+subjectKind+":"+subjectID)
	}
	eventType := normalizeSessionKey(env.Type)
	if source != "" && eventType != "" {
		return "event:" + sanitizeSessionPartition(source+":"+eventType)
	}
	return "main"
}

func payloadSessionKey(payload json.RawMessage) string {
	if len(payload) == 0 {
		return ""
	}
	var parsed struct {
		SessionKey string `json:"session_key"`
		ThreadID   string `json:"thread_id"`
	}
	if err := json.Unmarshal(payload, &parsed); err != nil {
		return ""
	}
	if key := normalizeSessionKey(parsed.SessionKey); key != "" {
		return key
	}
	return normalizeSessionKey(parsed.ThreadID)
}

func sanitizeSessionPartition(raw string) string {
	var b strings.Builder
	b.Grow(len(raw))
	for _, r := range raw {
		switch {
		case unicode.IsLetter(r), unicode.IsNumber(r), r == '-', r == '_', r == '.', r == ':', r == '/':
			b.WriteRune(r)
		default:
			b.WriteRune('_')
		}
	}
	sanitized := strings.TrimSpace(b.String())
	sanitized = strings.Trim(sanitized, "_")
	if sanitized == "" {
		return "unknown"
	}
	return sanitized
}

type controllerRPCEventRequest struct {
	Event      serve.EventEnvelope `json:"event"`
	SessionKey string              `json:"session_key,omitempty"`
}

type controllerRPCEventResponse struct {
	Status     string `json:"status"`
	Message    string `json:"message,omitempty"`
	EventID    string `json:"event_id,omitempty"`
	TurnID     string `json:"turn_id,omitempty"`
	ThreadID   string `json:"thread_id,omitempty"`
	SessionKey string `json:"session_key,omitempty"`
	Decision   string `json:"decision,omitempty"`
	Action     string `json:"action,omitempty"`
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

func waitForControllerRPCReady(ctx context.Context, client *http.Client, containerID string) error {
	if ctx == nil {
		ctx = context.Background()
	}
	timeout := resolveControllerRPCReadyTimeout()
	// Controller bootstrap can take longer than 30s with real model latency.
	// Keep readiness polling tolerant to avoid tearing down a just-started
	// controller session before the RPC server is actually ready.
	started := time.Now()
	deadline := started.Add(timeout)
	nextProgressLog := started.Add(10 * time.Second)
	for {
		if time.Now().After(deadline) {
			return fmt.Errorf("timed out waiting for controller rpc health endpoint after %s", timeout)
		}
		reqCtx := ctx
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
		} else if containerID != "" {
			if dockerErr := controllerHealthViaDockerExec(ctx, containerID); dockerErr == nil {
				return nil
			}
		}
		if time.Now().After(nextProgressLog) {
			holonlog.Info("waiting for controller rpc health endpoint", "elapsed", time.Since(started).Round(time.Second).String(), "timeout", timeout.String())
			nextProgressLog = time.Now().Add(10 * time.Second)
		}
		select {
		case <-time.After(300 * time.Millisecond):
		case <-ctx.Done():
			return ctx.Err()
		}
	}
}

func (h *cliControllerHandler) postEventWithReconnect(ctx context.Context, ref string, sessionKey string, env serve.EventEnvelope) (controllerRPCEventResponse, error) {
	resp, err := h.postEventRPC(ctx, ref, sessionKey, env)
	if err == nil {
		return resp, nil
	}
	if !isRetryableControllerRPCError(ctx, err) {
		return controllerRPCEventResponse{}, err
	}

	holonlog.Warn("controller rpc dispatch failed, restarting session", "error", err)

	h.mu.Lock()
	h.logControllerDoneIfAvailableLocked("before reconnect")
	if h.controllerSession != nil && h.sessionRunner != nil {
		stopCtx, stopCancel := context.WithTimeout(context.Background(), 5*time.Second)
		_ = h.sessionRunner.Stop(stopCtx, h.controllerSession)
		stopCancel()
	}
	h.controllerSession = nil
	h.controllerDone = nil
	h.controllerHTTPClient = nil
	h.controllerSocketPath = ""
	restartCtx := ctx
	if restartCtx == nil {
		restartCtx = context.Background()
	}
	restartErr := h.ensureControllerLocked(restartCtx, ref)
	client := h.controllerHTTPClient
	h.mu.Unlock()
	if restartErr != nil {
		return controllerRPCEventResponse{}, restartErr
	}
	return postEventRPC(restartCtx, client, sessionKey, env)
}

func (h *cliControllerHandler) waitForControllerEventResult(
	ctx context.Context,
	ref,
	sessionKey,
	eventID string,
	onProgress func(controllerRPCEventResponse, time.Duration),
) (controllerRPCEventResponse, error) {
	if strings.TrimSpace(eventID) == "" {
		return controllerRPCEventResponse{}, fmt.Errorf("event_id is required to wait for controller event result")
	}
	delay := 300 * time.Millisecond
	const maxDelay = 5 * time.Second
	lastStatus := ""
	startedAt := time.Now()
	lastProgressEmit := time.Time{}
	progressHeartbeat := resolveTurnProgressHeartbeat()

	for {
		resp, err := h.getEventStatusRPC(ctx, ref, sessionKey, eventID)
		if err != nil {
			return controllerRPCEventResponse{}, err
		}
		currentStatus := strings.ToLower(strings.TrimSpace(resp.Status))
		if currentStatus == "" {
			currentStatus = "unknown"
		}
		statusChanged := currentStatus != lastStatus
		if statusChanged {
			holonlog.Info(
				"controller event status",
				"event_id", eventID,
				"session_key", sessionKey,
				"status", currentStatus,
			)
			lastStatus = currentStatus
		}
		if onProgress != nil && !isControllerEventTerminalStatus(resp.Status) {
			emitProgress := false
			if lastProgressEmit.IsZero() || statusChanged {
				emitProgress = true
			} else if progressHeartbeat > 0 && time.Since(lastProgressEmit) >= progressHeartbeat {
				emitProgress = true
			}
			if emitProgress {
				onProgress(resp, time.Since(startedAt))
				lastProgressEmit = time.Now()
			}
		}
		if isControllerEventTerminalStatus(resp.Status) {
			if strings.EqualFold(strings.TrimSpace(resp.Status), "failed") {
				return controllerRPCEventResponse{}, fmt.Errorf("controller event execution failed: %s", strings.TrimSpace(resp.Message))
			}
			return resp, nil
		}
		if currentStatus != "running" {
			if delay < maxDelay {
				delay = delay + delay/2
				if delay > maxDelay {
					delay = maxDelay
				}
			}
		}
		timer := time.NewTimer(delay)
		select {
		case <-ctx.Done():
			timer.Stop()
			return controllerRPCEventResponse{}, ctx.Err()
		case <-timer.C:
		}
	}
}

func resolveTurnProgressHeartbeat() time.Duration {
	raw := strings.TrimSpace(os.Getenv("HOLON_SERVE_TURN_PROGRESS_HEARTBEAT"))
	if raw == "" {
		return defaultTurnProgressHeartbeat
	}
	interval, err := time.ParseDuration(raw)
	if err != nil || interval <= 0 {
		holonlog.Warn("invalid HOLON_SERVE_TURN_PROGRESS_HEARTBEAT; using default", "value", raw, "default", defaultTurnProgressHeartbeat.String())
		return defaultTurnProgressHeartbeat
	}
	return interval
}

func normalizeControllerProgressState(status string) string {
	normalized := strings.ToLower(strings.TrimSpace(status))
	switch normalized {
	case "accepted", "queued":
		return "queued"
	case "running":
		return "running"
	case "cancel_requested":
		return "cancel_requested"
	default:
		return "waiting"
	}
}

func buildTurnProgressMessage(status, rawMessage string, env serve.EventEnvelope, elapsed time.Duration) string {
	status = normalizeControllerProgressState(status)
	msg := strings.TrimSpace(rawMessage)
	if msg != "" && !isGenericTurnProgressMessage(msg, status) {
		return msg
	}

	contextLabel := progressEventContextLabel(env)
	switch status {
	case "queued":
		if contextLabel == "" {
			return "Queued: waiting for available execution slot"
		}
		return fmt.Sprintf("Queued: waiting to process %s", contextLabel)
	case "cancel_requested":
		return "Cancel requested: waiting for controller to stop current work"
	case "waiting":
		if contextLabel == "" {
			return "Waiting for controller event status update"
		}
		return fmt.Sprintf("Waiting for %s status update", contextLabel)
	default:
		phase := runningProgressPhase(env, elapsed)
		if contextLabel == "" {
			return phase
		}
		return fmt.Sprintf("%s: %s", phase, contextLabel)
	}
}

func isGenericTurnProgressMessage(message, normalizedStatus string) bool {
	lower := strings.ToLower(strings.TrimSpace(message))
	if lower == "" {
		return true
	}
	if lower == fmt.Sprintf("controller event status: %s", normalizedStatus) {
		return true
	}
	return lower == "running" || lower == "queued" || lower == "waiting"
}

func runningProgressPhase(env serve.EventEnvelope, elapsed time.Duration) string {
	if strings.EqualFold(strings.TrimSpace(env.Source), "rpc") && strings.EqualFold(strings.TrimSpace(env.Type), "rpc.turn.input") {
		switch {
		case elapsed < 6*time.Second:
			return "Understanding request and scanning relevant code"
		case elapsed < 20*time.Second:
			return "Inspecting files and planning code changes"
		case elapsed < 60*time.Second:
			return "Implementing changes and running checks"
		default:
			return "Verifying results and preparing response"
		}
	}

	switch {
	case elapsed < 5*time.Second:
		return "Analyzing event context"
	case elapsed < 20*time.Second:
		return "Planning next operation"
	case elapsed < 60*time.Second:
		return "Executing selected operation"
	default:
		return "Continuing execution"
	}
}

func progressEventContextLabel(env serve.EventEnvelope) string {
	source := strings.TrimSpace(env.Source)
	eventType := strings.TrimSpace(env.Type)
	subjectKind := strings.TrimSpace(env.Subject.Kind)
	subjectID := strings.TrimSpace(env.Subject.ID)

	switch {
	case strings.EqualFold(source, "rpc") && strings.EqualFold(eventType, "rpc.turn.input"):
		return "interactive user request"
	case strings.EqualFold(source, "github"):
		if subjectKind != "" && subjectID != "" {
			return fmt.Sprintf("GitHub %s #%s", subjectKind, subjectID)
		}
		if eventType != "" {
			return "GitHub event " + eventType
		}
		return "GitHub event"
	case strings.EqualFold(source, "timer") || strings.EqualFold(eventType, "timer.tick"):
		return "scheduled timer tick"
	default:
		if eventType != "" {
			return "event " + eventType
		}
		if source != "" {
			return source + " event"
		}
		return ""
	}
}

func (h *cliControllerHandler) getEventStatusRPC(ctx context.Context, ref, sessionKey, eventID string) (controllerRPCEventResponse, error) {
	h.mu.Lock()
	if err := h.ensureControllerLocked(ctx, ref); err != nil {
		h.mu.Unlock()
		return controllerRPCEventResponse{}, err
	}
	client := h.controllerHTTPClient
	containerID := ""
	if h.controllerSession != nil {
		containerID = strings.TrimSpace(h.controllerSession.ContainerID)
	}
	h.mu.Unlock()

	resp, err := getEventRPC(ctx, client, eventID)
	if err == nil || containerID == "" {
		return resp, err
	}

	dockerResp, dockerErr := getEventRPCViaDockerExec(ctx, containerID, eventID)
	if dockerErr == nil {
		id := containerID
		if len(id) > 12 {
			id = id[:12]
		}
		holonlog.Debug("controller status fallback succeeded via docker exec", "container_id", id, "event_id", eventID)
		return dockerResp, nil
	}
	return controllerRPCEventResponse{}, fmt.Errorf("controller status request failed and docker exec fallback failed: rpc_error=%v; fallback_error=%w", err, dockerErr)
}

func (h *cliControllerHandler) cancelEventWithReconnect(ctx context.Context, ref, eventID, reason string) (controllerRPCEventResponse, error) {
	h.mu.Lock()
	if err := h.ensureControllerLocked(ctx, ref); err != nil {
		h.mu.Unlock()
		return controllerRPCEventResponse{}, err
	}
	client := h.controllerHTTPClient
	containerID := ""
	if h.controllerSession != nil {
		containerID = strings.TrimSpace(h.controllerSession.ContainerID)
	}
	h.mu.Unlock()

	resp, err := cancelEventRPC(ctx, client, eventID, reason)
	if err == nil || containerID == "" {
		return resp, err
	}

	dockerResp, dockerErr := cancelEventRPCViaDockerExec(ctx, containerID, eventID, reason)
	if dockerErr == nil {
		id := containerID
		if len(id) > 12 {
			id = id[:12]
		}
		holonlog.Debug("controller cancel fallback succeeded via docker exec", "container_id", id, "event_id", eventID)
		return dockerResp, nil
	}
	return controllerRPCEventResponse{}, fmt.Errorf("controller cancel request failed and docker exec fallback failed: rpc_error=%v; fallback_error=%w", err, dockerErr)
}

func (h *cliControllerHandler) logControllerDoneIfAvailableLocked(reason string) {
	if h.controllerDone == nil {
		return
	}
	select {
	case waitErr := <-h.controllerDone:
		if waitErr == nil {
			return
		}
		containerID := ""
		if h.controllerSession != nil {
			containerID = h.controllerSession.ContainerID
		}
		holonlog.Warn(
			"controller runtime wait completed",
			"reason", reason,
			"container_id", containerID,
			"error", waitErr,
		)
	default:
	}
}

func (h *cliControllerHandler) postEventRPC(ctx context.Context, ref string, sessionKey string, env serve.EventEnvelope) (controllerRPCEventResponse, error) {
	h.mu.Lock()
	if err := h.ensureControllerLocked(ctx, ref); err != nil {
		h.mu.Unlock()
		return controllerRPCEventResponse{}, err
	}
	client := h.controllerHTTPClient
	containerID := ""
	if h.controllerSession != nil {
		containerID = strings.TrimSpace(h.controllerSession.ContainerID)
	}
	h.mu.Unlock()
	resp, err := postEventRPC(ctx, client, sessionKey, env)
	if err == nil || containerID == "" {
		return resp, err
	}
	dockerResp, dockerErr := postEventRPCViaDockerExec(ctx, containerID, sessionKey, env)
	if dockerErr == nil {
		id := containerID
		if len(id) > 12 {
			id = id[:12]
		}
		holonlog.Debug("controller rpc fallback succeeded via docker exec", "container_id", id)
		return dockerResp, nil
	}
	return controllerRPCEventResponse{}, fmt.Errorf("controller rpc request failed and docker exec fallback failed: rpc_error=%v; fallback_error=%w", err, dockerErr)
}

func postEventRPC(ctx context.Context, client *http.Client, sessionKey string, env serve.EventEnvelope) (controllerRPCEventResponse, error) {
	payload := controllerRPCEventRequest{
		Event:      env,
		SessionKey: normalizeSessionKey(sessionKey),
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to marshal controller rpc request: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, "http://unix/v1/runtime/events", bytes.NewReader(body))
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to create controller rpc request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("controller rpc request failed: %w", err)
	}
	defer resp.Body.Close()
	result, err := parseControllerRPCResponse(resp.Body)
	if err != nil {
		return controllerRPCEventResponse{}, err
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusAccepted {
		return controllerRPCEventResponse{}, fmt.Errorf("controller rpc status %d: %s", resp.StatusCode, strings.TrimSpace(result.Message))
	}
	if strings.EqualFold(strings.TrimSpace(result.Status), "failed") {
		return controllerRPCEventResponse{}, fmt.Errorf("controller event execution failed: %s", strings.TrimSpace(result.Message))
	}
	return result, nil
}

func getEventRPC(ctx context.Context, client *http.Client, eventID string) (controllerRPCEventResponse, error) {
	path := "http://unix/v1/runtime/events/" + url.PathEscape(strings.TrimSpace(eventID))
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, path, nil)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to create controller status request: %w", err)
	}
	resp, err := client.Do(req)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("controller status request failed: %w", err)
	}
	defer resp.Body.Close()
	result, err := parseControllerRPCResponse(resp.Body)
	if err != nil {
		return controllerRPCEventResponse{}, err
	}
	if resp.StatusCode == http.StatusNotFound {
		return controllerRPCEventResponse{}, fmt.Errorf("controller event %q not found", eventID)
	}
	if resp.StatusCode != http.StatusOK {
		return controllerRPCEventResponse{}, fmt.Errorf("controller status %d: %s", resp.StatusCode, strings.TrimSpace(result.Message))
	}
	return result, nil
}

func cancelEventRPC(ctx context.Context, client *http.Client, eventID, reason string) (controllerRPCEventResponse, error) {
	path := "http://unix/v1/runtime/events/" + url.PathEscape(strings.TrimSpace(eventID))
	reason = strings.TrimSpace(reason)
	if reason != "" {
		path = path + "?reason=" + url.QueryEscape(reason)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, path, nil)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to create controller cancel request: %w", err)
	}
	resp, err := client.Do(req)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("controller cancel request failed: %w", err)
	}
	defer resp.Body.Close()
	result, err := parseControllerRPCResponse(resp.Body)
	if err != nil {
		return controllerRPCEventResponse{}, err
	}
	if resp.StatusCode == http.StatusNotFound {
		return controllerRPCEventResponse{}, fmt.Errorf("controller event %q not found", eventID)
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusAccepted {
		return controllerRPCEventResponse{}, fmt.Errorf("controller cancel status %d: %s", resp.StatusCode, strings.TrimSpace(result.Message))
	}
	return result, nil
}

func parseControllerRPCResponse(body io.Reader) (controllerRPCEventResponse, error) {
	var raw struct {
		Status     string `json:"status"`
		Message    string `json:"message,omitempty"`
		Error      string `json:"error,omitempty"`
		EventID    string `json:"event_id,omitempty"`
		TurnID     string `json:"turn_id,omitempty"`
		ThreadID   string `json:"thread_id,omitempty"`
		SessionKey string `json:"session_key,omitempty"`
		Decision   string `json:"decision,omitempty"`
		Action     string `json:"action,omitempty"`
	}
	respBody, err := io.ReadAll(body)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to read controller rpc response: %w", err)
	}
	if err := json.Unmarshal(respBody, &raw); err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to parse controller rpc response: %w", err)
	}
	message := strings.TrimSpace(raw.Message)
	if message == "" {
		message = strings.TrimSpace(raw.Error)
	}
	result := controllerRPCEventResponse{
		Status:     strings.TrimSpace(raw.Status),
		Message:    message,
		EventID:    strings.TrimSpace(raw.EventID),
		TurnID:     strings.TrimSpace(raw.TurnID),
		ThreadID:   strings.TrimSpace(raw.ThreadID),
		SessionKey: strings.TrimSpace(raw.SessionKey),
		Decision:   strings.TrimSpace(raw.Decision),
		Action:     strings.TrimSpace(raw.Action),
	}
	return result, nil
}

func deriveHasAction(result controllerRPCEventResponse) (bool, []string) {
	refs := make([]string, 0, 2)
	addRef := func(ref string) {
		ref = strings.TrimSpace(ref)
		if ref == "" {
			return
		}
		for _, existing := range refs {
			if existing == ref {
				return
			}
		}
		refs = append(refs, ref)
	}

	if action := normalizeAnnounceAction(result.Action); action != "" && action != "none_required" {
		addRef("action:" + action)
	}

	lower := strings.ToLower(strings.TrimSpace(result.Message))
	for _, marker := range []struct {
		key      string
		refValue string
	}{
		{key: "opened pr", refValue: "action:opened_pr"},
		{key: "created pull request", refValue: "action:opened_pr"},
		{key: "posted review", refValue: "action:posted_review"},
		{key: "updated branch", refValue: "action:updated_branch"},
		{key: "pushed commit", refValue: "action:updated_branch"},
		{key: "posted comment", refValue: "action:commented"},
		{key: "commented on", refValue: "action:commented"},
	} {
		if strings.Contains(lower, marker.key) {
			addRef(marker.refValue)
		}
	}
	return len(refs) > 0, refs
}

func normalizeOutcomeStatus(status string, err error) string {
	if err != nil {
		if serve.IsSkipEventError(err) {
			return "skipped"
		}
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			return "interrupted"
		}
		return "failed"
	}
	normalized := strings.ToLower(strings.TrimSpace(status))
	switch normalized {
	case "ok", "completed", "success":
		return "ok"
	case "failed", "error":
		return "failed"
	case "interrupted", "cancelled", "canceled":
		return "interrupted"
	case "skipped":
		return "skipped"
	default:
		if normalized == "" {
			return "ok"
		}
		return normalized
	}
}

func (h *cliControllerHandler) recordEventOutcome(item controllerEvent, result controllerRPCEventResponse, dispatchErr error) (eventOutcomeRecord, error) {
	status := normalizeOutcomeStatus(result.Status, dispatchErr)
	hasAction, actionRefs := deriveHasAction(result)
	message := strings.TrimSpace(result.Message)
	if dispatchErr != nil {
		message = strings.TrimSpace(dispatchErr.Error())
	}
	record := eventOutcomeRecord{
		EventID:     firstNonEmpty(strings.TrimSpace(result.EventID), strings.TrimSpace(item.env.ID)),
		Status:      status,
		HasAction:   hasAction,
		ActionRefs:  actionRefs,
		Message:     message,
		CompletedAt: time.Now().UTC().Format(time.RFC3339Nano),
	}
	data, err := json.Marshal(record)
	if err != nil {
		return record, fmt.Errorf("failed to marshal event outcome record: %w", err)
	}
	path := strings.TrimSpace(h.activityLedgerPath)
	if path == "" {
		path = filepath.Join(h.stateDir, "controller-state", "activity-ledger.ndjson")
	}
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return record, fmt.Errorf("failed to create activity ledger dir: %w", err)
	}
	h.activityLedgerMu.Lock()
	defer h.activityLedgerMu.Unlock()
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return record, fmt.Errorf("failed to open activity ledger: %w", err)
	}
	defer f.Close()
	if _, err := f.Write(append(data, '\n')); err != nil {
		return record, fmt.Errorf("failed to append activity ledger: %w", err)
	}
	return record, nil
}

func shouldEmitActivity(outcome eventOutcomeRecord) (bool, string) {
	switch strings.ToLower(strings.TrimSpace(outcome.Status)) {
	case "failed", "interrupted":
		return true, "terminal_status"
	}
	if outcome.HasAction {
		return true, "has_action"
	}
	return false, "no_action"
}

func (h *cliControllerHandler) emitActivityForEvent(item controllerEvent, outcome eventOutcomeRecord, gatingReason string) error {
	h.mu.Lock()
	emitter := h.activityEmitter
	h.mu.Unlock()
	if emitter == nil {
		return fmt.Errorf("activity emitter is not configured")
	}
	content := map[string]interface{}{
		"type":          "event_activity",
		"event_id":      outcome.EventID,
		"source":        strings.TrimSpace(item.env.Source),
		"event_type":    strings.TrimSpace(item.env.Type),
		"status":        outcome.Status,
		"has_action":    outcome.HasAction,
		"message":       strings.TrimSpace(outcome.Message),
		"completed_at":  outcome.CompletedAt,
		"gating_reason": strings.TrimSpace(gatingReason),
	}
	if len(outcome.ActionRefs) > 0 {
		content["action_refs"] = append([]string(nil), outcome.ActionRefs...)
	}
	itemNotif := serve.NewItemNotification(
		fmt.Sprintf("activity_%d", time.Now().UTC().UnixNano()),
		serve.ItemNotificationCreated,
		serve.StateCompleted,
		content,
	)
	itemNotif.ThreadID = "main"
	emitter(itemNotif)
	holonlog.Info(
		"activity_emitted",
		"event_id", outcome.EventID,
		"status", outcome.Status,
		"has_action", outcome.HasAction,
		"gating_reason", gatingReason,
	)
	return nil
}

func deriveAnnounceOutcome(summary string) (decision string, action string) {
	normalizedSummary := strings.TrimSpace(summary)
	lowerSummary := strings.ToLower(normalizedSummary)

	decision = normalizeAnnounceDecision(extractSummaryFieldValue(normalizedSummary, "decision"))
	action = normalizeAnnounceAction(extractSummaryFieldValue(normalizedSummary, "action taken"))

	if decision == "" {
		switch {
		case strings.Contains(lowerSummary, "no-op"),
			strings.Contains(lowerSummary, "no op"),
			strings.Contains(lowerSummary, "none required"),
			strings.Contains(lowerSummary, "no action required"):
			decision = "no-op"
		case strings.Contains(lowerSummary, "pr-fix"),
			strings.Contains(lowerSummary, "pr fix"),
			strings.Contains(lowerSummary, "pull request fix"),
			strings.Contains(lowerSummary, "github-pr-fix"):
			decision = "pr-fix"
		case strings.Contains(lowerSummary, "pr-review"),
			strings.Contains(lowerSummary, "pr review"),
			strings.Contains(lowerSummary, "pull request review"),
			strings.Contains(lowerSummary, "github-review"),
			strings.Contains(lowerSummary, "posted review"):
			decision = "pr-review"
		case strings.Contains(lowerSummary, "issue-solve"),
			strings.Contains(lowerSummary, "issue solve"),
			strings.Contains(lowerSummary, "github-issue-solve"),
			strings.Contains(lowerSummary, "opened pr"),
			strings.Contains(lowerSummary, "created pull request"):
			decision = "issue-solve"
		default:
			decision = "unknown"
		}
	}

	if action == "" {
		switch {
		case strings.Contains(lowerSummary, "opened pr"),
			strings.Contains(lowerSummary, "created pull request"):
			action = "opened_pr"
		case strings.Contains(lowerSummary, "posted review"):
			action = "posted_review"
		case strings.Contains(lowerSummary, "updated branch"),
			strings.Contains(lowerSummary, "pushed commit"):
			action = "updated_branch"
		case strings.Contains(lowerSummary, "commented"),
			strings.Contains(lowerSummary, "posted comment"):
			action = "commented"
		case decision == "no-op":
			action = "none_required"
		}
	}

	return decision, action
}

func extractSummaryFieldValue(summary, label string) string {
	if strings.TrimSpace(summary) == "" || strings.TrimSpace(label) == "" {
		return ""
	}
	labelLower := strings.ToLower(strings.TrimSpace(label)) + ":"
	for _, line := range strings.Split(summary, "\n") {
		trimmed := strings.TrimSpace(line)
		lower := strings.ToLower(trimmed)
		if !strings.HasPrefix(lower, labelLower) {
			continue
		}
		value := strings.TrimSpace(trimmed[len(labelLower):])
		return strings.Trim(value, " -*`")
	}
	return ""
}

func normalizeAnnounceDecision(decision string) string {
	normalized := strings.ToLower(strings.TrimSpace(decision))
	normalized = strings.ReplaceAll(normalized, "_", "-")
	switch normalized {
	case "issue-solve", "issuesolve":
		return "issue-solve"
	case "pr-review", "prreview":
		return "pr-review"
	case "pr-fix", "prfix":
		return "pr-fix"
	case "no-op", "noop", "none", "no action":
		return "no-op"
	case "", "unknown":
		return ""
	default:
		return normalized
	}
}

func normalizeAnnounceAction(action string) string {
	normalized := strings.ToLower(strings.TrimSpace(action))
	if normalized == "" {
		return ""
	}
	switch normalized {
	case "opened_pr", "opened-pr", "opened pr", "created pull request":
		return "opened_pr"
	case "posted_review", "posted-review", "posted review":
		return "posted_review"
	case "updated_branch", "updated-branch", "updated branch", "pushed commit":
		return "updated_branch"
	case "commented", "posted comment":
		return "commented"
	case "none_required", "none-required", "none required", "none", "no action required":
		return "none_required"
	}

	var b strings.Builder
	prevUnderscore := false
	for _, r := range normalized {
		switch {
		case (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9'):
			b.WriteRune(r)
			prevUnderscore = false
		default:
			if prevUnderscore || b.Len() == 0 {
				continue
			}
			b.WriteByte('_')
			prevUnderscore = true
		}
	}
	result := strings.Trim(b.String(), "_")
	return result
}

func controllerHealthViaDockerExec(ctx context.Context, containerID string) error {
	if !isSafeDockerContainerID(containerID) {
		return fmt.Errorf("docker exec health failed: invalid container id")
	}
	script := fmt.Sprintf(`const http=require("http");
const req=http.request({socketPath:%q,path:"/health",method:"GET"},(res)=>{res.resume(); if(res.statusCode===200){process.exit(0);} process.exit(2);});
req.on("error",(err)=>{console.error(String(err)); process.exit(1);});
req.end();`, controllerRPCSocketPathInContainer)
	cmd := exec.CommandContext(ctx, "docker", "exec", containerID, "node", "-e", script)
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		stderrStr := strings.TrimSpace(stderr.String())
		if errors.Is(err, exec.ErrNotFound) {
			if stderrStr != "" {
				return fmt.Errorf("docker exec health failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w: %s", err, stderrStr)
			}
			return fmt.Errorf("docker exec health failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w", err)
		}
		if stderrStr != "" {
			return fmt.Errorf("docker exec health failed: %w: %s", err, stderrStr)
		}
		return fmt.Errorf("docker exec health failed: %w", err)
	}
	return nil
}

func postEventRPCViaDockerExec(ctx context.Context, containerID, sessionKey string, env serve.EventEnvelope) (controllerRPCEventResponse, error) {
	if !isSafeDockerContainerID(containerID) {
		return controllerRPCEventResponse{}, fmt.Errorf("docker exec rpc failed: invalid container id")
	}
	payload := controllerRPCEventRequest{
		Event:      env,
		SessionKey: normalizeSessionKey(sessionKey),
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to marshal controller rpc request: %w", err)
	}
	script := fmt.Sprintf(`const http=require("http");
let raw="";
process.stdin.setEncoding("utf8");
process.stdin.on("data",(c)=>raw+=c);
process.stdin.on("end",()=>{
  const req=http.request({socketPath:%q,path:"/v1/runtime/events",method:"POST",headers:{"Content-Type":"application/json"}},(res)=>{
    let out=""; res.setEncoding("utf8");
    res.on("data",(c)=>out+=c);
    res.on("end",()=>{ process.stdout.write(out); process.exit((res.statusCode===200||res.statusCode===202)?0:3); });
  });
  req.on("error",(err)=>{console.error(String(err)); process.exit(2);});
  req.write(raw); req.end();
});`, controllerRPCSocketPathInContainer)
	cmd := exec.CommandContext(ctx, "docker", "exec", "-i", containerID, "node", "-e", script)
	cmd.Stdin = bytes.NewReader(body)
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		stderrStr := strings.TrimSpace(stderr.String())
		if errors.Is(err, exec.ErrNotFound) {
			if stderrStr != "" {
				return controllerRPCEventResponse{}, fmt.Errorf("docker exec rpc failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w: %s", err, stderrStr)
			}
			return controllerRPCEventResponse{}, fmt.Errorf("docker exec rpc failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w", err)
		}
		if stderrStr != "" {
			return controllerRPCEventResponse{}, fmt.Errorf("docker exec rpc failed: %w: %s", err, stderrStr)
		}
		return controllerRPCEventResponse{}, fmt.Errorf("docker exec rpc failed: %w", err)
	}
	respBody := bytes.TrimSpace(stdout.Bytes())
	result, err := parseControllerRPCResponse(bytes.NewReader(respBody))
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to parse docker exec rpc response: %w (body=%q)", err, string(respBody))
	}
	if strings.EqualFold(strings.TrimSpace(result.Status), "failed") {
		return controllerRPCEventResponse{}, fmt.Errorf("controller event execution failed: %s", strings.TrimSpace(result.Message))
	}
	return result, nil
}

func getEventRPCViaDockerExec(ctx context.Context, containerID, eventID string) (controllerRPCEventResponse, error) {
	if !isSafeDockerContainerID(containerID) {
		return controllerRPCEventResponse{}, fmt.Errorf("docker exec status failed: invalid container id")
	}
	escapedEventID := url.PathEscape(strings.TrimSpace(eventID))
	script := fmt.Sprintf(`const http=require("http");
const req=http.request({socketPath:%q,path:"/v1/runtime/events/%s",method:"GET"},(res)=>{
  let out=""; res.setEncoding("utf8");
  res.on("data",(c)=>out+=c);
  res.on("end",()=>{ process.stdout.write(out); process.exit(res.statusCode===200?0:3); });
});
req.on("error",(err)=>{console.error(String(err)); process.exit(2);});
req.end();`, controllerRPCSocketPathInContainer, escapedEventID)
	cmd := exec.CommandContext(ctx, "docker", "exec", containerID, "node", "-e", script)
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		stderrStr := strings.TrimSpace(stderr.String())
		if errors.Is(err, exec.ErrNotFound) {
			if stderrStr != "" {
				return controllerRPCEventResponse{}, fmt.Errorf("docker exec status failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w: %s", err, stderrStr)
			}
			return controllerRPCEventResponse{}, fmt.Errorf("docker exec status failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w", err)
		}
		if stderrStr != "" {
			return controllerRPCEventResponse{}, fmt.Errorf("docker exec status failed: %w: %s", err, stderrStr)
		}
		return controllerRPCEventResponse{}, fmt.Errorf("docker exec status failed: %w", err)
	}
	respBody := bytes.TrimSpace(stdout.Bytes())
	result, err := parseControllerRPCResponse(bytes.NewReader(respBody))
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to parse docker exec status response: %w (body=%q)", err, string(respBody))
	}
	return result, nil
}

func cancelEventRPCViaDockerExec(ctx context.Context, containerID, eventID, reason string) (controllerRPCEventResponse, error) {
	if !isSafeDockerContainerID(containerID) {
		return controllerRPCEventResponse{}, fmt.Errorf("docker exec cancel failed: invalid container id")
	}
	escapedEventID := url.PathEscape(strings.TrimSpace(eventID))
	path := fmt.Sprintf("/v1/runtime/events/%s", escapedEventID)
	reason = strings.TrimSpace(reason)
	if reason != "" {
		path = path + "?reason=" + url.QueryEscape(reason)
	}
	script := fmt.Sprintf(`const http=require("http");
const req=http.request({socketPath:%q,path:%q,method:"DELETE"},(res)=>{
  let out=""; res.setEncoding("utf8");
  res.on("data",(c)=>out+=c);
  res.on("end",()=>{ process.stdout.write(out); process.exit((res.statusCode===200||res.statusCode===202)?0:3); });
});
req.on("error",(err)=>{console.error(String(err)); process.exit(2);});
req.end();`, controllerRPCSocketPathInContainer, path)
	cmd := exec.CommandContext(ctx, "docker", "exec", containerID, "node", "-e", script)
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		stderrStr := strings.TrimSpace(stderr.String())
		if errors.Is(err, exec.ErrNotFound) {
			if stderrStr != "" {
				return controllerRPCEventResponse{}, fmt.Errorf("docker exec cancel failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w: %s", err, stderrStr)
			}
			return controllerRPCEventResponse{}, fmt.Errorf("docker exec cancel failed: docker CLI not found in PATH; ensure Docker is installed and 'docker' is available: %w", err)
		}
		if stderrStr != "" {
			return controllerRPCEventResponse{}, fmt.Errorf("docker exec cancel failed: %w: %s", err, stderrStr)
		}
		return controllerRPCEventResponse{}, fmt.Errorf("docker exec cancel failed: %w", err)
	}
	respBody := bytes.TrimSpace(stdout.Bytes())
	result, err := parseControllerRPCResponse(bytes.NewReader(respBody))
	if err != nil {
		return controllerRPCEventResponse{}, fmt.Errorf("failed to parse docker exec cancel response: %w (body=%q)", err, string(respBody))
	}
	return result, nil
}

func removeStaleControllerSocket(socketPath string) error {
	if strings.TrimSpace(socketPath) == "" {
		return nil
	}
	if info, err := os.Stat(socketPath); err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("failed to stat controller socket %s: %w", socketPath, err)
	} else if info.Mode()&os.ModeSocket != 0 {
		conn, err := net.DialTimeout("unix", socketPath, 200*time.Millisecond)
		if err == nil {
			_ = conn.Close()
			return nil
		}
	}
	if err := os.Remove(socketPath); err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("failed to remove stale controller socket %s: %w", socketPath, err)
	}
	return nil
}

func isSafeDockerContainerID(containerID string) bool {
	id := strings.TrimSpace(containerID)
	if len(id) < 12 || len(id) > 64 {
		return false
	}
	for _, ch := range id {
		if (ch < 'a' || ch > 'f') && (ch < '0' || ch > '9') {
			return false
		}
	}
	return true
}

func resolveControllerRPCReadyTimeout() time.Duration {
	const defaultTimeout = 2 * time.Minute
	raw := strings.TrimSpace(os.Getenv("HOLON_SERVE_RPC_READY_TIMEOUT"))
	if raw == "" {
		return defaultTimeout
	}
	timeout, err := time.ParseDuration(raw)
	if err != nil || timeout <= 0 {
		holonlog.Warn("invalid HOLON_SERVE_RPC_READY_TIMEOUT; using default", "value", raw, "default", defaultTimeout.String())
		return defaultTimeout
	}
	return timeout
}

func resolveControllerEventTimeout() time.Duration {
	raw := strings.TrimSpace(os.Getenv("HOLON_SERVE_EVENT_TIMEOUT"))
	if raw == "" {
		return defaultControllerEventTimeout
	}
	timeout, err := time.ParseDuration(raw)
	if err != nil || timeout <= 0 {
		holonlog.Warn("invalid HOLON_SERVE_EVENT_TIMEOUT; using default", "value", raw, "default", defaultControllerEventTimeout.String())
		return defaultControllerEventTimeout
	}
	return timeout
}

func isControllerEventPendingStatus(status string) bool {
	// Canonical serve lifecycle:
	// pending: accepted -> queued -> running -> cancel_requested
	// terminal: completed | failed | interrupted
	normalized := strings.ToLower(strings.TrimSpace(status))
	return normalized == "" || normalized == "accepted" || normalized == "queued" || normalized == "running" || normalized == "cancel_requested"
}

func isControllerEventTerminalStatus(status string) bool {
	normalized := strings.ToLower(strings.TrimSpace(status))
	return normalized == "completed" || normalized == "failed" || normalized == "interrupted"
}

func isRetryableControllerRPCError(requestCtx context.Context, err error) bool {
	lower := strings.ToLower(err.Error())
	if strings.Contains(lower, "no such file") ||
		strings.Contains(lower, "connection refused") ||
		strings.Contains(lower, "broken pipe") ||
		strings.Contains(lower, "connection reset") {
		return true
	}
	if requestCtx != nil && requestCtx.Err() != nil {
		return false
	}
	return strings.Contains(lower, "context canceled") ||
		strings.Contains(lower, "deadline exceeded")
}

func resolveServeSessionConcurrency() int {
	raw := strings.TrimSpace(os.Getenv("HOLON_SERVE_CONCURRENCY"))
	if raw == "" {
		return defaultServeSessionConcurrency
	}
	value, err := strconv.Atoi(raw)
	if err != nil || value <= 0 {
		holonlog.Warn("invalid HOLON_SERVE_CONCURRENCY; using default", "value", raw, "default", defaultServeSessionConcurrency)
		return defaultServeSessionConcurrency
	}
	return value
}

func resolveServeFollowupPolicy() serveFollowupPolicy {
	raw := strings.ToLower(strings.TrimSpace(os.Getenv("HOLON_SERVE_FOLLOWUP_POLICY")))
	switch serveFollowupPolicy(raw) {
	case serveFollowupPolicyFollowup, serveFollowupPolicyInterrupt, serveFollowupPolicyCollect:
		return serveFollowupPolicy(raw)
	case "":
		return serveFollowupPolicyFollowup
	default:
		holonlog.Warn("invalid HOLON_SERVE_FOLLOWUP_POLICY; using default", "value", raw, "default", string(serveFollowupPolicyFollowup))
		return serveFollowupPolicyFollowup
	}
}

func resolveServeMaxQueuedTurns() int {
	raw := strings.TrimSpace(os.Getenv("HOLON_SERVE_MAX_QUEUED_TURNS"))
	if raw == "" {
		return defaultServeMaxQueuedTurns
	}
	value, err := strconv.Atoi(raw)
	if err != nil || value <= 0 {
		holonlog.Warn("invalid HOLON_SERVE_MAX_QUEUED_TURNS; using default", "value", raw, "default", defaultServeMaxQueuedTurns)
		return defaultServeMaxQueuedTurns
	}
	return value
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
}

func (h *cliControllerHandler) loadTurnEventIndex() error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.turnEventIndex == nil {
		h.turnEventIndex = make(map[string]string)
	}
	path := strings.TrimSpace(h.turnEventIndexPath)
	if path == "" {
		return nil
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("failed to read turn event index: %w", err)
	}
	var raw map[string]string
	if err := json.Unmarshal(data, &raw); err != nil {
		return fmt.Errorf("failed to parse turn event index: %w", err)
	}
	for turnID, eventID := range raw {
		turnID = strings.TrimSpace(turnID)
		eventID = strings.TrimSpace(eventID)
		if turnID == "" || eventID == "" {
			continue
		}
		h.turnEventIndex[turnID] = eventID
	}
	return nil
}

func (h *cliControllerHandler) saveTurnEventIndexLocked() error {
	path := strings.TrimSpace(h.turnEventIndexPath)
	if path == "" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return fmt.Errorf("failed to create turn event index dir: %w", err)
	}
	data, err := json.MarshalIndent(h.turnEventIndex, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal turn event index: %w", err)
	}
	tmpPath := path + ".tmp"
	if err := os.WriteFile(tmpPath, append(data, '\n'), 0644); err != nil {
		return fmt.Errorf("failed to write turn event index temp file: %w", err)
	}
	if err := os.Rename(tmpPath, path); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to replace turn event index: %w", err)
	}
	return nil
}

func (h *cliControllerHandler) setTurnEventIndex(turnID, eventID string) error {
	turnID = strings.TrimSpace(turnID)
	eventID = strings.TrimSpace(eventID)
	if turnID == "" {
		return fmt.Errorf("turn_id is required")
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.turnEventIndex == nil {
		h.turnEventIndex = make(map[string]string)
	}
	if eventID == "" {
		delete(h.turnEventIndex, turnID)
	} else {
		h.turnEventIndex[turnID] = eventID
	}
	if err := h.saveTurnEventIndexLocked(); err != nil {
		return err
	}
	return nil
}

type serveStartupDiagnostics struct {
	AgentID                   string                   `json:"agent_id"`
	AgentHome                 string                   `json:"agent_home"`
	StateDir                  string                   `json:"state_dir"`
	Workspace                 string                   `json:"workspace"`
	ConfigSource              string                   `json:"config_source"`
	RoleSource                string                   `json:"role_source"`
	RoleInferred              string                   `json:"role_inferred"`
	ServeInput                string                   `json:"serve_input"`
	InputMode                 string                   `json:"input_mode"`
	SubscriptionEnabled       bool                     `json:"subscription_enabled"`
	TransportMode             string                   `json:"transport_mode"`
	SubscriptionReason        string                   `json:"subscription_reason,omitempty"`
	SubscribedRepos           []string                 `json:"subscribed_repos,omitempty"`
	RepoHint                  string                   `json:"repo_hint,omitempty"`
	TickInterval              string                   `json:"tick_interval"`
	WebhookPort               int                      `json:"webhook_port,omitempty"`
	RuntimeMode               string                   `json:"runtime_mode"`
	RuntimeDevAgentSource     string                   `json:"runtime_dev_agent_source,omitempty"`
	RuntimeDevAgentSourceFrom string                   `json:"runtime_dev_agent_source_origin,omitempty"`
	RuntimeMounts             []runtimeMountDiagnostic `json:"runtime_mounts,omitempty"`
	Preview                   string                   `json:"preview"`
	Warnings                  []string                 `json:"warnings,omitempty"`
}

type serveStartupDiagnosticsInput struct {
	AgentID                   string
	AgentHome                 string
	StateDir                  string
	Workspace                 string
	ConfigSource              string
	RoleSource                string
	RoleInferred              string
	ServeInput                string
	InputMode                 string
	SubscriptionEnabled       bool
	SubscriptionStatus        map[string]interface{}
	RepoHint                  string
	TickInterval              time.Duration
	WebhookPort               int
	RuntimeMode               string
	RuntimeDevAgentSource     string
	RuntimeDevAgentSourceFrom string
	RuntimeMounts             []runtimeMountDiagnostic
	NoSubscriptionsFlag       bool
}

func buildServeStartupDiagnostics(input serveStartupDiagnosticsInput) serveStartupDiagnostics {
	diag := serveStartupDiagnostics{
		AgentID:                   input.AgentID,
		AgentHome:                 input.AgentHome,
		StateDir:                  input.StateDir,
		Workspace:                 input.Workspace,
		ConfigSource:              input.ConfigSource,
		RoleSource:                input.RoleSource,
		RoleInferred:              input.RoleInferred,
		ServeInput:                firstNonEmpty(input.ServeInput, "-"),
		InputMode:                 input.InputMode,
		SubscriptionEnabled:       input.SubscriptionEnabled,
		TransportMode:             "none",
		RepoHint:                  strings.TrimSpace(input.RepoHint),
		TickInterval:              input.TickInterval.String(),
		WebhookPort:               input.WebhookPort,
		RuntimeMode:               input.RuntimeMode,
		RuntimeDevAgentSource:     input.RuntimeDevAgentSource,
		RuntimeDevAgentSourceFrom: input.RuntimeDevAgentSourceFrom,
		RuntimeMounts:             append([]runtimeMountDiagnostic(nil), input.RuntimeMounts...),
		Preview:                   "experimental",
		Warnings:                  []string{"holon serve is experimental/preview in this release; use holon run for GA workloads."},
	}

	if input.InputMode == "subscription" {
		mode := strings.TrimSpace(asString(input.SubscriptionStatus["mode"]))
		reason := strings.TrimSpace(asString(input.SubscriptionStatus["reason"]))
		transportMode := strings.TrimSpace(asString(input.SubscriptionStatus["transport_mode"]))
		if transportMode == "auto" {
			transportMode = ""
		}
		repos := asStringSlice(input.SubscriptionStatus["subscribed_repos"])
		diag.SubscribedRepos = repos
		diag.SubscriptionReason = reason
		if mode == "rpc_only" {
			diag.TransportMode = "rpc_only"
		} else {
			diag.TransportMode = firstNonEmpty(transportMode, mode, "unknown")
		}
		switch reason {
		case "empty_repos":
			diag.Warnings = append(diag.Warnings,
				"subscriptions.github.repos is empty; serve is running in rpc-only passive mode.",
				"next action: add subscriptions.github.repos in agent.yaml (or pass --repo with --tick-interval for timer-driven polling).",
			)
		case "no_subscriptions":
			diag.Warnings = append(diag.Warnings,
				"no GitHub subscriptions configured; serve is running in rpc-only passive mode.",
				"next action: add a github subscription block to agent.yaml or run with --no-subscriptions and feed events via --input.",
			)
		case "rpc_only":
			diag.Warnings = append(diag.Warnings, "serve is running in rpc-only passive mode.")
		}
		if mode == "rpc_only" && input.TickInterval <= 0 {
			diag.Warnings = append(diag.Warnings, "idle behavior: waiting for turn/start RPC or externally injected events.")
		}
	}

	if input.InputMode == "stdin_file" {
		diag.Warnings = append(diag.Warnings, "serve subscriptions are disabled; events come from --input only.")
		if diag.ServeInput == "-" {
			diag.Warnings = append(diag.Warnings, "idle behavior: waiting for newline-delimited JSON events on stdin.")
		}
	}

	if input.InputMode == "webhook_legacy" {
		diag.TransportMode = "webhook"
		diag.Warnings = append(diag.Warnings,
			"legacy webhook mode is for backward compatibility; prefer subscription mode with agent.yaml.",
		)
	}

	if input.NoSubscriptionsFlag {
		diag.Warnings = append(diag.Warnings, "--no-subscriptions is enabled; serve will not read agent.yaml subscriptions.")
	}

	return diag
}

func writeServeStartupDiagnostics(stateDir string, diag serveStartupDiagnostics) error {
	data, err := json.MarshalIndent(diag, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal serve startup diagnostics: %w", err)
	}
	data = append(data, '\n')
	path := filepath.Join(stateDir, "serve-startup-diagnostics.json")
	tmp, err := os.CreateTemp(stateDir, ".serve-startup-diagnostics-*.tmp")
	if err != nil {
		return fmt.Errorf("failed to create temp diagnostics file: %w", err)
	}
	tmpPath := tmp.Name()
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to write serve startup diagnostics to temp file: %w", err)
	}
	if err := tmp.Close(); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to close temp diagnostics file: %w", err)
	}
	if err := os.Rename(tmpPath, path); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to atomically replace serve startup diagnostics file: %w", err)
	}
	holonlog.Info("serve startup diagnostics written", "path", path)
	return nil
}

func logServeStartupDiagnostics(diag serveStartupDiagnostics) {
	holonlog.Info(
		"serve startup diagnostics",
		"agent_id", diag.AgentID,
		"agent_home", diag.AgentHome,
		"state_dir", diag.StateDir,
		"workspace", diag.Workspace,
		"config_source", diag.ConfigSource,
		"role_source", diag.RoleSource,
		"role_inferred", diag.RoleInferred,
		"input_mode", diag.InputMode,
		"subscription_enabled", diag.SubscriptionEnabled,
		"transport_mode", diag.TransportMode,
		"subscription_reason", diag.SubscriptionReason,
		"subscribed_repos", diag.SubscribedRepos,
		"repo_hint", diag.RepoHint,
		"serve_input", diag.ServeInput,
		"tick_interval", diag.TickInterval,
		"webhook_port", diag.WebhookPort,
		"runtime_mode", diag.RuntimeMode,
		"runtime_dev_agent_source", diag.RuntimeDevAgentSource,
		"runtime_dev_agent_source_origin", diag.RuntimeDevAgentSourceFrom,
		"runtime_mounts", diag.RuntimeMounts,
		"preview", diag.Preview,
	)
	for _, warning := range diag.Warnings {
		holonlog.Warn("serve startup warning", "message", warning)
	}
}

func asString(value interface{}) string {
	s, _ := value.(string)
	return s
}

func asStringSlice(value interface{}) []string {
	switch typed := value.(type) {
	case []string:
		return append([]string(nil), typed...)
	case []interface{}:
		out := make([]string, 0, len(typed))
		for _, item := range typed {
			s, ok := item.(string)
			if !ok {
				continue
			}
			out = append(out, s)
		}
		return out
	default:
		return nil
	}
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
	socketPath := strings.TrimSpace(h.controllerSocketPath)
	if h.controllerSession != nil && h.sessionRunner != nil {
		holonlog.Info("stopping controller runtime")
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := h.sessionRunner.Stop(ctx, h.controllerSession); err != nil {
			return err
		}
	}
	h.controllerSession = nil
	h.controllerDone = nil
	h.controllerHTTPClient = nil
	h.controllerSocketPath = ""
	if socketPath != "" {
		if err := removeStaleControllerSocket(socketPath); err != nil {
			holonlog.Warn("failed to clean controller socket path on close", "path", socketPath, "error", err)
		}
	}
	return nil
}

func init() {
	serveCmd.Flags().StringVar(&serveRepo, "repo", "", "Default repository in owner/repo format (optional repo hint)")
	serveCmd.Flags().StringVar(&serveInput, "input", "-", "Input source for events ('-' for stdin, or path to file)")
	serveCmd.Flags().StringVar(&serveAgentID, "agent-id", "main", "Agent ID (default: main)")
	serveCmd.Flags().StringVar(&serveAgentHome, "agent-home", "", "Agent home directory (overrides --agent-id)")
	serveCmd.Flags().StringVar(&serveSessionID, "session", "main", "Default serve session/thread id to load/create on startup")
	serveCmd.Flags().BoolVar(&serveNoDefaultSession, "no-default-session", false, "Disable default session/thread creation on startup (debugging)")
	serveCmd.Flags().BoolVar(&serveLazyController, "lazy-runtime", false, "Do not eagerly start serve runtime on startup (debugging)")
	serveCmd.Flags().DurationVar(&serveControllerWarmupTimeout, "runtime-warmup-timeout", 2*time.Minute, "Timeout for eagerly warming the serve runtime on startup (0 disables warmup)")
	serveCmd.Flags().BoolVar(&serveWarmupBestEffort, "runtime-warmup-best-effort", false, "Continue serving even if runtime warmup fails (debugging)")
	serveCmd.Flags().IntVar(&serveMaxEvents, "max-events", 0, "Stop after processing N events (0 = unlimited, not supported in webhook mode)")
	serveCmd.Flags().BoolVar(&serveDryRun, "dry-run", false, "Log forwarded events without starting the serve runtime session")
	serveCmd.Flags().DurationVar(&serveTickInterval, "tick-interval", 0, "Emit timer.tick events periodically (e.g. 5m)")
	serveCmd.Flags().StringVar(&serveLogLevel, "log-level", "progress", "Log level: debug, info, progress, minimal")
	serveCmd.Flags().StringVar(&serveRuntimeMode, "runtime-mode", "prod", "Runtime mode: prod (default), dev (mount local agent dist)")
	serveCmd.Flags().StringVar(&serveRuntimeDevAgentSource, "runtime-dev-agent-source", "", "Local agent source directory for --runtime-mode=dev (defaults: HOLON_RUNTIME_DEV_AGENT_SOURCE, HOLON_DEV_AGENT_SOURCE, ./agents/claude)")
	serveCmd.Flags().IntVar(&serveWebhookPort, "webhook-port", 0, "Override ingress webhook port for subscription mode; with --no-subscriptions, enables legacy webhook mode")
	serveCmd.Flags().BoolVar(&serveNoSubscriptions, "no-subscriptions", false, "Disable agent.yaml subscriptions and use stdin/file input instead")
	rootCmd.AddCommand(serveCmd)
}

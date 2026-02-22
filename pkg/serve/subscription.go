package serve

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync"
	"time"

	"github.com/fsnotify/fsnotify"
	"github.com/holon-run/holon/pkg/agenthome"
	holonlog "github.com/holon-run/holon/pkg/log"
)

// SubscriptionManager manages active subscriptions and their transport
type SubscriptionManager struct {
	agentHome        string
	configPath       string
	config           agenthome.Config
	webhookServer    *WebhookServer
	forwarder        forwarderRunner
	websocketSrc     *WebSocketSource
	eventService     *Service
	mu               sync.Mutex
	reconcileMu      sync.Mutex
	started          bool
	webhookPort      int
	stateDir         string
	handler          EventHandler
	dispatcher       TurnDispatcher
	defaultSessionID string
	noDefaultSession bool
	mode             string
	cancel           context.CancelFunc

	// Hot reload settings.
	reloadDebounce     time.Duration
	reloadPollInterval time.Duration
	disableHotReload   bool

	// Tracks the currently active GitHub ingress transport configuration (may differ from raw config
	// while a reload attempt is in progress / failed).
	activeTransportMode string
	activeRepos         []string
	activeWebsocketURL  string

	lastReloadAt    time.Time
	lastReloadError string

	watchReady chan struct{}

	forwarderFactory func(ForwarderConfig) (forwarderRunner, error)

	forwarderRestartCooldown    time.Duration
	lastForwarderRestartAttempt time.Time
}

// ManagerConfig holds configuration for SubscriptionManager
type ManagerConfig struct {
	AgentHome        string
	StateDir         string
	Handler          EventHandler
	WebhookPort      int // 0 means use the default port (8080)
	TurnDispatcher   TurnDispatcher
	DefaultSessionID string
	NoDefaultSession bool

	// Hot reload configuration. When unset, defaults are used.
	ReloadDebounce           time.Duration // default: 600ms
	ReloadPollInterval       time.Duration // default: 3s
	DisableHotReload         bool
	ForwarderRestartCooldown time.Duration // default: 10s

	ForwarderFactory func(ForwarderConfig) (forwarderRunner, error)
}

type forwarderRunner interface {
	Start(ctx context.Context) error
	Stop() error
	HealthCheck() error
	Status() map[string]interface{}
}

// NewSubscriptionManager creates a new subscription manager
func NewSubscriptionManager(cfg ManagerConfig) (*SubscriptionManager, error) {
	// Load agent config
	agentCfg, err := agenthome.LoadConfig(cfg.AgentHome)
	if err != nil {
		return nil, fmt.Errorf("failed to load agent config: %w", err)
	}

	debounce := cfg.ReloadDebounce
	if debounce <= 0 {
		debounce = 600 * time.Millisecond
	}
	poll := cfg.ReloadPollInterval
	if poll <= 0 {
		poll = 3 * time.Second
	}
	restartCooldown := cfg.ForwarderRestartCooldown
	if restartCooldown <= 0 {
		restartCooldown = 10 * time.Second
	}

	forwarderFactory := cfg.ForwarderFactory
	if forwarderFactory == nil {
		forwarderFactory = func(fc ForwarderConfig) (forwarderRunner, error) {
			return NewForwarder(fc)
		}
	}

	return &SubscriptionManager{
		agentHome:        cfg.AgentHome,
		configPath:       filepath.Join(cfg.AgentHome, "agent.yaml"),
		config:           agentCfg,
		stateDir:         cfg.StateDir,
		handler:          cfg.Handler,
		webhookPort:      cfg.WebhookPort,
		dispatcher:       cfg.TurnDispatcher,
		defaultSessionID: cfg.DefaultSessionID,
		noDefaultSession: cfg.NoDefaultSession,
		mode:             "rpc_only",

		reloadDebounce:     debounce,
		reloadPollInterval: poll,
		disableHotReload:   cfg.DisableHotReload,

		forwarderFactory: forwarderFactory,

		forwarderRestartCooldown: restartCooldown,
	}, nil
}

// Start starts the subscription manager and all configured subscriptions
func (sm *SubscriptionManager) Start(ctx context.Context) error {
	sm.mu.Lock()

	if sm.started {
		sm.mu.Unlock()
		return fmt.Errorf("subscription manager already started")
	}

	runCtx, cancel := context.WithCancel(ctx)
	sm.cancel = cancel

	// Find GitHub subscriptions
	var gitHubSub *agenthome.GitHubSubscription
	for _, sub := range sm.config.Subscriptions {
		if sub.GitHub != nil {
			gitHubSub = sub.GitHub
			break
		}
	}

	var startErr error
	switch {
	case gitHubSub == nil:
		startErr = sm.startRPCOnlyMode(runCtx)
	case len(gitHubSub.Repos) == 0:
		startErr = sm.startRPCOnlyMode(runCtx)
	default:
		transportMode := strings.TrimSpace(strings.ToLower(gitHubSub.Transport.Mode))
		if transportMode == "" || transportMode == "auto" {
			transportMode = "gh_forward" // Default to gh_forward for local development
		}
		switch transportMode {
		case "gh_forward":
			startErr = sm.startGHForwardMode(runCtx, gitHubSub)
		case "websocket":
			startErr = sm.startWebSocketMode(runCtx, gitHubSub)
		default:
			startErr = fmt.Errorf("unsupported transport mode: %s", transportMode)
		}
	}

	if startErr != nil {
		cancel()
		sm.cancel = nil
		sm.mu.Unlock()
		return startErr
	}

	sm.started = true
	status := sm.statusLocked()
	sm.mu.Unlock()
	if err := sm.writeStatusFile(status); err != nil {
		return err
	}

	go sm.watchForwarderHealth(runCtx)

	if !sm.disableHotReload {
		ready := make(chan struct{})
		sm.mu.Lock()
		sm.watchReady = ready
		sm.mu.Unlock()
		go sm.watchAndReloadConfig(runCtx, ready)
		select {
		case <-ready:
		case <-time.After(500 * time.Millisecond):
			holonlog.Warn("config reload watcher not ready after timeout; continuing")
		}
	} else {
		holonlog.Info("subscription config hot reload disabled")
	}
	return nil
}

func (sm *SubscriptionManager) startGHForwardMode(ctx context.Context, sub *agenthome.GitHubSubscription) error {
	port := sm.resolvePort()

	// Create webhook server
	webhookURL := BuildWebhookURL(port, "/ingress/github/webhook")
	webhookSrv, err := NewWebhookServer(WebhookConfig{
		Port:             port,
		RepoHint:         sub.Repos[0], // Use first repo as hint
		StateDir:         sm.stateDir,
		Handler:          sm.handler,
		TurnDispatcher:   sm.dispatcher,
		DefaultSessionID: sm.defaultSessionID,
		NoDefaultSession: sm.noDefaultSession,
	})
	if err != nil {
		return fmt.Errorf("failed to create webhook server: %w", err)
	}
	sm.webhookServer = webhookSrv

	if err := sm.startWebhookServer(ctx, webhookSrv); err != nil {
		webhookSrv.Close()
		return fmt.Errorf("failed to start webhook server: %w", err)
	}

	// Create gh webhook forward manager
	forwarder, err := sm.forwarderFactory(ForwarderConfig{
		Port:  port,
		Repos: sub.Repos,
		URL:   webhookURL,
	})
	if err != nil {
		webhookSrv.Close()
		return fmt.Errorf("failed to create forwarder: %w", err)
	}
	sm.forwarder = forwarder

	// Start gh webhook forward
	if err := forwarder.Start(ctx); err != nil {
		webhookSrv.Close()
		return fmt.Errorf("failed to start gh webhook forward: %w", err)
	}
	sm.mode = "gh_forward"
	sm.activeTransportMode = "gh_forward"
	sm.activeRepos = append([]string(nil), sub.Repos...)
	sm.activeWebsocketURL = ""

	holonlog.Info(
		"subscription manager started",
		"transport", "gh_forward",
		"webhook_port", port,
		"repos", sub.Repos,
	)

	return nil
}

func (sm *SubscriptionManager) startWebSocketMode(ctx context.Context, sub *agenthome.GitHubSubscription) error {
	repoHint := sub.Repos[0]
	port := sm.resolvePort()
	webhookSrv, err := NewWebhookServer(WebhookConfig{
		Port:             port,
		RepoHint:         repoHint,
		StateDir:         sm.stateDir,
		Handler:          sm.handler,
		TurnDispatcher:   sm.dispatcher,
		DefaultSessionID: sm.defaultSessionID,
		NoDefaultSession: sm.noDefaultSession,
	})
	if err != nil {
		return fmt.Errorf("failed to create webhook server: %w", err)
	}
	sm.webhookServer = webhookSrv
	if err := sm.startWebhookServer(ctx, webhookSrv); err != nil {
		webhookSrv.Close()
		return fmt.Errorf("failed to start webhook server: %w", err)
	}

	src := NewWebSocketSource(WebSocketSourceConfig{
		URL: sub.Transport.WebsocketURL,
	})

	if err := src.Start(ctx, func(_ context.Context, raw []byte) error {
		normalizedRaw := normalizeWebSocketMessage(raw)
		env, normErr := normalizeLine(normalizedRaw, repoHint, time.Now)
		if normErr != nil {
			holonlog.Warn("failed to normalize websocket event", "error", normErr)
			return nil
		}
		if injectErr := webhookSrv.InjectEvent(ctx, env); injectErr != nil {
			holonlog.Warn("failed to inject websocket event", "error", injectErr, "event_id", env.ID, "type", env.Type)
		}
		return nil
	}); err != nil {
		return fmt.Errorf("failed to start websocket source: %w", err)
	}
	sm.websocketSrc = src
	sm.mode = "websocket"
	sm.activeTransportMode = "websocket"
	sm.activeRepos = append([]string(nil), sub.Repos...)
	sm.activeWebsocketURL = sub.Transport.WebsocketURL

	holonlog.Info(
		"subscription manager started",
		"transport", "websocket",
		"url", sub.Transport.WebsocketURL,
		"repos", sub.Repos,
	)

	return nil
}

func (sm *SubscriptionManager) startRPCOnlyMode(ctx context.Context) error {
	port := sm.resolvePort()
	webhookSrv, err := NewWebhookServer(WebhookConfig{
		Port:             port,
		RepoHint:         "",
		StateDir:         sm.stateDir,
		Handler:          sm.handler,
		TurnDispatcher:   sm.dispatcher,
		DefaultSessionID: sm.defaultSessionID,
		NoDefaultSession: sm.noDefaultSession,
	})
	if err != nil {
		return fmt.Errorf("failed to create rpc server: %w", err)
	}
	sm.webhookServer = webhookSrv
	sm.mode = "rpc_only"
	sm.activeTransportMode = ""
	sm.activeRepos = nil
	sm.activeWebsocketURL = ""
	if err := sm.startWebhookServer(ctx, webhookSrv); err != nil {
		webhookSrv.Close()
		return fmt.Errorf("failed to start rpc server: %w", err)
	}
	holonlog.Info("no active subscriptions configured, starting in rpc-only mode", "port", port)
	return nil
}

func (sm *SubscriptionManager) startWebhookServer(ctx context.Context, webhookSrv *WebhookServer) error {
	errCh := make(chan error, 1)
	go func() {
		errCh <- webhookSrv.Start(ctx)
	}()

	healthURL := BuildWebhookURL(webhookSrv.Port(), "/health")
	deadline := time.NewTimer(3 * time.Second)
	defer deadline.Stop()
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case err := <-errCh:
			if err != nil && !errors.Is(err, context.Canceled) {
				return err
			}
			return fmt.Errorf("webhook server exited before becoming ready")
		case <-deadline.C:
			return fmt.Errorf("timeout waiting for webhook server readiness")
		case <-ticker.C:
			req, reqErr := http.NewRequestWithContext(ctx, http.MethodGet, healthURL, nil)
			if reqErr != nil {
				return fmt.Errorf("failed to build health request: %w", reqErr)
			}
			resp, doErr := (&http.Client{Timeout: 250 * time.Millisecond}).Do(req)
			if doErr != nil {
				continue
			}
			_ = resp.Body.Close()
			if resp.StatusCode == http.StatusOK {
				go func() {
					if err := <-errCh; err != nil && !errors.Is(err, context.Canceled) {
						holonlog.Error("webhook server exited", "error", err)
					}
				}()
				return nil
			}
		}
	}
}

func (sm *SubscriptionManager) resolvePort() int {
	if sm.webhookPort > 0 {
		return sm.webhookPort
	}
	return DefaultPort
}

func normalizeWebSocketMessage(raw []byte) []byte {
	type wsMessage struct {
		Payload json.RawMessage `json:"payload"`
		Headers map[string]any  `json:"headers"`
	}

	var msg wsMessage
	if err := json.Unmarshal(raw, &msg); err != nil || len(msg.Payload) == 0 {
		return raw
	}

	var payload map[string]any
	if err := json.Unmarshal(msg.Payload, &payload); err != nil {
		return msg.Payload
	}
	for k, v := range msg.Headers {
		normKey := normalizeHeaderKey(k)
		if _, exists := payload[normKey]; !exists {
			payload[normKey] = v
		}
	}
	merged, err := json.Marshal(payload)
	if err != nil {
		return msg.Payload
	}
	return merged
}

func normalizeHeaderKey(k string) string {
	// Match webhook normalization style: X-GitHub-Event -> x_github_event.
	return strings.ReplaceAll(strings.ToLower(strings.TrimSpace(k)), "-", "_")
}

// Stop stops the subscription manager and all active subscriptions
func (sm *SubscriptionManager) Stop() error {
	sm.reconcileMu.Lock()
	sm.mu.Lock()

	if !sm.started {
		sm.mu.Unlock()
		sm.reconcileMu.Unlock()
		return nil
	}

	var errs []error

	if sm.cancel != nil {
		sm.cancel()
		sm.cancel = nil
	}

	toStopForwarder := sm.forwarder
	toStopWS := sm.websocketSrc
	toCloseSvc := sm.eventService
	toCloseWebhook := sm.webhookServer

	sm.forwarder = nil
	sm.websocketSrc = nil
	sm.eventService = nil
	sm.webhookServer = nil

	sm.started = false
	status := sm.statusLocked()
	sm.mu.Unlock()

	// Stop components outside sm.mu, but still under reconcileMu to avoid races with reload reconcile.
	if toStopForwarder != nil {
		if err := toStopForwarder.Stop(); err != nil {
			errs = append(errs, fmt.Errorf("failed to stop forwarder: %w", err))
		}
	}
	if toStopWS != nil {
		if err := toStopWS.Stop(); err != nil {
			errs = append(errs, fmt.Errorf("failed to stop websocket source: %w", err))
		}
	}
	if toCloseSvc != nil {
		if err := toCloseSvc.Close(); err != nil {
			errs = append(errs, fmt.Errorf("failed to close websocket event service: %w", err))
		}
	}
	if toCloseWebhook != nil {
		if err := toCloseWebhook.Close(); err != nil {
			errs = append(errs, fmt.Errorf("failed to stop webhook server: %w", err))
		}
	}

	sm.reconcileMu.Unlock()
	if err := sm.writeStatusFile(status); err != nil {
		errs = append(errs, fmt.Errorf("failed to write status file: %w", err))
	}

	if len(errs) > 0 {
		return fmt.Errorf("errors stopping subscription manager: %v", errs)
	}

	holonlog.Info("subscription manager stopped")
	return nil
}

// IsRunning returns true if the subscription manager is running
func (sm *SubscriptionManager) IsRunning() bool {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	return sm.started
}

// GetWebhookPort returns the webhook port if running, 0 otherwise
func (sm *SubscriptionManager) GetWebhookPort() int {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	if sm.webhookServer != nil {
		return sm.webhookServer.Port()
	}
	return 0
}

// InjectEvent injects an event into the webhook server (for timer ticks, etc)
func (sm *SubscriptionManager) InjectEvent(ctx context.Context, env EventEnvelope) error {
	sm.mu.Lock()
	webhookServer := sm.webhookServer
	eventService := sm.eventService
	sm.mu.Unlock()

	if webhookServer != nil {
		return webhookServer.InjectEvent(ctx, env)
	}
	if eventService != nil {
		return eventService.InjectEvent(ctx, env)
	}
	return fmt.Errorf("no active subscription transport for event injection")
}

// Status returns the current status of the subscription manager
func (sm *SubscriptionManager) Status() map[string]interface{} {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	return sm.statusLocked()
}

func (sm *SubscriptionManager) statusLocked() map[string]interface{} {
	reason := sm.statusReasonLocked()
	status := map[string]interface{}{
		"running":        sm.started,
		"mode":           sm.mode,
		"rpc_active":     sm.webhookServer != nil,
		"ingress_active": sm.forwarder != nil || sm.websocketSrc != nil,
		"agent_home":     sm.agentHome,
		"config_path":    sm.configPath,
		"state_dir":      sm.stateDir,
		"updated_at":     time.Now().UTC().Format(time.RFC3339Nano),
	}
	if reason != "" {
		status["reason"] = reason
	}
	if !sm.lastReloadAt.IsZero() {
		status["last_reload_at"] = sm.lastReloadAt.UTC().Format(time.RFC3339Nano)
	}
	if strings.TrimSpace(sm.lastReloadError) != "" {
		status["last_reload_error"] = sm.lastReloadError
	}

	if sm.webhookServer != nil {
		status["webhook_port"] = sm.webhookServer.Port()
		status["webhook_url"] = BuildWebhookURL(sm.webhookServer.Port(), "/ingress/github/webhook")
	}

	if sm.forwarder != nil {
		status["forwarder"] = sm.forwarder.Status()
	}
	if sm.websocketSrc != nil {
		status["websocket"] = sm.websocketSrc.Status()
	}

	// Include subscription info
	var repos []string
	for _, sub := range sm.config.Subscriptions {
		if sub.GitHub != nil {
			repos = append(repos, sub.GitHub.Repos...)
			status["transport_mode"] = sub.GitHub.Transport.Mode
			if status["transport_mode"] == "" {
				status["transport_mode"] = "auto"
			}
		}
	}
	status["subscribed_repos"] = repos

	return status
}

func (sm *SubscriptionManager) statusReasonLocked() string {
	if !sm.started {
		return ""
	}
	if sm.mode != "rpc_only" {
		return ""
	}
	if len(sm.config.Subscriptions) == 0 {
		return "no_subscriptions"
	}
	hasGitHub := false
	for _, sub := range sm.config.Subscriptions {
		if sub.GitHub == nil {
			continue
		}
		hasGitHub = true
		if len(sub.GitHub.Repos) > 0 {
			return "rpc_only"
		}
	}
	if !hasGitHub {
		return "no_subscriptions"
	}
	return "empty_repos"
}

// WriteStatusFile writes the current status to a file in the state directory
func (sm *SubscriptionManager) WriteStatusFile() error {
	sm.mu.Lock()
	status := sm.statusLocked()
	sm.mu.Unlock()
	return sm.writeStatusFile(status)
}

func (sm *SubscriptionManager) writeStatusFile(status map[string]interface{}) error {
	if sm.stateDir == "" {
		holonlog.Info("subscription status", "status", status)
		return nil
	}
	if err := os.MkdirAll(sm.stateDir, 0755); err != nil {
		return fmt.Errorf("failed to create state dir for subscription status: %w", err)
	}

	data, err := json.MarshalIndent(status, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal subscription status: %w", err)
	}
	data = append(data, '\n')

	statusPath := filepath.Join(sm.stateDir, "subscription-status.json")
	tmp, err := os.CreateTemp(sm.stateDir, ".subscription-status-*.tmp")
	if err != nil {
		return fmt.Errorf("failed to create temp status file: %w", err)
	}
	tmpPath := tmp.Name()
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to write temp status file: %w", err)
	}
	if err := tmp.Close(); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to close temp status file: %w", err)
	}
	if err := os.Rename(tmpPath, statusPath); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("failed to replace status file: %w", err)
	}

	holonlog.Info("subscription status updated", "path", statusPath)
	return nil
}

// SubscribedRepos returns the repos declared in agent.yaml subscriptions.
func (sm *SubscriptionManager) SubscribedRepos() []string {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	var repos []string
	for _, sub := range sm.config.Subscriptions {
		if sub.GitHub == nil {
			continue
		}
		repos = append(repos, sub.GitHub.Repos...)
	}
	return repos
}

func (sm *SubscriptionManager) watchAndReloadConfig(ctx context.Context, ready chan<- struct{}) {
	// Always run polling. fsnotify is best-effort and used to reduce reload latency.
	//
	// We watch the parent directory because many editors/atomic writers replace the file via rename.
	dir := filepath.Dir(sm.configPath)
	base := filepath.Base(sm.configPath)

	var watcher *fsnotify.Watcher
	w, err := fsnotify.NewWatcher()
	if err != nil {
		holonlog.Warn("failed to create fsnotify watcher; falling back to polling only", "error", err)
	} else {
		if addErr := w.Add(dir); addErr != nil {
			holonlog.Warn("failed to watch agent home dir; falling back to polling only", "dir", dir, "error", addErr)
			_ = w.Close()
		} else {
			watcher = w
		}
	}
	if watcher != nil {
		defer watcher.Close()
	}

	var lastSig string
	if sig, sigErr := fileSignature(sm.configPath); sigErr == nil {
		lastSig = sig
	}

	pollTicker := time.NewTicker(sm.reloadPollInterval)
	defer pollTicker.Stop()

	var debounceTimer *time.Timer
	var debounceCh <-chan time.Time
	schedule := func() {
		if debounceTimer == nil {
			debounceTimer = time.NewTimer(sm.reloadDebounce)
			debounceCh = debounceTimer.C
			return
		}
		if !debounceTimer.Stop() {
			select {
			case <-debounceTimer.C:
			default:
			}
		}
		debounceTimer.Reset(sm.reloadDebounce)
		debounceCh = debounceTimer.C
	}

	if ready != nil {
		close(ready)
	}

	for {
		select {
		case <-ctx.Done():
			if debounceTimer != nil {
				debounceTimer.Stop()
			}
			return
		case <-pollTicker.C:
			sig, sigErr := fileSignature(sm.configPath)
			if sigErr != nil {
				// Keep polling; surface error via status on reload attempts only.
				continue
			}
			if sig != lastSig {
				lastSig = sig
				schedule()
			}
		case ev := <-watcherEvents(watcher):
			if ev.Name == "" {
				continue
			}
			if filepath.Dir(ev.Name) != dir || filepath.Base(ev.Name) != base {
				continue
			}
			// Any of these can indicate content changes, atomic replace, or chmod.
			if ev.Op&(fsnotify.Write|fsnotify.Create|fsnotify.Rename|fsnotify.Remove|fsnotify.Chmod) != 0 {
				schedule()
			}
		case err := <-watcherErrors(watcher):
			if err != nil {
				holonlog.Warn("config watcher error", "error", err)
			}
		case <-debounceCh:
			debounceCh = nil
			if debounceTimer != nil {
				debounceTimer.Stop()
				debounceTimer = nil
			}
			if err := sm.reloadConfig(ctx); err != nil {
				holonlog.Warn("failed to reload subscription config; keeping existing subscriptions", "error", err)
			}
		}
	}
}

func (sm *SubscriptionManager) watchForwarderHealth(ctx context.Context) {
	ticker := time.NewTicker(sm.reloadPollInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			sm.ensureForwarderHealthy(ctx)
		}
	}
}

func (sm *SubscriptionManager) ensureForwarderHealthy(ctx context.Context) {
	sm.reconcileMu.Lock()
	defer sm.reconcileMu.Unlock()

	sm.mu.Lock()
	if sm.activeTransportMode != "gh_forward" || sm.forwarder == nil || sm.webhookServer == nil {
		sm.mu.Unlock()
		return
	}
	forwarder := sm.forwarder
	webhookSrv := sm.webhookServer
	repos := append([]string(nil), sm.activeRepos...)
	cooldown := sm.forwarderRestartCooldown
	lastAttempt := sm.lastForwarderRestartAttempt
	sm.mu.Unlock()

	healthErr := forwarder.HealthCheck()
	if healthErr == nil {
		return
	}

	now := time.Now()
	if cooldown > 0 && now.Sub(lastAttempt) < cooldown {
		holonlog.Warn(
			"gh webhook forward unhealthy but restart cooldown is active",
			"error", healthErr,
			"cooldown", cooldown.String(),
		)
		return
	}

	sm.mu.Lock()
	if sm.activeTransportMode != "gh_forward" || sm.forwarder == nil || sm.webhookServer == nil {
		sm.mu.Unlock()
		return
	}
	if sm.forwarder != forwarder || sm.webhookServer != webhookSrv {
		sm.mu.Unlock()
		return
	}
	if cooldown > 0 && now.Sub(sm.lastForwarderRestartAttempt) < cooldown {
		sm.mu.Unlock()
		return
	}
	repos = append([]string(nil), sm.activeRepos...)
	sm.lastForwarderRestartAttempt = now
	sm.mu.Unlock()

	holonlog.Warn("gh webhook forward health check failed; attempting restart", "error", healthErr)
	if err := sm.reconcileGitHubTransportLocked(ctx, webhookSrv, desiredTransport{
		Mode:  "gh_forward",
		Repos: repos,
	}); err != nil {
		holonlog.Warn("failed to restart gh webhook forward after health check failure", "error", err)
		return
	}

	if err := sm.WriteStatusFile(); err != nil {
		holonlog.Warn("failed to write status file after gh webhook forward restart", "error", err)
	}
}

func watcherEvents(w *fsnotify.Watcher) <-chan fsnotify.Event {
	if w == nil {
		return nil
	}
	return w.Events
}

func watcherErrors(w *fsnotify.Watcher) <-chan error {
	if w == nil {
		return nil
	}
	return w.Errors
}

func fileSignature(path string) (string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("%d-%d", info.ModTime().UnixNano(), info.Size()), nil
}

func (sm *SubscriptionManager) reloadConfig(ctx context.Context) error {
	cfg, err := agenthome.LoadConfig(sm.agentHome)
	sm.mu.Lock()
	sm.lastReloadAt = time.Now().UTC()
	if err != nil {
		sm.lastReloadError = err.Error()
		status := sm.statusLocked()
		sm.mu.Unlock()
		_ = sm.writeStatusFile(status)
		return fmt.Errorf("load agent config from %s: %w", sm.configPath, err)
	}

	desired, desiredErr := desiredGitHubTransport(cfg)
	if desiredErr != nil {
		sm.lastReloadError = desiredErr.Error()
		status := sm.statusLocked()
		sm.mu.Unlock()
		_ = sm.writeStatusFile(status)
		return desiredErr
	}

	// Fast no-op: config doesn't change the desired transport.
	if desired.Mode == sm.activeTransportMode &&
		reflect.DeepEqual(desired.Repos, sm.activeRepos) &&
		desired.WebsocketURL == sm.activeWebsocketURL {
		sm.lastReloadError = ""
		status := sm.statusLocked()
		sm.mu.Unlock()
		_ = sm.writeStatusFile(status)
		return nil
	}

	// Capture current server ptr; Start() always creates one and keeps it running.
	webhookSrv := sm.webhookServer
	sm.mu.Unlock()
	if webhookSrv == nil {
		return fmt.Errorf("internal error: webhook server not initialized")
	}

	if err := sm.reconcileGitHubTransport(ctx, webhookSrv, desired); err != nil {
		sm.mu.Lock()
		sm.lastReloadError = err.Error()
		status := sm.statusLocked()
		sm.mu.Unlock()
		_ = sm.writeStatusFile(status)
		return err
	}

	// Reconcile succeeded: adopt the new config as the current source-of-truth.
	sm.mu.Lock()
	sm.config = cfg
	sm.lastReloadError = ""
	status := sm.statusLocked()
	sm.mu.Unlock()
	_ = sm.writeStatusFile(status)
	return nil
}

type desiredTransport struct {
	Mode         string
	Repos        []string
	WebsocketURL string
}

func desiredGitHubTransport(cfg agenthome.Config) (desiredTransport, error) {
	var gitHubSub *agenthome.GitHubSubscription
	for _, sub := range cfg.Subscriptions {
		if sub.GitHub != nil {
			gitHubSub = sub.GitHub
			break
		}
	}
	// No repos means "no ingress" regardless of transport mode (serve remains in rpc-only mode).
	if gitHubSub == nil || len(gitHubSub.Repos) == 0 {
		return desiredTransport{Mode: ""}, nil
	}

	mode := strings.TrimSpace(strings.ToLower(gitHubSub.Transport.Mode))
	if mode == "" || mode == "auto" {
		mode = "gh_forward"
	}

	switch mode {
	case "gh_forward":
		// ok
	case "websocket":
		if strings.TrimSpace(gitHubSub.Transport.WebsocketURL) == "" {
			return desiredTransport{}, fmt.Errorf("subscriptions.github.transport.websocket_url is required when transport.mode=websocket")
		}
	default:
		return desiredTransport{}, fmt.Errorf("unsupported transport mode: %s", mode)
	}

	return desiredTransport{
		Mode:         mode,
		Repos:        append([]string(nil), gitHubSub.Repos...),
		WebsocketURL: strings.TrimSpace(gitHubSub.Transport.WebsocketURL),
	}, nil
}

func (sm *SubscriptionManager) reconcileGitHubTransport(ctx context.Context, webhookSrv *WebhookServer, desired desiredTransport) error {
	// Concurrency note:
	// - Reconcile is only triggered by the single watch loop goroutine.
	// - Stop() may run concurrently during shutdown.
	// We use reconcileMu to prevent overlap between reconcile and Stop while still allowing us to
	// start new transports without holding sm.mu for the entire operation.
	sm.reconcileMu.Lock()
	defer sm.reconcileMu.Unlock()
	return sm.reconcileGitHubTransportLocked(ctx, webhookSrv, desired)
}

func (sm *SubscriptionManager) reconcileGitHubTransportLocked(ctx context.Context, webhookSrv *WebhookServer, desired desiredTransport) error {
	switch desired.Mode {
	case "":
		// Stop ingress, keep RPC server alive.
		var toStopForwarder forwarderRunner
		var toStopWS *WebSocketSource
		var toCloseSvc *Service

		sm.mu.Lock()
		toStopForwarder = sm.forwarder
		toStopWS = sm.websocketSrc
		toCloseSvc = sm.eventService
		sm.forwarder = nil
		sm.websocketSrc = nil
		sm.eventService = nil
		sm.mode = "rpc_only"
		sm.activeTransportMode = ""
		sm.activeRepos = nil
		sm.activeWebsocketURL = ""
		sm.mu.Unlock()

		if toStopForwarder != nil {
			if err := toStopForwarder.Stop(); err != nil {
				holonlog.Warn("failed to stop forwarder during reconcile", "error", err)
			}
		}
		if toStopWS != nil {
			if err := toStopWS.Stop(); err != nil {
				holonlog.Warn("failed to stop websocket source during reconcile", "error", err)
			}
		}
		if toCloseSvc != nil {
			if err := toCloseSvc.Close(); err != nil {
				holonlog.Warn("failed to close websocket event service during reconcile", "error", err)
			}
		}
		return nil
	case "gh_forward":
		port := webhookSrv.Port()
		webhookURL := BuildWebhookURL(port, "/ingress/github/webhook")
		newForwarder, err := sm.forwarderFactory(ForwarderConfig{
			Port:  port,
			Repos: desired.Repos,
			URL:   webhookURL,
		})
		if err != nil {
			return fmt.Errorf("create forwarder: %w", err)
		}
		if err := newForwarder.Start(ctx); err != nil {
			return fmt.Errorf("start forwarder: %w", err)
		}

		var toStopForwarder forwarderRunner
		var toStopWS *WebSocketSource
		var toCloseSvc *Service
		sm.mu.Lock()
		toStopForwarder = sm.forwarder
		toStopWS = sm.websocketSrc
		toCloseSvc = sm.eventService
		sm.forwarder = newForwarder
		sm.websocketSrc = nil
		sm.eventService = nil
		sm.mode = "gh_forward"
		sm.activeTransportMode = "gh_forward"
		sm.activeRepos = append([]string(nil), desired.Repos...)
		sm.activeWebsocketURL = ""
		sm.mu.Unlock()

		if toStopForwarder != nil {
			if err := toStopForwarder.Stop(); err != nil {
				holonlog.Warn("failed to stop old forwarder during reconcile", "error", err)
			}
		}
		if toStopWS != nil {
			if err := toStopWS.Stop(); err != nil {
				holonlog.Warn("failed to stop websocket source during reconcile", "error", err)
			}
		}
		if toCloseSvc != nil {
			if err := toCloseSvc.Close(); err != nil {
				holonlog.Warn("failed to close websocket event service during reconcile", "error", err)
			}
		}
		return nil
	case "websocket":
		repoHint := desired.Repos[0]
		newSrc := NewWebSocketSource(WebSocketSourceConfig{
			URL: desired.WebsocketURL,
		})

		if err := newSrc.Start(ctx, func(_ context.Context, raw []byte) error {
			normalizedRaw := normalizeWebSocketMessage(raw)
			env, normErr := normalizeLine(normalizedRaw, repoHint, time.Now)
			if normErr != nil {
				holonlog.Warn("failed to normalize websocket event", "error", normErr)
				return nil
			}
			if injectErr := webhookSrv.InjectEvent(ctx, env); injectErr != nil {
				holonlog.Warn("failed to inject websocket event", "error", injectErr, "event_id", env.ID, "type", env.Type)
			}
			return nil
		}); err != nil {
			return fmt.Errorf("start websocket source: %w", err)
		}

		var toStopForwarder forwarderRunner
		var toStopWS *WebSocketSource
		var toCloseSvc *Service
		sm.mu.Lock()
		toStopForwarder = sm.forwarder
		toStopWS = sm.websocketSrc
		toCloseSvc = sm.eventService
		sm.forwarder = nil
		sm.websocketSrc = newSrc
		sm.eventService = nil
		sm.mode = "websocket"
		sm.activeTransportMode = "websocket"
		sm.activeRepos = append([]string(nil), desired.Repos...)
		sm.activeWebsocketURL = desired.WebsocketURL
		sm.mu.Unlock()

		if toStopForwarder != nil {
			if err := toStopForwarder.Stop(); err != nil {
				holonlog.Warn("failed to stop forwarder during reconcile", "error", err)
			}
		}
		if toStopWS != nil {
			if err := toStopWS.Stop(); err != nil {
				holonlog.Warn("failed to stop old websocket source during reconcile", "error", err)
			}
		}
		if toCloseSvc != nil {
			if err := toCloseSvc.Close(); err != nil {
				holonlog.Warn("failed to close websocket event service during reconcile", "error", err)
			}
		}
		return nil
	default:
		return fmt.Errorf("unsupported transport mode: %s", desired.Mode)
	}
}

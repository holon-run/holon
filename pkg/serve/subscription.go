package serve

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/holon-run/holon/pkg/agenthome"
	holonlog "github.com/holon-run/holon/pkg/log"
)

// SubscriptionManager manages active subscriptions and their transport
type SubscriptionManager struct {
	agentHome        string
	config           agenthome.Config
	webhookServer    *WebhookServer
	forwarder        *Forwarder
	websocketSrc     *WebSocketSource
	eventService     *Service
	mu               sync.Mutex
	started          bool
	webhookPort      int
	stateDir         string
	handler          EventHandler
	dispatcher       TurnDispatcher
	defaultSessionID string
	noDefaultSession bool
	mode             string
	cancel           context.CancelFunc
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
}

// NewSubscriptionManager creates a new subscription manager
func NewSubscriptionManager(cfg ManagerConfig) (*SubscriptionManager, error) {
	// Load agent config
	agentCfg, err := agenthome.LoadConfig(cfg.AgentHome)
	if err != nil {
		return nil, fmt.Errorf("failed to load agent config: %w", err)
	}

	return &SubscriptionManager{
		agentHome:        cfg.AgentHome,
		config:           agentCfg,
		stateDir:         cfg.StateDir,
		handler:          cfg.Handler,
		webhookPort:      cfg.WebhookPort,
		dispatcher:       cfg.TurnDispatcher,
		defaultSessionID: cfg.DefaultSessionID,
		noDefaultSession: cfg.NoDefaultSession,
		mode:             "rpc_only",
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

	if gitHubSub == nil {
		if err := sm.startRPCOnlyMode(runCtx); err != nil {
			cancel()
			sm.cancel = nil
			sm.mu.Unlock()
			return err
		}
		sm.started = true
		status := sm.statusLocked()
		sm.mu.Unlock()
		if err := sm.writeStatusFile(status); err != nil {
			return err
		}
		return nil
	}

	// Determine transport mode
	transportMode := gitHubSub.Transport.Mode
	if transportMode == "" || transportMode == "auto" {
		transportMode = "gh_forward" // Default to gh_forward for local development
	}

	if len(gitHubSub.Repos) == 0 {
		if err := sm.startRPCOnlyMode(runCtx); err != nil {
			cancel()
			sm.cancel = nil
			sm.mu.Unlock()
			return err
		}
		sm.started = true
		status := sm.statusLocked()
		sm.mu.Unlock()
		if err := sm.writeStatusFile(status); err != nil {
			return err
		}
		return nil
	}

	switch transportMode {
	case "gh_forward":
		if err := sm.startGHForwardMode(runCtx, gitHubSub); err != nil {
			cancel()
			sm.cancel = nil
			sm.mu.Unlock()
			return err
		}
	case "websocket":
		if err := sm.startWebSocketMode(runCtx, gitHubSub); err != nil {
			cancel()
			sm.cancel = nil
			sm.mu.Unlock()
			return err
		}
	default:
		cancel()
		sm.cancel = nil
		sm.mu.Unlock()
		return fmt.Errorf("unsupported transport mode: %s", transportMode)
	}

	sm.started = true
	status := sm.statusLocked()
	sm.mu.Unlock()
	if err := sm.writeStatusFile(status); err != nil {
		return err
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
	forwarder, err := NewForwarder(ForwarderConfig{
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
	sm.mu.Lock()

	if !sm.started {
		sm.mu.Unlock()
		return nil
	}

	var errs []error

	if sm.cancel != nil {
		sm.cancel()
		sm.cancel = nil
	}

	// Stop forwarder first
	if sm.forwarder != nil {
		if err := sm.forwarder.Stop(); err != nil {
			errs = append(errs, fmt.Errorf("failed to stop forwarder: %w", err))
		}
		sm.forwarder = nil
	}

	// Stop websocket source
	if sm.websocketSrc != nil {
		if err := sm.websocketSrc.Stop(); err != nil {
			errs = append(errs, fmt.Errorf("failed to stop websocket source: %w", err))
		}
		sm.websocketSrc = nil
	}
	if sm.eventService != nil {
		if err := sm.eventService.Close(); err != nil {
			errs = append(errs, fmt.Errorf("failed to close websocket event service: %w", err))
		}
		sm.eventService = nil
	}

	// Stop webhook server
	if sm.webhookServer != nil {
		if err := sm.webhookServer.Close(); err != nil {
			errs = append(errs, fmt.Errorf("failed to stop webhook server: %w", err))
		}
		sm.webhookServer = nil
	}

	sm.started = false
	status := sm.statusLocked()
	sm.mu.Unlock()
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
		"state_dir":      sm.stateDir,
		"updated_at":     time.Now().UTC().Format(time.RFC3339Nano),
	}
	if reason != "" {
		status["reason"] = reason
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

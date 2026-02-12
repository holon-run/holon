package serve

import (
	"context"
	"encoding/json"
	"fmt"
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
	agentHome     string
	config        agenthome.Config
	webhookServer *WebhookServer
	forwarder     *Forwarder
	websocketSrc  *WebSocketSource
	eventService  *Service
	mu            sync.Mutex
	started       bool
	webhookPort   int
	stateDir      string
	handler       EventHandler
}

// ManagerConfig holds configuration for SubscriptionManager
type ManagerConfig struct {
	AgentHome   string
	StateDir    string
	Handler     EventHandler
	WebhookPort int // 0 means auto-select
}

// NewSubscriptionManager creates a new subscription manager
func NewSubscriptionManager(cfg ManagerConfig) (*SubscriptionManager, error) {
	// Load agent config
	agentCfg, err := agenthome.LoadConfig(cfg.AgentHome)
	if err != nil {
		return nil, fmt.Errorf("failed to load agent config: %w", err)
	}

	return &SubscriptionManager{
		agentHome:   cfg.AgentHome,
		config:      agentCfg,
		stateDir:    cfg.StateDir,
		handler:     cfg.Handler,
		webhookPort: cfg.WebhookPort,
	}, nil
}

// Start starts the subscription manager and all configured subscriptions
func (sm *SubscriptionManager) Start(ctx context.Context) error {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.started {
		return fmt.Errorf("subscription manager already started")
	}

	// Find GitHub subscriptions
	var gitHubSub *agenthome.GitHubSubscription
	for _, sub := range sm.config.Subscriptions {
		if sub.GitHub != nil {
			gitHubSub = sub.GitHub
			break
		}
	}

	if gitHubSub == nil {
		holonlog.Info("no GitHub subscriptions configured, running in passive mode")
		sm.started = true
		if err := sm.writeStatusFileLocked(); err != nil {
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
		holonlog.Info("GitHub subscription configured with no repos, running in passive mode")
		sm.started = true
		if err := sm.writeStatusFileLocked(); err != nil {
			return err
		}
		return nil
	}

	switch transportMode {
	case "gh_forward":
		if err := sm.startGHForwardMode(ctx, gitHubSub); err != nil {
			return err
		}
	case "websocket":
		if err := sm.startWebSocketMode(ctx, gitHubSub); err != nil {
			return err
		}
	default:
		return fmt.Errorf("unsupported transport mode: %s", transportMode)
	}

	sm.started = true
	if err := sm.writeStatusFileLocked(); err != nil {
		return err
	}
	return nil
}

func (sm *SubscriptionManager) startGHForwardMode(ctx context.Context, sub *agenthome.GitHubSubscription) error {
	// Determine webhook port
	port := sm.webhookPort
	if port == 0 {
		var err error
		port, err = GetAvailablePort()
		if err != nil {
			return fmt.Errorf("failed to find available port: %w", err)
		}
		holonlog.Info("auto-selected webhook port", "port", port)
	}

	// Create webhook server
	webhookURL := BuildWebhookURL(port, "/ingress/github/webhook")
	webhookSrv, err := NewWebhookServer(WebhookConfig{
		Port:     port,
		RepoHint: sub.Repos[0], // Use first repo as hint
		StateDir: sm.stateDir,
		Handler:  sm.handler,
	})
	if err != nil {
		return fmt.Errorf("failed to create webhook server: %w", err)
	}
	sm.webhookServer = webhookSrv

	// Start webhook server
	if err := webhookSrv.Start(ctx); err != nil {
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

	holonlog.Info(
		"subscription manager started",
		"transport", "gh_forward",
		"webhook_port", port,
		"repos", sub.Repos,
	)

	return nil
}

func (sm *SubscriptionManager) startWebSocketMode(ctx context.Context, sub *agenthome.GitHubSubscription) error {
	repoHint := ""
	if len(sub.Repos) > 0 {
		repoHint = sub.Repos[0]
	}

	svc, err := New(Config{
		RepoHint: repoHint,
		StateDir: sm.stateDir,
		Handler:  sm.handler,
	})
	if err != nil {
		return fmt.Errorf("failed to create websocket event service: %w", err)
	}
	sm.eventService = svc

	src := NewWebSocketSource(WebSocketSourceConfig{
		URL: sub.Transport.WebsocketURL,
	})
	sm.websocketSrc = src

	if err := src.Start(ctx, func(_ context.Context, raw []byte) error {
		normalizedRaw := normalizeWebSocketMessage(raw)
		env, normErr := normalizeLine(normalizedRaw, repoHint, time.Now)
		if normErr != nil {
			holonlog.Warn("failed to normalize websocket event", "error", normErr)
			return nil
		}
		if injectErr := svc.InjectEvent(ctx, env); injectErr != nil {
			holonlog.Warn("failed to inject websocket event", "error", injectErr, "event_id", env.ID, "type", env.Type)
		}
		return nil
	}); err != nil {
		svc.Close()
		sm.eventService = nil
		sm.websocketSrc = nil
		return fmt.Errorf("failed to start websocket source: %w", err)
	}

	holonlog.Info(
		"subscription manager started",
		"transport", "websocket",
		"url", sub.Transport.WebsocketURL,
		"repos", sub.Repos,
	)

	return nil
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
	defer sm.mu.Unlock()

	if !sm.started {
		return nil
	}

	var errs []error

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
	if err := sm.writeStatusFileLocked(); err != nil {
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
	defer sm.mu.Unlock()

	if sm.webhookServer == nil {
		return fmt.Errorf("webhook server not running")
	}

	return sm.webhookServer.InjectEvent(ctx, env)
}

// Status returns the current status of the subscription manager
func (sm *SubscriptionManager) Status() map[string]interface{} {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	return sm.statusLocked()
}

func (sm *SubscriptionManager) statusLocked() map[string]interface{} {
	status := map[string]interface{}{
		"running":    sm.started,
		"agent_home": sm.agentHome,
		"state_dir":  sm.stateDir,
		"updated_at": time.Now().UTC().Format(time.RFC3339Nano),
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

// WriteStatusFile writes the current status to a file in the state directory
func (sm *SubscriptionManager) WriteStatusFile() error {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	return sm.writeStatusFileLocked()
}

func (sm *SubscriptionManager) writeStatusFileLocked() error {
	status := sm.statusLocked()

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

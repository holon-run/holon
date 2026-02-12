package serve

import (
	"context"
	"fmt"
	"sync"

	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/agenthome"
)

// SubscriptionManager manages active subscriptions and their transport
type SubscriptionManager struct {
	agentHome      string
	config         agenthome.Config
	webhookServer  *WebhookServer
	forwarder      *Forwarder
	mu             sync.Mutex
	started        bool
	webhookPort    int
	stateDir       string
	handler        EventHandler
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
		return nil
	}

	switch transportMode {
	case "gh_forward":
		return sm.startGHForwardMode(ctx, gitHubSub)
	case "websocket":
		return fmt.Errorf("websocket transport mode not yet implemented")
	default:
		return fmt.Errorf("unsupported transport mode: %s", transportMode)
	}
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

	sm.started = true

	holonlog.Info(
		"subscription manager started",
		"transport", "gh_forward",
		"webhook_port", port,
		"repos", sub.Repos,
	)

	return nil
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

	// Stop webhook server
	if sm.webhookServer != nil {
		if err := sm.webhookServer.Close(); err != nil {
			errs = append(errs, fmt.Errorf("failed to stop webhook server: %w", err))
		}
		sm.webhookServer = nil
	}

	sm.started = false

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

	status := map[string]interface{}{
		"running": sm.started,
		"agent_home": sm.agentHome,
	}

	if sm.webhookServer != nil {
		status["webhook_port"] = sm.webhookServer.Port()
		status["webhook_url"] = BuildWebhookURL(sm.webhookServer.Port(), "/ingress/github/webhook")
	}

	if sm.forwarder != nil {
		status["forwarder"] = sm.forwarder.Status()
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
// TODO: Implement actual file writing to state directory
func (sm *SubscriptionManager) WriteStatusFile() error {
	status := sm.Status()

	// For now, just log the status
	// TODO: Write status to state directory (e.g., sm.stateDir/status.json)
	holonlog.Info("subscription status", "status", status)
	return nil
}

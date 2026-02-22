package serve

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/holon-run/holon/pkg/agenthome"
)

type captureEventHandler struct {
	mu     sync.Mutex
	events []EventEnvelope
	ch     chan EventEnvelope
}

func (h *captureEventHandler) HandleEvent(_ context.Context, env EventEnvelope) error {
	h.mu.Lock()
	h.events = append(h.events, env)
	h.mu.Unlock()
	if h.ch != nil {
		select {
		case h.ch <- env:
		default:
		}
	}
	return nil
}

func TestNormalizeWebSocketMessage_MergesHeadersIntoPayload(t *testing.T) {
	raw := []byte(`{
		"headers": {
			"X-GitHub-Event": "issue_comment",
			"X-GitHub-Delivery": "delivery-1"
		},
		"payload": {
			"action": "created",
			"issue": {"number": 123},
			"repository": {"full_name": "holon-run/holon"}
		}
	}`)

	out := normalizeWebSocketMessage(raw)

	var payload map[string]any
	if err := json.Unmarshal(out, &payload); err != nil {
		t.Fatalf("unmarshal normalized payload: %v", err)
	}
	if payload["x_github_event"] != "issue_comment" {
		t.Fatalf("x_github_event = %v, want issue_comment", payload["x_github_event"])
	}
	if payload["x_github_delivery"] != "delivery-1" {
		t.Fatalf("x_github_delivery = %v, want delivery-1", payload["x_github_delivery"])
	}
}

func TestSubscriptionManager_WriteStatusFile(t *testing.T) {
	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "test",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{"holon-run/holon"},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode: "gh_forward",
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	handler := &captureEventHandler{}
	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome: agentHome,
		StateDir:  stateDir,
		Handler:   handler,
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}
	if err := sm.WriteStatusFile(); err != nil {
		t.Fatalf("write status file: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(stateDir, "subscription-status.json"))
	if err != nil {
		t.Fatalf("read status file: %v", err)
	}
	var status map[string]any
	if err := json.Unmarshal(data, &status); err != nil {
		t.Fatalf("unmarshal status file: %v", err)
	}
	if status["agent_home"] != agentHome {
		t.Fatalf("agent_home = %v, want %s", status["agent_home"], agentHome)
	}
	if status["transport_mode"] != "gh_forward" {
		t.Fatalf("transport_mode = %v, want gh_forward", status["transport_mode"])
	}
	if status["mode"] != "rpc_only" {
		t.Fatalf("mode = %v, want rpc_only before start", status["mode"])
	}
	if _, ok := status["reason"]; ok {
		t.Fatalf("reason should be omitted before start, got %v", status["reason"])
	}
}

func TestSubscriptionManager_StartRPCOnlyWhenNoRepos(t *testing.T) {
	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "test",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode: "gh_forward",
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	handler := &captureEventHandler{}
	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome:   agentHome,
		StateDir:    stateDir,
		Handler:     handler,
		WebhookPort: mustGetPort(t),
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := sm.Start(ctx); err != nil {
		t.Fatalf("start subscription manager: %v", err)
	}
	defer sm.Stop()

	status := sm.Status()
	if status["mode"] != "rpc_only" {
		t.Fatalf("mode = %v, want rpc_only", status["mode"])
	}
	if status["rpc_active"] != true {
		t.Fatalf("rpc_active = %v, want true", status["rpc_active"])
	}
	if status["reason"] != "empty_repos" {
		t.Fatalf("reason = %v, want empty_repos", status["reason"])
	}
}

func TestSubscriptionManager_WebSocketMode_StartsAndProcessesEvents(t *testing.T) {
	received := make(chan EventEnvelope, 1)
	handler := &captureEventHandler{ch: received}
	upgrader := websocket.Upgrader{}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()
		msg := `{
			"headers": {
				"X-GitHub-Event": "issue_comment",
				"X-GitHub-Delivery": "delivery-2"
			},
			"payload": {
				"action": "created",
				"issue": {"number": 641},
				"repository": {"full_name": "holon-run/holon"},
				"comment": {"id": 2001}
			}
		}`
		_ = conn.WriteMessage(websocket.TextMessage, []byte(msg))
		<-r.Context().Done()
	}))
	defer server.Close()

	wsURL := "ws" + strings.TrimPrefix(server.URL, "http")

	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "main",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{"holon-run/holon"},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode:         "websocket",
						WebsocketURL: wsURL,
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome:   agentHome,
		StateDir:    stateDir,
		Handler:     handler,
		WebhookPort: mustGetPort(t),
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := sm.Start(ctx); err != nil {
		t.Fatalf("start subscription manager: %v", err)
	}
	defer sm.Stop()

	select {
	case env := <-received:
		if env.Type != "github.issue.comment.created" {
			t.Fatalf("event type = %s, want github.issue.comment.created", env.Type)
		}
		if env.Scope.Repo != "holon-run/holon" {
			t.Fatalf("event repo = %s, want holon-run/holon", env.Scope.Repo)
		}
	case <-time.After(3 * time.Second):
		t.Fatalf("timed out waiting for websocket event")
	}

	data, err := os.ReadFile(filepath.Join(stateDir, "subscription-status.json"))
	if err != nil {
		t.Fatalf("read subscription status: %v", err)
	}
	var status map[string]any
	if err := json.Unmarshal(data, &status); err != nil {
		t.Fatalf("unmarshal subscription status: %v", err)
	}
	if status["transport_mode"] != "websocket" {
		t.Fatalf("transport_mode = %v, want websocket", status["transport_mode"])
	}
}

func mustGetPort(t *testing.T) int {
	t.Helper()
	l, err := (&net.ListenConfig{}).Listen(context.Background(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen for test port: %v", err)
	}
	defer l.Close()
	addr, ok := l.Addr().(*net.TCPAddr)
	if !ok {
		t.Fatalf("listener addr type = %T, want *net.TCPAddr", l.Addr())
	}
	port := addr.Port
	if port <= 0 {
		t.Fatalf("invalid ephemeral port: %d", port)
	}
	return port
}

func TestSubscriptionManager_HotReload_EnablesWebSocketIngress(t *testing.T) {
	received := make(chan EventEnvelope, 1)
	handler := &captureEventHandler{ch: received}
	upgrader := websocket.Upgrader{}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()
		msg := `{
			"headers": {
				"X-GitHub-Event": "issue_comment",
				"X-GitHub-Delivery": "delivery-hr-1"
			},
			"payload": {
				"action": "created",
				"issue": {"number": 777},
				"repository": {"full_name": "holon-run/holon"},
				"comment": {"id": 2002}
			}
		}`
		_ = conn.WriteMessage(websocket.TextMessage, []byte(msg))
		<-r.Context().Done()
	}))
	defer server.Close()

	wsURL := "ws" + strings.TrimPrefix(server.URL, "http")

	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	// Start with empty repos -> rpc-only.
	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "main",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode:         "websocket",
						WebsocketURL: wsURL,
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome:          agentHome,
		StateDir:           stateDir,
		Handler:            handler,
		WebhookPort:        mustGetPort(t),
		ReloadPollInterval: 20 * time.Millisecond,
		ReloadDebounce:     10 * time.Millisecond,
		DisableHotReload:   false,
		DefaultSessionID:   "",
		NoDefaultSession:   false,
		TurnDispatcher:     nil,
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := sm.Start(ctx); err != nil {
		t.Fatalf("start subscription manager: %v", err)
	}
	defer sm.Stop()

	status := sm.Status()
	if status["mode"] != "rpc_only" {
		t.Fatalf("mode = %v, want rpc_only before reload", status["mode"])
	}

	// Enable websocket ingress by writing updated config.
	cfg.Subscriptions[0].GitHub.Repos = []string{"holon-run/holon"}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config (reload): %v", err)
	}

	select {
	case env := <-received:
		if env.Type != "github.issue.comment.created" {
			t.Fatalf("event type = %s, want github.issue.comment.created", env.Type)
		}
	case <-time.After(3 * time.Second):
		t.Fatalf("timed out waiting for websocket event after reload")
	}

	status = sm.Status()
	if status["mode"] != "websocket" {
		t.Fatalf("mode = %v, want websocket after reload", status["mode"])
	}
	if status["ingress_active"] != true {
		t.Fatalf("ingress_active = %v, want true after reload", status["ingress_active"])
	}
}

func TestSubscriptionManager_HotReload_InvalidConfigKeepsExistingIngress(t *testing.T) {
	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	received := make(chan EventEnvelope, 10)
	handler := &captureEventHandler{ch: received}
	upgrader := websocket.Upgrader{}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()
		msg := `{
			"headers": {
				"X-GitHub-Event": "issue_comment",
				"X-GitHub-Delivery": "delivery-hr-2"
			},
			"payload": {
				"action": "created",
				"issue": {"number": 888},
				"repository": {"full_name": "holon-run/holon"},
				"comment": {"id": 2003}
			}
		}`
		tick := time.NewTicker(50 * time.Millisecond)
		defer tick.Stop()
		for {
			select {
			case <-r.Context().Done():
				return
			case <-tick.C:
				_ = conn.WriteMessage(websocket.TextMessage, []byte(msg))
			}
		}
	}))
	defer server.Close()
	wsURL := "ws" + strings.TrimPrefix(server.URL, "http")

	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "main",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{"holon-run/holon"},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode:         "websocket",
						WebsocketURL: wsURL,
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome:          agentHome,
		StateDir:           stateDir,
		Handler:            handler,
		WebhookPort:        mustGetPort(t),
		ReloadPollInterval: 20 * time.Millisecond,
		ReloadDebounce:     10 * time.Millisecond,
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := sm.Start(ctx); err != nil {
		t.Fatalf("start subscription manager: %v", err)
	}
	defer sm.Stop()

	// Ensure we're receiving events.
	select {
	case <-received:
	case <-time.After(3 * time.Second):
		t.Fatalf("timed out waiting for websocket event before invalid reload")
	}

	// Write invalid YAML to trigger reload error.
	if err := os.WriteFile(filepath.Join(agentHome, "agent.yaml"), []byte("version: v1\nagent:\n  id: [\n"), 0644); err != nil {
		t.Fatalf("write invalid config: %v", err)
	}

	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		status := sm.Status()
		if status["mode"] == "websocket" && status["ingress_active"] == true && status["last_reload_error"] != nil {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	status := sm.Status()
	if status["mode"] != "websocket" {
		t.Fatalf("mode = %v, want websocket after invalid reload", status["mode"])
	}
	if status["ingress_active"] != true {
		t.Fatalf("ingress_active = %v, want true after invalid reload", status["ingress_active"])
	}
	if status["last_reload_error"] == nil {
		t.Fatalf("last_reload_error should be set after invalid reload")
	}

	// Fix config and verify the error clears.
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save fixed config: %v", err)
	}
	deadline = time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		status = sm.Status()
		if status["last_reload_error"] == nil {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	status = sm.Status()
	if status["last_reload_error"] != nil {
		t.Fatalf("last_reload_error should clear after fixing config, got %v", status["last_reload_error"])
	}
}

type fakeForwarder struct {
	mu        sync.Mutex
	started   bool
	starts    int
	stops     int
	lastRepos []string
	unhealthy bool
}

func (f *fakeForwarder) Start(_ context.Context) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.started = true
	f.starts++
	return nil
}

func (f *fakeForwarder) Stop() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if !f.started {
		return nil
	}
	f.started = false
	f.stops++
	return nil
}

func (f *fakeForwarder) Status() map[string]interface{} {
	f.mu.Lock()
	defer f.mu.Unlock()
	return map[string]interface{}{
		"running": f.started,
		"starts":  f.starts,
		"stops":   f.stops,
		"repos":   append([]string(nil), f.lastRepos...),
	}
}

func (f *fakeForwarder) HealthCheck() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if !f.started {
		return fmt.Errorf("forwarder not started")
	}
	if f.unhealthy {
		return fmt.Errorf("forwarder unhealthy")
	}
	return nil
}

func TestSubscriptionManager_HotReload_SwitchesTransportMode_WebsocketToGHForward(t *testing.T) {
	handler := &captureEventHandler{}
	upgrader := websocket.Upgrader{}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()
		<-r.Context().Done()
	}))
	defer server.Close()

	wsURL := "ws" + strings.TrimPrefix(server.URL, "http")

	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "main",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{"holon-run/holon"},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode:         "websocket",
						WebsocketURL: wsURL,
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	var created []*fakeForwarder
	factory := func(fc ForwarderConfig) (forwarderRunner, error) {
		ff := &fakeForwarder{lastRepos: append([]string(nil), fc.Repos...)}
		created = append(created, ff)
		return ff, nil
	}

	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome:          agentHome,
		StateDir:           stateDir,
		Handler:            handler,
		WebhookPort:        mustGetPort(t),
		ReloadPollInterval: 20 * time.Millisecond,
		ReloadDebounce:     10 * time.Millisecond,
		ForwarderFactory:   factory,
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := sm.Start(ctx); err != nil {
		t.Fatalf("start subscription manager: %v", err)
	}
	defer sm.Stop()

	// Wait for websocket to connect so we can verify it gets stopped.
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		status := sm.Status()
		ws, ok := status["websocket"].(map[string]interface{})
		if ok && ws["connected"] == true {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	sm.mu.Lock()
	oldSrc := sm.websocketSrc
	sm.mu.Unlock()
	if oldSrc == nil {
		t.Fatalf("expected websocket source to be initialized")
	}

	// Switch to gh_forward.
	cfg.Subscriptions[0].GitHub.Transport.Mode = "gh_forward"
	cfg.Subscriptions[0].GitHub.Transport.WebsocketURL = ""
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config (switch): %v", err)
	}

	deadline = time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		status := sm.Status()
		if status["mode"] == "gh_forward" && status["ingress_active"] == true {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	status := sm.Status()
	if status["mode"] != "gh_forward" {
		t.Fatalf("mode = %v, want gh_forward after switch", status["mode"])
	}
	if status["ingress_active"] != true {
		t.Fatalf("ingress_active = %v, want true after switch", status["ingress_active"])
	}
	if len(created) != 1 {
		t.Fatalf("expected forwarder to be created once, got %d", len(created))
	}
	if created[0].Status()["running"] != true {
		t.Fatalf("expected forwarder to be running after switch")
	}

	// Old websocket source should be stopped.
	deadline = time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if oldSrc.Status()["running"] == false {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if oldSrc.Status()["running"] != false {
		t.Fatalf("expected old websocket source to be stopped after switch")
	}
}

func TestSubscriptionManager_AutoRestartsForwarderWhenHealthCheckFails(t *testing.T) {
	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "main",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{"holon-run/holon"},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode: "gh_forward",
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	var (
		createdMu sync.Mutex
		created   []*fakeForwarder
	)
	factory := func(fc ForwarderConfig) (forwarderRunner, error) {
		ff := &fakeForwarder{lastRepos: append([]string(nil), fc.Repos...)}
		createdMu.Lock()
		created = append(created, ff)
		createdMu.Unlock()
		return ff, nil
	}

	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome:                agentHome,
		StateDir:                 stateDir,
		Handler:                  &captureEventHandler{},
		WebhookPort:              mustGetPort(t),
		ReloadPollInterval:       20 * time.Millisecond,
		ReloadDebounce:           10 * time.Millisecond,
		ForwarderRestartCooldown: 10 * time.Millisecond,
		ForwarderFactory:         factory,
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := sm.Start(ctx); err != nil {
		t.Fatalf("start subscription manager: %v", err)
	}
	defer sm.Stop()

	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		createdMu.Lock()
		n := len(created)
		createdMu.Unlock()
		if n >= 1 {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	createdMu.Lock()
	if len(created) < 1 {
		createdMu.Unlock()
		t.Fatalf("expected initial forwarder to be created")
	}
	initial := created[0]
	createdMu.Unlock()

	// Simulate an unhealthy forwarder while process is still considered started.
	initial.mu.Lock()
	initial.unhealthy = true
	initial.mu.Unlock()

	deadline = time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		createdMu.Lock()
		n := len(created)
		createdMu.Unlock()
		if n >= 2 {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	createdMu.Lock()
	if len(created) < 2 {
		createdMu.Unlock()
		t.Fatalf("expected a new forwarder to be created after health check failure")
	}
	restarted := created[len(created)-1]
	createdMu.Unlock()

	restarted.mu.Lock()
	running := restarted.started
	restarted.mu.Unlock()
	if !running {
		t.Fatalf("expected restarted forwarder to be running")
	}
}

func TestSubscriptionManager_AutoRestartsForwarderWhenHotReloadDisabled(t *testing.T) {
	agentHome := t.TempDir()
	stateDir := filepath.Join(agentHome, "state")
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		t.Fatalf("mkdir state dir: %v", err)
	}

	cfg := agenthome.Config{
		Version: "v1",
		Agent: agenthome.AgentConfig{
			ID:      "main",
			Profile: "default",
		},
		Subscriptions: []agenthome.Subscription{
			{
				GitHub: &agenthome.GitHubSubscription{
					Repos: []string{"holon-run/holon"},
					Transport: agenthome.GitHubSubscriptionTransport{
						Mode: "gh_forward",
					},
				},
			},
		},
	}
	if err := agenthome.SaveConfig(agentHome, cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	var (
		createdMu sync.Mutex
		created   []*fakeForwarder
	)
	factory := func(fc ForwarderConfig) (forwarderRunner, error) {
		ff := &fakeForwarder{lastRepos: append([]string(nil), fc.Repos...)}
		createdMu.Lock()
		created = append(created, ff)
		createdMu.Unlock()
		return ff, nil
	}

	sm, err := NewSubscriptionManager(ManagerConfig{
		AgentHome:                agentHome,
		StateDir:                 stateDir,
		Handler:                  &captureEventHandler{},
		WebhookPort:              mustGetPort(t),
		ReloadPollInterval:       20 * time.Millisecond,
		ForwarderRestartCooldown: 10 * time.Millisecond,
		DisableHotReload:         true,
		ForwarderFactory:         factory,
	})
	if err != nil {
		t.Fatalf("new subscription manager: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := sm.Start(ctx); err != nil {
		t.Fatalf("start subscription manager: %v", err)
	}
	defer sm.Stop()

	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		createdMu.Lock()
		n := len(created)
		createdMu.Unlock()
		if n >= 1 {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	createdMu.Lock()
	if len(created) < 1 {
		createdMu.Unlock()
		t.Fatalf("expected initial forwarder to be created")
	}
	initial := created[0]
	createdMu.Unlock()

	initial.mu.Lock()
	initial.unhealthy = true
	initial.mu.Unlock()

	deadline = time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		createdMu.Lock()
		n := len(created)
		createdMu.Unlock()
		if n >= 2 {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}

	createdMu.Lock()
	if len(created) < 2 {
		createdMu.Unlock()
		t.Fatalf("expected a new forwarder to be created after health check failure")
	}
	createdMu.Unlock()
}

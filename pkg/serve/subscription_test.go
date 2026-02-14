package serve

import (
	"context"
	"encoding/json"
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

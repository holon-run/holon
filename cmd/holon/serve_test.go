package main

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/holon-run/holon/pkg/runtime/docker"
	"github.com/holon-run/holon/pkg/serve"
)

func TestFirstNonEmpty(t *testing.T) {
	t.Parallel()

	if got := firstNonEmpty("", "  ", "x", "y"); got != "x" {
		t.Fatalf("firstNonEmpty = %q, want x", got)
	}
}

func TestBuildServeStartupDiagnostics_SubscriptionRPCOnly(t *testing.T) {
	t.Parallel()

	diag := buildServeStartupDiagnostics(serveStartupDiagnosticsInput{
		AgentID:             "main",
		AgentHome:           "/tmp/agent",
		StateDir:            "/tmp/agent/state",
		Workspace:           "/tmp/agent/workspace",
		ConfigSource:        "/tmp/agent/agent.yaml",
		RoleSource:          "/tmp/agent/ROLE.md",
		RoleInferred:        "pm",
		ServeInput:          "-",
		InputMode:           "subscription",
		SubscriptionEnabled: true,
		SubscriptionStatus: map[string]interface{}{
			"mode":             "rpc_only",
			"reason":           "empty_repos",
			"subscribed_repos": []string{},
		},
		TickInterval: 0,
		RuntimeMode:  "prod",
	})

	if diag.TransportMode != "rpc_only" {
		t.Fatalf("transport_mode = %q, want rpc_only", diag.TransportMode)
	}
	if diag.SubscriptionReason != "empty_repos" {
		t.Fatalf("subscription_reason = %q, want empty_repos", diag.SubscriptionReason)
	}
	if diag.RoleSource != "/tmp/agent/ROLE.md" {
		t.Fatalf("role_source = %q", diag.RoleSource)
	}
	if diag.RoleInferred != "pm" {
		t.Fatalf("role_inferred = %q", diag.RoleInferred)
	}
	joinedWarnings := strings.Join(diag.Warnings, " | ")
	if !strings.Contains(joinedWarnings, "subscriptions.github.repos is empty") {
		t.Fatalf("warnings missing empty repos guidance: %s", joinedWarnings)
	}
	if !strings.Contains(joinedWarnings, "idle behavior") {
		t.Fatalf("warnings missing idle behavior guidance: %s", joinedWarnings)
	}

	data, err := json.Marshal(diag)
	if err != nil {
		t.Fatalf("marshal diagnostics: %v", err)
	}
	if strings.Contains(string(data), "controller_role") {
		t.Fatalf("diagnostics should not contain controller_role field: %s", string(data))
	}
}

func TestBuildServeStartupDiagnostics_StdinModeWarnings(t *testing.T) {
	t.Parallel()

	diag := buildServeStartupDiagnostics(serveStartupDiagnosticsInput{
		InputMode:           "stdin_file",
		SubscriptionEnabled: false,
		ServeInput:          "-",
		NoSubscriptionsFlag: true,
		RuntimeMode:         "prod",
	})
	joinedWarnings := strings.Join(diag.Warnings, " | ")
	if !strings.Contains(joinedWarnings, "waiting for newline-delimited JSON events on stdin") {
		t.Fatalf("warnings missing stdin idle guidance: %s", joinedWarnings)
	}
	if !strings.Contains(joinedWarnings, "--no-subscriptions is enabled") {
		t.Fatalf("warnings missing no-subscriptions guidance: %s", joinedWarnings)
	}
}

func TestBuildServeStartupDiagnostics_SubscriptionAutoTransportUsesEffectiveMode(t *testing.T) {
	t.Parallel()

	diag := buildServeStartupDiagnostics(serveStartupDiagnosticsInput{
		InputMode: "subscription",
		SubscriptionStatus: map[string]interface{}{
			"mode":           "gh_forward",
			"transport_mode": "auto",
		},
		RuntimeMode: "prod",
	})
	if diag.TransportMode != "gh_forward" {
		t.Fatalf("transport_mode = %q, want gh_forward", diag.TransportMode)
	}
}

func TestWriteServeStartupDiagnostics(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	diag := serveStartupDiagnostics{
		AgentID:      "main",
		RoleSource:   filepath.Join(td, "ROLE.md"),
		RoleInferred: "pm",
		Preview:      "experimental",
	}
	if err := writeServeStartupDiagnostics(td, diag); err != nil {
		t.Fatalf("writeServeStartupDiagnostics() error = %v", err)
	}
	data, err := os.ReadFile(filepath.Join(td, "serve-startup-diagnostics.json"))
	if err != nil {
		t.Fatalf("read diagnostics file: %v", err)
	}
	var got map[string]interface{}
	if err := json.Unmarshal(data, &got); err != nil {
		t.Fatalf("unmarshal diagnostics file: %v", err)
	}
	if got["role_source"] != diag.RoleSource {
		t.Fatalf("role_source = %v, want %s", got["role_source"], diag.RoleSource)
	}
	if got["role_inferred"] != "pm" {
		t.Fatalf("role_inferred = %v, want pm", got["role_inferred"])
	}
}

func TestSessionStatePathAndReadSessionID(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	h := &cliControllerHandler{stateDir: td}
	if got := h.sessionStatePath(); got != filepath.Join(td, "controller-state", "controller-session.json") {
		t.Fatalf("sessionStatePath() = %q", got)
	}
	if got := h.readSessionID(); got != "" {
		t.Fatalf("readSessionID() for missing file = %q, want empty", got)
	}

	if err := os.MkdirAll(filepath.Dir(h.sessionStatePath()), 0o755); err != nil {
		t.Fatalf("mkdir session dir: %v", err)
	}
	if err := os.WriteFile(h.sessionStatePath(), []byte(`{"session_id":"abc123"}`), 0o644); err != nil {
		t.Fatalf("write session state: %v", err)
	}
	if got := h.readSessionID(); got != "abc123" {
		t.Fatalf("readSessionID() = %q, want abc123", got)
	}
}

func TestAcquireServeAgentLock_BasicLifecycle(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	release, err := acquireServeAgentLock(td)
	if err != nil {
		t.Fatalf("first acquire failed: %v", err)
	}

	if _, err := acquireServeAgentLock(td); err == nil {
		t.Fatalf("expected second acquire to fail while locked")
	}

	release()

	release2, err := acquireServeAgentLock(td)
	if err != nil {
		t.Fatalf("acquire after release failed: %v", err)
	}
	release2()
}

func TestAcquireServeAgentLock_RemovesStaleLock(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	lockPath := filepath.Join(td, "agent.lock")
	if err := os.WriteFile(lockPath, []byte("999999\n"), 0o644); err != nil {
		t.Fatalf("write stale lock: %v", err)
	}

	release, err := acquireServeAgentLock(td)
	if err != nil {
		t.Fatalf("acquire with stale lock failed: %v", err)
	}
	release()
}

func TestHandleEvent_PersistentControllerAndReconnect(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	agentHome := shortTempDir(t, "holon-661-home")
	socketPath := filepath.Join(agentHome, "run", "agent.sock")
	rpcServer := newMockControllerRPCServer(t, socketPath)
	defer rpcServer.Close()

	mockRunner := &mockSessionRunner{
		waitCh:       make(chan error, 2),
		waitObserved: make(chan struct{}, 2),
	}

	h := &cliControllerHandler{
		repoHint:            "holon-run/holon",
		stateDir:            td,
		agentHome:           agentHome,
		controllerWorkspace: t.TempDir(),
		controllerRoleLabel: "dev",
		logLevel:            "progress",
		sessionRunner:       mockRunner,
	}
	defer h.Close()

	ctx := context.Background()
	env1 := serve.EventEnvelope{
		ID:   "evt-1",
		Type: "issue_comment",
		Scope: serve.EventScope{
			Repo: "holon-run/holon",
		},
		Subject: serve.EventSubject{
			Kind: "issue",
			ID:   "579",
		},
	}
	env2 := env1
	env2.ID = "evt-2"
	env3 := env1
	env3.ID = "evt-3"

	if err := h.HandleEvent(ctx, env1); err != nil {
		t.Fatalf("handle event1: %v", err)
	}
	if err := h.HandleEvent(ctx, env2); err != nil {
		t.Fatalf("handle event2: %v", err)
	}
	rpcServer.WaitForEvents(t, 2, 2*time.Second)

	if h.restartAttempts != 1 {
		t.Fatalf("restartAttempts after 2 events = %d, want 1", h.restartAttempts)
	}
	if mockRunner.startCount != 1 {
		t.Fatalf("startCount after 2 events = %d, want 1", mockRunner.startCount)
	}
	if got := mockRunner.lastConfig.Env["HOLON_WORKSPACE_INDEX_PATH"]; got != "/root/state/workspace-index.json" {
		t.Fatalf("HOLON_WORKSPACE_INDEX_PATH = %q, want /root/state/workspace-index.json", got)
	}
	if mockRunner.lastConfig.Workspace != h.controllerWorkspace {
		t.Fatalf("Workspace = %q, want %q", mockRunner.lastConfig.Workspace, h.controllerWorkspace)
	}
	if got := mockRunner.lastConfig.Env["HOLON_CONTROLLER_RPC_SOCKET"]; got != "/root/run/agent.sock" {
		t.Fatalf("HOLON_CONTROLLER_RPC_SOCKET = %q, want /root/run/agent.sock", got)
	}
	// Force controller session exit and trigger reconnect on next event.
	mockRunner.waitCh <- errors.New("session exited")
	select {
	case <-mockRunner.waitObserved:
	case <-time.After(1 * time.Second):
		t.Fatalf("timed out waiting for controller session exit to be observed")
	}

	if err := h.HandleEvent(ctx, env3); err != nil {
		t.Fatalf("handle event3 after stop: %v", err)
	}
	rpcServer.WaitForEvents(t, 3, 2*time.Second)
	if h.restartAttempts != 2 {
		t.Fatalf("restartAttempts after reconnect = %d, want 2", h.restartAttempts)
	}
	if mockRunner.startCount != 2 {
		t.Fatalf("startCount after reconnect = %d, want 2", mockRunner.startCount)
	}

	// Let close finish gracefully.
	mockRunner.waitCh <- nil
}

func TestInferControllerRole(t *testing.T) {
	t.Parallel()

	if got := inferControllerRole("ROLE: PM\nProduct manager"); got != "pm" {
		t.Fatalf("infer pm = %q", got)
	}
	if got := inferControllerRole("ROLE: DEV\nSoftware engineer"); got != "dev" {
		t.Fatalf("infer dev = %q", got)
	}
	if got := inferControllerRole("unknown"); got != "pm" {
		t.Fatalf("infer default = %q", got)
	}
	if got := inferControllerRole("---\nrole: dev\n---\nbody"); got != "dev" {
		t.Fatalf("infer frontmatter dev = %q", got)
	}
}

func TestBuildTickEvent(t *testing.T) {
	t.Parallel()

	at := time.Date(2026, 2, 10, 15, 4, 59, 0, time.UTC)
	env := buildTickEvent("holon-run/holon", at, 5*time.Minute)
	if env.Source != "timer" {
		t.Fatalf("source = %q", env.Source)
	}
	if env.Type != "timer.tick" {
		t.Fatalf("type = %q", env.Type)
	}
	if env.Scope.Repo != "holon-run/holon" {
		t.Fatalf("repo = %q", env.Scope.Repo)
	}
	if env.Subject.Kind != "timer" {
		t.Fatalf("subject kind = %q", env.Subject.Kind)
	}
	if env.Subject.ID != "1770735600" {
		t.Fatalf("subject id = %q", env.Subject.ID)
	}
	if env.DedupeKey != "timer:holon-run/holon:1770735600" {
		t.Fatalf("dedupe key = %q", env.DedupeKey)
	}
}

func TestLoadControllerRole(t *testing.T) {
	t.Parallel()

	agentHome := t.TempDir()
	rolePath := filepath.Join(agentHome, "ROLE.md")
	if err := os.WriteFile(rolePath, []byte("ROLE: DEV\n"), 0o644); err != nil {
		t.Fatalf("write role: %v", err)
	}
	roleLabel, err := loadControllerRole(agentHome)
	if err != nil {
		t.Fatalf("loadControllerRole() error: %v", err)
	}
	if roleLabel != "dev" {
		t.Fatalf("role label = %q, want dev", roleLabel)
	}
}

func TestLoadControllerRole_EmptyFile(t *testing.T) {
	t.Parallel()

	agentHome := t.TempDir()
	rolePath := filepath.Join(agentHome, "ROLE.md")
	if err := os.WriteFile(rolePath, []byte("   \n"), 0o644); err != nil {
		t.Fatalf("write role: %v", err)
	}
	if _, err := loadControllerRole(agentHome); err == nil {
		t.Fatalf("expected error for empty ROLE.md")
	}
}

func TestControllerPrompts_IncludeAgentHomeContract(t *testing.T) {
	t.Parallel()

	h := &cliControllerHandler{
		controllerRoleLabel: "pm",
	}
	systemPrompt, userPrompt, err := h.controllerPrompts()
	if err != nil {
		t.Fatalf("controllerPrompts() error: %v", err)
	}
	if !strings.Contains(systemPrompt, "HOLON_AGENT_HOME") {
		t.Fatalf("expected HOLON_AGENT_HOME contract, got: %q", systemPrompt)
	}
	if !strings.Contains(userPrompt, "HOLON_WORKSPACE_INDEX_PATH") {
		t.Fatalf("expected HOLON_WORKSPACE_INDEX_PATH contract, got: %q", userPrompt)
	}
	if !strings.Contains(userPrompt, "HOLON_CONTROLLER_RPC_SOCKET") {
		t.Fatalf("unexpected runtime user prompt: %q", userPrompt)
	}
}

func TestResolveControllerWorkspace(t *testing.T) {
	t.Parallel()

	agentHome := t.TempDir()
	got, err := resolveControllerWorkspace(agentHome)
	if err != nil {
		t.Fatalf("resolveControllerWorkspace() error: %v", err)
	}

	want := filepath.Join(agentHome, "workspace")
	if got != want {
		t.Fatalf("resolveControllerWorkspace() = %q, want %q", got, want)
	}
	info, err := os.Stat(got)
	if err != nil {
		t.Fatalf("stat workspace: %v", err)
	}
	if !info.IsDir() {
		t.Fatalf("workspace path is not a directory: %s", got)
	}
}

func TestWriteControllerSpecAndPrompts_ExcludesSkillsMetadata(t *testing.T) {
	t.Parallel()

	inputDir := t.TempDir()
	h := &cliControllerHandler{
		controllerRoleLabel: "pm",
	}

	if err := h.writeControllerSpecAndPrompts(inputDir); err != nil {
		t.Fatalf("writeControllerSpecAndPrompts() error: %v", err)
	}

	specPath := filepath.Join(inputDir, "spec.yaml")
	specData, err := os.ReadFile(specPath)
	if err != nil {
		t.Fatalf("read spec.yaml: %v", err)
	}
	spec := string(specData)
	if strings.Contains(spec, "skills:") {
		t.Fatalf("spec.yaml should not contain metadata.skills, got:\n%s", spec)
	}
	if !strings.Contains(spec, "name: \"github-controller-session\"") {
		t.Fatalf("spec.yaml missing expected metadata.name, got:\n%s", spec)
	}
}

func TestEnsureGoalStateFile(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	stateDir := filepath.Join(td, "controller-state")
	if err := os.MkdirAll(stateDir, 0o755); err != nil {
		t.Fatalf("mkdir controller-state: %v", err)
	}

	h := &cliControllerHandler{stateDir: td}
	if err := h.ensureGoalStateFile(); err != nil {
		t.Fatalf("ensureGoalStateFile() error: %v", err)
	}
	path := filepath.Join(stateDir, "goal-state.json")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read goal-state.json: %v", err)
	}
	var got map[string]any
	if err := json.Unmarshal(data, &got); err != nil {
		t.Fatalf("unmarshal goal-state.json: %v", err)
	}
	if got["version"] != float64(1) {
		t.Fatalf("version = %v", got["version"])
	}
}

func TestReadAnthropicEnvFromClaudeSettings(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	settingsPath := filepath.Join(td, "settings.json")
	if err := os.WriteFile(settingsPath, []byte(`{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "token-from-settings",
    "ANTHROPIC_BASE_URL": "https://example.ai",
    "OTHER": "ignored"
  }
}`), 0o644); err != nil {
		t.Fatalf("write settings: %v", err)
	}

	got, err := readAnthropicEnvFromClaudeSettings(settingsPath)
	if err != nil {
		t.Fatalf("readAnthropicEnvFromClaudeSettings() error: %v", err)
	}

	if got["ANTHROPIC_AUTH_TOKEN"] != "token-from-settings" {
		t.Fatalf("ANTHROPIC_AUTH_TOKEN = %q", got["ANTHROPIC_AUTH_TOKEN"])
	}
	if got["ANTHROPIC_BASE_URL"] != "https://example.ai" {
		t.Fatalf("ANTHROPIC_BASE_URL = %q", got["ANTHROPIC_BASE_URL"])
	}
	if _, ok := got["OTHER"]; ok {
		t.Fatalf("unexpected key OTHER in result")
	}
}

func TestResolveServeRuntimeEnv_PrefersProcessEnv(t *testing.T) {
	t.Setenv("ANTHROPIC_AUTH_TOKEN", "token-from-env")
	t.Setenv("ANTHROPIC_BASE_URL", "https://env.ai")

	got := resolveServeRuntimeEnv(context.Background())
	if got["ANTHROPIC_AUTH_TOKEN"] != "token-from-env" {
		t.Fatalf("ANTHROPIC_AUTH_TOKEN = %q", got["ANTHROPIC_AUTH_TOKEN"])
	}
	if got["ANTHROPIC_BASE_URL"] != "https://env.ai" {
		t.Fatalf("ANTHROPIC_BASE_URL = %q", got["ANTHROPIC_BASE_URL"])
	}
}

func TestResolveServeRuntimeEnv_MergesSettingsFallbackForMissingKeys(t *testing.T) {
	td := t.TempDir()
	claudeDir := filepath.Join(td, ".claude")
	if err := os.MkdirAll(claudeDir, 0o755); err != nil {
		t.Fatalf("mkdir .claude: %v", err)
	}
	settingsPath := filepath.Join(claudeDir, "settings.json")
	if err := os.WriteFile(settingsPath, []byte(`{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "token-from-settings",
    "ANTHROPIC_BASE_URL": "https://settings.ai"
  }
}`), 0o644); err != nil {
		t.Fatalf("write settings.json: %v", err)
	}

	t.Setenv("HOME", td)
	t.Setenv("ANTHROPIC_AUTH_TOKEN", "")
	t.Setenv("ANTHROPIC_BASE_URL", "https://env.ai")

	got := resolveServeRuntimeEnv(context.Background())
	if got["ANTHROPIC_BASE_URL"] != "https://env.ai" {
		t.Fatalf("ANTHROPIC_BASE_URL = %q, want env value", got["ANTHROPIC_BASE_URL"])
	}
	if got["ANTHROPIC_AUTH_TOKEN"] != "token-from-settings" {
		t.Fatalf("ANTHROPIC_AUTH_TOKEN = %q, want settings fallback value", got["ANTHROPIC_AUTH_TOKEN"])
	}
}

func TestResolveServeRuntimeEnv_InjectsGitHubToken(t *testing.T) {
	t.Setenv("GITHUB_TOKEN", "gh-token-from-env")
	t.Setenv("GH_TOKEN", "")

	got := resolveServeRuntimeEnv(context.Background())
	if got["GITHUB_TOKEN"] != "gh-token-from-env" {
		t.Fatalf("GITHUB_TOKEN = %q", got["GITHUB_TOKEN"])
	}
	if got["GH_TOKEN"] != "gh-token-from-env" {
		t.Fatalf("GH_TOKEN = %q", got["GH_TOKEN"])
	}
}

func TestResolveServeRuntimeEnv_PrefersHolonGitHubToken(t *testing.T) {
	t.Setenv("HOLON_GITHUB_TOKEN", "holon-token")
	t.Setenv("GITHUB_TOKEN", "actions-token")
	t.Setenv("GH_TOKEN", "")

	got := resolveServeRuntimeEnv(context.Background())
	if got["HOLON_GITHUB_TOKEN"] != "holon-token" {
		t.Fatalf("HOLON_GITHUB_TOKEN = %q", got["HOLON_GITHUB_TOKEN"])
	}
	if got["GITHUB_TOKEN"] != "holon-token" {
		t.Fatalf("GITHUB_TOKEN = %q", got["GITHUB_TOKEN"])
	}
	if got["GH_TOKEN"] != "holon-token" {
		t.Fatalf("GH_TOKEN = %q", got["GH_TOKEN"])
	}
}

type mockSessionRunner struct {
	mu           sync.Mutex
	startCount   int
	stopCount    int
	waitCh       chan error
	waitObserved chan struct{}
	lastConfig   ControllerSessionConfig
}

type mockControllerRPCServer struct {
	server *http.Server
	mu     sync.Mutex
	events []serve.EventEnvelope
}

func newMockControllerRPCServer(t *testing.T, socketPath string) *mockControllerRPCServer {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(socketPath), 0o755); err != nil {
		t.Fatalf("mkdir socket dir: %v", err)
	}
	_ = os.Remove(socketPath)

	s := &mockControllerRPCServer{}
	mux := http.NewServeMux()
	mux.HandleFunc("/health", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(map[string]string{"status": "ok"})
	})
	mux.HandleFunc("/v1/controller/events", func(w http.ResponseWriter, r *http.Request) {
		var req struct {
			Event serve.EventEnvelope `json:"event"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		s.mu.Lock()
		s.events = append(s.events, req.Event)
		s.mu.Unlock()
		_ = json.NewEncoder(w).Encode(map[string]any{
			"status":   "completed",
			"event_id": req.Event.ID,
		})
	})
	s.server = &http.Server{Handler: mux}
	listener, err := net.Listen("unix", socketPath)
	if err != nil {
		t.Fatalf("listen unix socket: %v", err)
	}

	go func() {
		_ = s.server.Serve(listener)
	}()
	t.Cleanup(func() {
		s.Close()
	})
	return s
}

func (s *mockControllerRPCServer) WaitForEvents(t *testing.T, want int, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		s.mu.Lock()
		got := len(s.events)
		s.mu.Unlock()
		if got >= want {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	s.mu.Lock()
	got := len(s.events)
	s.mu.Unlock()
	t.Fatalf("timed out waiting for %d events, got %d", want, got)
}

func (s *mockControllerRPCServer) Close() {
	if s.server != nil {
		_ = s.server.Close()
	}
}

func shortTempDir(t *testing.T, prefix string) string {
	t.Helper()
	dir, err := os.MkdirTemp(os.TempDir(), prefix+"-")
	if err != nil {
		t.Fatalf("mkdir temp dir: %v", err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func (m *mockSessionRunner) Start(_ context.Context, cfg ControllerSessionConfig) (*docker.SessionHandle, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.startCount++
	m.lastConfig = cfg
	return &docker.SessionHandle{ContainerID: "session-" + strconv.Itoa(m.startCount)}, nil
}

func (m *mockSessionRunner) Wait(_ context.Context, _ *docker.SessionHandle) error {
	err := <-m.waitCh
	select {
	case m.waitObserved <- struct{}{}:
	default:
	}
	return err
}

func (m *mockSessionRunner) Stop(_ context.Context, _ *docker.SessionHandle) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.stopCount++
	return nil
}

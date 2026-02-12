package main

import (
	"context"
	"encoding/json"
	"errors"
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

func TestAppendJSONLine(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	path := filepath.Join(td, "events.ndjson")

	first := map[string]any{"id": "evt-1", "type": "issue_comment"}
	second := map[string]any{"id": "evt-2", "type": "pull_request"}

	if err := appendJSONLine(path, first); err != nil {
		t.Fatalf("append first line: %v", err)
	}
	if err := appendJSONLine(path, second); err != nil {
		t.Fatalf("append second line: %v", err)
	}

	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read channel file: %v", err)
	}

	lines := bytesToLines(raw)
	if len(lines) != 2 {
		t.Fatalf("line count = %d, want 2", len(lines))
	}

	var gotFirst map[string]any
	if err := json.Unmarshal([]byte(lines[0]), &gotFirst); err != nil {
		t.Fatalf("unmarshal first line: %v", err)
	}
	if gotFirst["id"] != "evt-1" {
		t.Fatalf("first id = %v, want evt-1", gotFirst["id"])
	}

	var gotSecond map[string]any
	if err := json.Unmarshal([]byte(lines[1]), &gotSecond); err != nil {
		t.Fatalf("unmarshal second line: %v", err)
	}
	if gotSecond["id"] != "evt-2" {
		t.Fatalf("second id = %v, want evt-2", gotSecond["id"])
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

func TestCompactChannelBestEffortLocked(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	channelDir := filepath.Join(td, "controller-state")
	if err := os.MkdirAll(channelDir, 0o755); err != nil {
		t.Fatalf("mkdir controller-state: %v", err)
	}
	channelPath := filepath.Join(channelDir, "event-channel.ndjson")
	cursorPath := filepath.Join(channelDir, "event-channel.cursor")

	line1 := `{"id":"evt-1"}`
	line2 := `{"id":"evt-2"}`
	content := line1 + "\n" + line2 + "\n"
	if err := os.WriteFile(channelPath, []byte(content), 0o644); err != nil {
		t.Fatalf("write channel: %v", err)
	}
	cursor := len(line1) + 1
	if err := os.WriteFile(cursorPath, []byte(strconv.Itoa(cursor)), 0o644); err != nil {
		t.Fatalf("write cursor: %v", err)
	}

	h := &cliControllerHandler{
		stateDir:          td,
		controllerChannel: channelPath,
	}
	original := maxEventChannelSizeBytes
	maxEventChannelSizeBytes = 1
	defer func() {
		maxEventChannelSizeBytes = original
	}()

	h.compactChannelBestEffortLocked()

	gotChannel, err := os.ReadFile(channelPath)
	if err != nil {
		t.Fatalf("read channel after compact: %v", err)
	}
	if string(gotChannel) != line2+"\n" {
		t.Fatalf("channel after compact = %q, want %q", string(gotChannel), line2+"\n")
	}
	gotCursor, err := os.ReadFile(cursorPath)
	if err != nil {
		t.Fatalf("read cursor after compact: %v", err)
	}
	if string(gotCursor) != "0" {
		t.Fatalf("cursor after compact = %q, want 0", string(gotCursor))
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
	mockRunner := &mockSessionRunner{
		waitCh:       make(chan error, 2),
		waitObserved: make(chan struct{}, 2),
	}

	h := &cliControllerHandler{
		repoHint:            "holon-run/holon",
		stateDir:            td,
		agentHome:           t.TempDir(),
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
	if h.restartAttempts != 1 {
		t.Fatalf("restartAttempts after 2 events = %d, want 1", h.restartAttempts)
	}
	if mockRunner.startCount != 1 {
		t.Fatalf("startCount after 2 events = %d, want 1", mockRunner.startCount)
	}

	data, err := os.ReadFile(filepath.Join(td, "controller-state", "event-channel.ndjson"))
	if err != nil {
		t.Fatalf("read channel file: %v", err)
	}
	lines := bytesToLines(data)
	if len(lines) != 2 {
		t.Fatalf("channel line count = %d, want 2", len(lines))
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
	if !strings.Contains(userPrompt, "HOLON_CONTROLLER_GOAL_STATE_PATH") {
		t.Fatalf("unexpected runtime user prompt: %q", userPrompt)
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

func bytesToLines(raw []byte) []string {
	text := string(raw)
	if text == "" {
		return nil
	}
	parts := make([]string, 0, 4)
	start := 0
	for i := 0; i < len(text); i++ {
		if text[i] != '\n' {
			continue
		}
		if i > start {
			parts = append(parts, text[start:i])
		}
		start = i + 1
	}
	return parts
}

type mockSessionRunner struct {
	mu           sync.Mutex
	startCount   int
	stopCount    int
	waitCh       chan error
	waitObserved chan struct{}
}

func (m *mockSessionRunner) Start(_ context.Context, _ ControllerSessionConfig) (*docker.SessionHandle, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.startCount++
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

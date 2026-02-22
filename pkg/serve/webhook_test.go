package serve

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

type mockEventHandler struct {
	events []EventEnvelope
	errors []error
}

func (m *mockEventHandler) HandleEvent(_ context.Context, env EventEnvelope) error {
	m.events = append(m.events, env)
	if len(m.errors) > 0 {
		err := m.errors[0]
		m.errors = m.errors[1:]
		return err
	}
	return nil
}

type mockAckEventHandler struct {
	mockEventHandler
	acks chan TurnAckRecord
}

func (m *mockAckEventHandler) TurnAcks() <-chan TurnAckRecord {
	return m.acks
}

func TestWebhookServer_InvalidJSON(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Send invalid JSON
	req := httptest.NewRequest("POST", "/webhook", strings.NewReader("not json"))
	req.Header.Set("X-GitHub-Event", "push")
	req.Header.Set("X-GitHub-Delivery", "del-1")
	w := httptest.NewRecorder()
	ws.handleWebhook(w, req)

	// Should accept the request even with invalid JSON (wrapper handles it)
	if w.Code != http.StatusAccepted {
		t.Fatalf("expected StatusAccepted, got %d", w.Code)
	}

	// Wait a bit for event processing
	time.Sleep(100 * time.Millisecond)

	// The event should have been wrapped and queued
	// Invalid JSON is logged but doesn't reject the request
}

func TestWebhookServer_Deduplication(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	payload := map[string]interface{}{
		"action":     "created",
		"repository": map[string]interface{}{"full_name": "test/repo"},
		"issue":      map[string]interface{}{"number": 123},
		"comment":    map[string]interface{}{"id": 456},
	}
	body, _ := json.Marshal(payload)

	// Send first request
	req1 := httptest.NewRequest("POST", "/webhook", bytes.NewReader(body))
	req1.Header.Set("X-GitHub-Event", "issue_comment")
	req1.Header.Set("X-GitHub-Delivery", "del-1")
	w1 := httptest.NewRecorder()
	ws.handleWebhook(w1, req1)

	// Send duplicate request
	req2 := httptest.NewRequest("POST", "/webhook", bytes.NewReader(body))
	req2.Header.Set("X-GitHub-Event", "issue_comment")
	req2.Header.Set("X-GitHub-Delivery", "del-1")
	w2 := httptest.NewRecorder()
	ws.handleWebhook(w2, req2)

	// Process events
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	go ws.processEvents(ctx)

	// Wait for processing
	time.Sleep(500 * time.Millisecond)

	// Should only process one event (duplicate skipped)
	if len(handler.events) != 1 {
		t.Fatalf("expected 1 event after deduplication, got %d", len(handler.events))
	}
}

func TestWebhookServer_ProcessTurnAcks_FromSource(t *testing.T) {
	td := t.TempDir()
	handler := &mockAckEventHandler{
		acks: make(chan TurnAckRecord, 1),
	}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	turnID, _ := ws.runtime.beginTurn("thread-1")
	if _, ok := ws.runtime.loadTurn(turnID); !ok {
		t.Fatalf("expected turn to be active before ack")
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go ws.processTurnAcks(ctx)

	handler.acks <- TurnAckRecord{
		TurnID:  turnID,
		Status:  "running",
		Message: "still running",
	}
	time.Sleep(50 * time.Millisecond)
	if _, ok := ws.runtime.loadTurn(turnID); !ok {
		t.Fatalf("turn %s should remain active after non-terminal progress ack", turnID)
	}

	handler.acks <- TurnAckRecord{
		TurnID:  turnID,
		Status:  "completed",
		Message: "done",
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if _, ok := ws.runtime.loadTurn(turnID); !ok {
			return
		}
		time.Sleep(20 * time.Millisecond)
	}
	t.Fatalf("turn %s was not completed from ack source", turnID)
}

func TestWebhookServer_InjectEvent_UsesSameProcessingPipeline(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()
	ws.now = func() time.Time { return time.Date(2026, 2, 10, 0, 0, 0, 0, time.UTC) }

	env := EventEnvelope{
		Source: "timer",
		Type:   "timer.tick",
		Scope:  EventScope{Repo: "holon-run/holon"},
		Subject: EventSubject{
			Kind: "timer",
			ID:   "1739145600",
		},
		DedupeKey: "timer:holon-run/holon:1739145600",
	}
	if err := ws.InjectEvent(context.Background(), env); err != nil {
		t.Fatalf("InjectEvent first failed: %v", err)
	}
	if err := ws.InjectEvent(context.Background(), env); err != nil {
		t.Fatalf("InjectEvent second failed: %v", err)
	}
	if len(handler.events) != 1 {
		t.Fatalf("expected 1 event after dedupe, got %d", len(handler.events))
	}
}

func TestWebhookServer_InjectEvent_RespectsPauseState(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()
	if err := ws.runtime.Pause(); err != nil {
		t.Fatalf("Pause failed: %v", err)
	}

	env := EventEnvelope{
		Source: "timer",
		Type:   "timer.tick",
		Scope:  EventScope{Repo: "holon-run/holon"},
		Subject: EventSubject{
			Kind: "timer",
			ID:   "1739145600",
		},
	}
	if err := ws.InjectEvent(context.Background(), env); err != nil {
		t.Fatalf("InjectEvent failed: %v", err)
	}
	if len(handler.events) != 0 {
		t.Fatalf("expected paused runtime to skip injected events, got %d", len(handler.events))
	}
}

func TestWebhookServer_ChannelFull(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:           8080,
		StateDir:       td,
		Handler:        handler,
		ChannelTimeout: 100 * time.Millisecond,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Fill the channel by sending many requests rapidly
	payload := map[string]interface{}{
		"action":     "created",
		"repository": map[string]interface{}{"full_name": "test/repo"},
		"issue":      map[string]interface{}{"number": 123},
	}
	body, _ := json.Marshal(payload)

	busyCount := 0
	acceptedCount := 0

	// Send more requests than channel capacity (100)
	for i := 0; i < 150; i++ {
		req := httptest.NewRequest("POST", "/webhook", bytes.NewReader(body))
		req.Header.Set("X-GitHub-Event", "issue_comment")
		req.Header.Set("X-GitHub-Delivery", string(rune(i)))
		w := httptest.NewRecorder()
		ws.handleWebhook(w, req)

		if w.Code == http.StatusServiceUnavailable {
			busyCount++
		} else if w.Code == http.StatusAccepted {
			acceptedCount++
		}
	}

	// At least some requests should have been accepted
	if acceptedCount == 0 {
		t.Fatalf("expected some requests to be accepted, got %d", acceptedCount)
	}

	// Some requests should have gotten busy response
	if busyCount == 0 {
		t.Log("Warning: no requests got busy response (channel might not have filled)")
	}
}

func TestWebhookServer_BodySizeLimit(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	maxSize := int64(1024) // 1KB
	ws, err := NewWebhookServer(WebhookConfig{
		Port:        8080,
		StateDir:    td,
		Handler:     handler,
		MaxBodySize: maxSize,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Send body larger than limit
	largeBody := make([]byte, maxSize+100)
	for i := range largeBody {
		largeBody[i] = 'x'
	}

	req := httptest.NewRequest("POST", "/webhook", bytes.NewReader(largeBody))
	req.Header.Set("X-GitHub-Event", "push")
	w := httptest.NewRecorder()
	ws.handleWebhook(w, req)

	// Should reject with error
	if w.Code != http.StatusBadRequest && w.Code != http.StatusServiceUnavailable {
		t.Fatalf("expected rejection for oversized body, got status %d", w.Code)
	}
}

func TestWebhookServer_HTTPErrorPaths(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	tests := []struct {
		name       string
		method     string
		body       string
		wantStatus int
	}{
		{
			name:       "non-POST method",
			method:     "GET",
			body:       "{}",
			wantStatus: http.StatusMethodNotAllowed,
		},
		{
			name:       "empty body",
			method:     "POST",
			body:       "",
			wantStatus: http.StatusBadRequest,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := httptest.NewRequest(tt.method, "/webhook", strings.NewReader(tt.body))
			w := httptest.NewRecorder()
			ws.handleWebhook(w, req)

			if w.Code != tt.wantStatus {
				t.Fatalf("expected status %d, got %d", tt.wantStatus, w.Code)
			}
		})
	}
}

func TestWebhookServer_StatePersistence(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}

	payload := map[string]interface{}{
		"action":     "created",
		"repository": map[string]interface{}{"full_name": "test/repo"},
		"issue":      map[string]interface{}{"number": 123},
		"comment":    map[string]interface{}{"id": 456},
	}
	body, _ := json.Marshal(payload)

	req := httptest.NewRequest("POST", "/webhook", bytes.NewReader(body))
	req.Header.Set("X-GitHub-Event", "issue_comment")
	req.Header.Set("X-GitHub-Delivery", "del-1")
	w := httptest.NewRecorder()
	ws.handleWebhook(w, req)

	// Process event
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	go ws.processEvents(ctx)

	time.Sleep(200 * time.Millisecond)
	ws.Close()

	// Check state file exists
	statePath := filepath.Join(td, "serve-state.json")
	data, err := os.ReadFile(statePath)
	if err != nil {
		t.Fatalf("failed to read state file: %v", err)
	}

	var state persistentState
	if err := json.Unmarshal(data, &state); err != nil {
		t.Fatalf("failed to parse state: %v", err)
	}

	if state.LastEventID == "" {
		t.Fatalf("expected LastEventID to be set, got empty string")
	}
}

func TestWebhookServer_RestartLoadsStateAndDedupes(t *testing.T) {
	td := t.TempDir()
	dedupeKey := "issue_comment:test/repo:123"

	firstHandler := &mockEventHandler{}
	firstServer, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  firstHandler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer(first) failed: %v", err)
	}
	firstServer.now = func() time.Time { return time.Date(2026, 2, 14, 9, 0, 0, 0, time.UTC) }

	firstEvent := EventEnvelope{
		ID:        "evt-first",
		Source:    "github",
		Type:      "github.issue_comment.created",
		DedupeKey: dedupeKey,
		Scope: EventScope{
			Repo: "test/repo",
		},
		Subject: EventSubject{
			Kind: "issue",
			ID:   "123",
		},
	}
	if err := firstServer.InjectEvent(context.Background(), firstEvent); err != nil {
		t.Fatalf("InjectEvent(first) failed: %v", err)
	}
	if len(firstHandler.events) != 1 {
		t.Fatalf("expected first server to process one event, got %d", len(firstHandler.events))
	}
	if err := firstServer.Close(); err != nil {
		t.Fatalf("Close(first) failed: %v", err)
	}

	secondHandler := &mockEventHandler{}
	secondServer, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  secondHandler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer(second) failed: %v", err)
	}
	defer secondServer.Close()
	secondServer.now = func() time.Time { return time.Date(2026, 2, 14, 10, 0, 0, 0, time.UTC) }

	duplicateEvent := EventEnvelope{
		ID:        "evt-second",
		Source:    "github",
		Type:      "github.issue_comment.created",
		DedupeKey: dedupeKey,
		Scope: EventScope{
			Repo: "test/repo",
		},
		Subject: EventSubject{
			Kind: "issue",
			ID:   "123",
		},
	}
	if err := secondServer.InjectEvent(context.Background(), duplicateEvent); err != nil {
		t.Fatalf("InjectEvent(second) failed: %v", err)
	}
	if len(secondHandler.events) != 0 {
		t.Fatalf("expected duplicate event to be skipped after restart, got %d handled events", len(secondHandler.events))
	}

	statePath := filepath.Join(td, "serve-state.json")
	data, err := os.ReadFile(statePath)
	if err != nil {
		t.Fatalf("failed to read state file: %v", err)
	}

	var state persistentState
	if err := json.Unmarshal(data, &state); err != nil {
		t.Fatalf("failed to parse state: %v", err)
	}
	if _, ok := state.ProcessedAt[dedupeKey]; !ok {
		t.Fatalf("expected dedupe key %q to be present in persisted state", dedupeKey)
	}
	if state.LastEventID != "evt-second" {
		t.Fatalf("expected LastEventID to advance on duplicate event, got %q", state.LastEventID)
	}
}

func TestWebhookServer_Timeouts(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	readTimeout := 1 * time.Second
	writeTimeout := 1 * time.Second
	idleTimeout := 5 * time.Second

	ws, err := NewWebhookServer(WebhookConfig{
		Port:         8080,
		StateDir:     td,
		Handler:      handler,
		ReadTimeout:  readTimeout,
		WriteTimeout: writeTimeout,
		IdleTimeout:  idleTimeout,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Check that server has timeouts configured
	if ws.server.ReadTimeout != readTimeout {
		t.Fatalf("expected ReadTimeout %v, got %v", readTimeout, ws.server.ReadTimeout)
	}
	if ws.server.WriteTimeout != writeTimeout {
		t.Fatalf("expected WriteTimeout %v, got %v", writeTimeout, ws.server.WriteTimeout)
	}
	if ws.server.IdleTimeout != idleTimeout {
		t.Fatalf("expected IdleTimeout %v, got %v", idleTimeout, ws.server.IdleTimeout)
	}
}

func TestWebhookServer_DefaultTimeouts(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}

	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
		// No timeouts specified - should use defaults
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Check default timeouts are set
	if ws.server.ReadTimeout == 0 {
		t.Fatal("expected default ReadTimeout to be set")
	}
	if ws.server.WriteTimeout == 0 {
		t.Fatal("expected default WriteTimeout to be set")
	}
	if ws.server.IdleTimeout == 0 {
		t.Fatal("expected default IdleTimeout to be set")
	}

	if ws.server.ReadTimeout != 10*time.Second {
		t.Fatalf("expected default ReadTimeout 10s, got %v", ws.server.ReadTimeout)
	}
	if ws.server.WriteTimeout != 10*time.Second {
		t.Fatalf("expected default WriteTimeout 10s, got %v", ws.server.WriteTimeout)
	}
	if ws.server.IdleTimeout != 60*time.Second {
		t.Fatalf("expected default IdleTimeout 60s, got %v", ws.server.IdleTimeout)
	}
}

func TestWebhookServer_HealthEndpoint(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	req := httptest.NewRequest("GET", "/health", nil)
	w := httptest.NewRecorder()
	ws.handleHealth(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status OK, got %d", w.Code)
	}

	ct := w.Header().Get("Content-Type")
	if ct != "application/json" {
		t.Fatalf("expected content-type application/json, got %s", ct)
	}

	var resp map[string]string
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp["status"] != "ok" {
		t.Fatalf("expected status ok, got %s", resp["status"])
	}
	if resp["time"] == "" {
		t.Fatal("expected time field to be set")
	}
}

func TestWebhookServer_ConfigValidation(t *testing.T) {
	td := t.TempDir()

	tests := []struct {
		name    string
		config  WebhookConfig
		wantErr bool
	}{
		{
			name: "valid config",
			config: WebhookConfig{
				Port:     8080,
				StateDir: td,
				Handler:  &mockEventHandler{},
			},
			wantErr: false,
		},
		{
			name: "missing handler",
			config: WebhookConfig{
				Port:     8080,
				StateDir: td,
				Handler:  nil,
			},
			wantErr: true,
		},
		{
			name: "missing state dir",
			config: WebhookConfig{
				Port:     8080,
				StateDir: "",
				Handler:  &mockEventHandler{},
			},
			wantErr: true,
		},
		{
			name: "invalid port - too low",
			config: WebhookConfig{
				Port:     0,
				StateDir: td,
				Handler:  &mockEventHandler{},
			},
			wantErr: true,
		},
		{
			name: "invalid port - too high",
			config: WebhookConfig{
				Port:     70000,
				StateDir: td,
				Handler:  &mockEventHandler{},
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := NewWebhookServer(tt.config)
			if (err != nil) != tt.wantErr {
				t.Fatalf("NewWebhookServer() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

func TestWebhookServer_ChannelTimeoutBehavior(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	channelTimeout := 50 * time.Millisecond

	ws, err := NewWebhookServer(WebhookConfig{
		Port:           8080,
		StateDir:       td,
		Handler:        handler,
		ChannelTimeout: channelTimeout,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Check channel timeout is set
	if ws.channelTimeout != channelTimeout {
		t.Fatalf("expected channelTimeout %v, got %v", channelTimeout, ws.channelTimeout)
	}

	payload := map[string]interface{}{
		"action":     "created",
		"repository": map[string]interface{}{"full_name": "test/repo"},
		"issue":      map[string]interface{}{"number": 123},
	}
	body, _ := json.Marshal(payload)

	// Fill the channel to trigger timeout behavior
	for i := 0; i < 150; i++ {
		req := httptest.NewRequest("POST", "/webhook", bytes.NewReader(body))
		req.Header.Set("X-GitHub-Event", "issue_comment")
		req.Header.Set("X-GitHub-Delivery", string(rune(i)))
		w := httptest.NewRecorder()

		start := time.Now()
		ws.handleWebhook(w, req)
		elapsed := time.Since(start)

		// Request should return quickly (within channel timeout + small buffer)
		if elapsed > channelTimeout+500*time.Millisecond {
			t.Fatalf("request took too long: %v, expected < %v", elapsed, channelTimeout+500*time.Millisecond)
		}
	}
}

func TestWebhookServer_CloseCleanup(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}

	// Close should not error on first call
	if err := ws.Close(); err != nil {
		t.Fatalf("Close() failed: %v", err)
	}

	// Note: Calling Close() multiple times may return an error from closing
	// already-closed files, which is expected behavior for the ndjsonWriter
	// The important thing is that resources are cleaned up on first call
}

func TestWebhookServer_WrapWithHeaders_InvalidJSON(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Test with invalid JSON
	invalidBody := []byte("not json")
	headers := map[string]string{
		"x_github_event": "push",
	}

	wrapped := ws.wrapWithHeaders(invalidBody, headers)

	// Should return original body when JSON parsing fails
	if !bytes.Equal(wrapped, invalidBody) {
		t.Fatalf("expected original body to be returned for invalid JSON, got %s", wrapped)
	}
}

func TestWebhookServer_MaxBodySizeDefault(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}

	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
		// MaxBodySize not specified - should use default
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Check default max body size is set (10MB)
	expectedSize := int64(10 * 1024 * 1024)
	if ws.maxBodySize != expectedSize {
		t.Fatalf("expected default MaxBodySize %d, got %d", expectedSize, ws.maxBodySize)
	}
}

func TestWebhookServer_ReadBodyError(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Create a reader that fails on read
	failReader := &failingReader{}

	req := httptest.NewRequest("POST", "/webhook", failReader)
	req.Header.Set("X-GitHub-Event", "push")
	w := httptest.NewRecorder()
	ws.handleWebhook(w, req)

	// Should return bad request
	if w.Code != http.StatusBadRequest {
		t.Fatalf("expected status BadRequest on read error, got %d", w.Code)
	}
}

// failingReader is an io.Reader that always fails
type failingReader struct{}

func (f *failingReader) Read(p []byte) (n int, err error) {
	return 0, io.ErrClosedPipe
}

// JSON-RPC Handler Tests

func TestWebhookServer_JSONRPC_ValidRequest(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Register a test method
	ws.rpcRegistry.RegisterMethod("test.method", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		return map[string]string{"result": "success"}, nil
	})

	// Create valid JSON-RPC request
	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "test.method",
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	// Should return OK with JSON-RPC response
	if w.Code != http.StatusOK {
		t.Fatalf("expected status OK, got %d", w.Code)
	}

	ct := w.Header().Get("Content-Type")
	if ct != "application/json" {
		t.Fatalf("expected content-type application/json, got %s", ct)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.JSONRPC != "2.0" {
		t.Errorf("expected jsonrpc version 2.0, got %s", resp.JSONRPC)
	}

	if resp.Error != nil {
		t.Fatalf("unexpected error: %s", resp.Error.Message)
	}

	var result map[string]string
	if err := json.Unmarshal(resp.Result, &result); err != nil {
		t.Fatalf("failed to unmarshal result: %v", err)
	}

	if result["result"] != "success" {
		t.Errorf("expected result 'success', got %s", result["result"])
	}
}

func TestWebhookServer_JSONRPC_InvalidJSON(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	req := httptest.NewRequest("POST", "/rpc", strings.NewReader("not json"))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	// Should return BadRequest with JSON-RPC error response
	if w.Code != http.StatusBadRequest {
		t.Fatalf("expected status BadRequest, got %d", w.Code)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Error == nil {
		t.Fatal("expected error response, got nil")
	}

	if resp.Error.Code != ErrCodeParseError {
		t.Errorf("expected error code %d, got %d", ErrCodeParseError, resp.Error.Code)
	}
}

func TestWebhookServer_JSONRPC_MissingJSONRPCVersion(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	requestBody := map[string]interface{}{
		"id":     1,
		"method": "test.method",
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	if w.Code != http.StatusBadRequest {
		t.Fatalf("expected status BadRequest, got %d", w.Code)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Error == nil {
		t.Fatal("expected error response, got nil")
	}

	if resp.Error.Code != ErrCodeInvalidRequest {
		t.Errorf("expected error code %d, got %d", ErrCodeInvalidRequest, resp.Error.Code)
	}
}

func TestWebhookServer_JSONRPC_MethodNotFound(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "unknown.method",
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status OK, got %d", w.Code)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Error == nil {
		t.Fatal("expected error response, got nil")
	}

	if resp.Error.Code != ErrCodeMethodNotFound {
		t.Errorf("expected error code %d, got %d", ErrCodeMethodNotFound, resp.Error.Code)
	}
}

func TestWebhookServer_JSONRPC_InvalidMethod(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		// Missing method field
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	if w.Code != http.StatusBadRequest {
		t.Fatalf("expected status BadRequest, got %d", w.Code)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Error == nil {
		t.Fatal("expected error response, got nil")
	}

	if resp.Error.Code != ErrCodeInvalidRequest {
		t.Errorf("expected error code %d, got %d", ErrCodeInvalidRequest, resp.Error.Code)
	}
}

func TestWebhookServer_JSONRPC_NonPostMethod(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	req := httptest.NewRequest("GET", "/rpc", nil)
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	if w.Code != http.StatusMethodNotAllowed {
		t.Fatalf("expected status MethodNotAllowed, got %d", w.Code)
	}
}

func TestWebhookServer_JSONRPC_EmptyBody(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader([]byte("")))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	if w.Code != http.StatusBadRequest {
		t.Fatalf("expected status BadRequest, got %d", w.Code)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Error == nil {
		t.Fatal("expected error response, got nil")
	}

	if resp.Error.Code != ErrCodeInvalidRequest {
		t.Errorf("expected error code %d, got %d", ErrCodeInvalidRequest, resp.Error.Code)
	}
}

func TestWebhookServer_JSONRPC_HandlerReturnsError(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Register a method that returns an error
	ws.rpcRegistry.RegisterMethod("failing.method", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		return nil, NewJSONRPCError(ErrCodeInvalidParams, "invalid parameters")
	})

	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "failing.method",
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status OK, got %d", w.Code)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Error == nil {
		t.Fatal("expected error response, got nil")
	}

	if resp.Error.Code != ErrCodeInvalidParams {
		t.Errorf("expected error code %d, got %d", ErrCodeInvalidParams, resp.Error.Code)
	}

	if resp.Error.Message != "invalid parameters" {
		t.Errorf("expected error message 'invalid parameters', got %s", resp.Error.Message)
	}
}

func TestWebhookServer_JSONRPC_WithParams(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Register a method that uses params
	ws.rpcRegistry.RegisterMethod("echo.params", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		var data map[string]interface{}
		if err := json.Unmarshal(params, &data); err != nil {
			return nil, NewJSONRPCError(ErrCodeInvalidParams, "invalid params")
		}
		return data, nil
	})

	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "echo.params",
		"params":  map[string]string{"key": "value"},
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("expected status OK, got %d", w.Code)
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	if resp.Error != nil {
		t.Fatalf("unexpected error: %s", resp.Error.Message)
	}

	var result map[string]interface{}
	if err := json.Unmarshal(resp.Result, &result); err != nil {
		t.Fatalf("failed to unmarshal result: %v", err)
	}

	if result["key"] != "value" {
		t.Errorf("expected key 'value', got %v", result["key"])
	}
}

// TestWebhookServer_JSONRPC_NotificationNoResponse tests that notifications (requests without id) return 204 No Content
func TestWebhookServer_JSONRPC_NotificationNoResponse(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Register a test method
	methodCalled := false
	ws.rpcRegistry.RegisterMethod("test.notify", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		methodCalled = true
		return map[string]string{"result": "success"}, nil
	})

	// Create JSON-RPC notification (no id field)
	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"method":  "test.notify",
		"params":  map[string]string{"key": "value"},
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	// Should return No Content (204) for notifications
	if w.Code != http.StatusNoContent {
		t.Fatalf("expected status No Content (204), got %d", w.Code)
	}

	// Verify method was still called
	if !methodCalled {
		t.Error("notification handler was not called")
	}

	// Verify response body is empty
	if w.Body.Len() != 0 {
		t.Errorf("expected empty response body for notification, got %d bytes", w.Body.Len())
	}
}

// TestWebhookServer_JSONRPC_NotificationWithError tests that notifications with errors also return 204
func TestWebhookServer_JSONRPC_NotificationWithError(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Register a method that returns an error
	ws.rpcRegistry.RegisterMethod("failing.notify", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		return nil, NewJSONRPCError(ErrCodeInvalidParams, "invalid parameters")
	})

	// Create JSON-RPC notification (no id field)
	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"method":  "failing.notify",
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	// Should return No Content (204) even for errors in notifications
	if w.Code != http.StatusNoContent {
		t.Fatalf("expected status No Content (204) for notification error, got %d", w.Code)
	}

	// Verify response body is empty
	if w.Body.Len() != 0 {
		t.Errorf("expected empty response body for notification with error, got %d bytes", w.Body.Len())
	}
}

// TestWebhookServer_JSONRPC_WithIDReturnsResponse tests that regular requests with id return normal response
func TestWebhookServer_JSONRPC_WithIDReturnsResponse(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	// Register a test method
	ws.rpcRegistry.RegisterMethod("test.method", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		return map[string]string{"result": "success"}, nil
	})

	// Create JSON-RPC request WITH id
	requestBody := map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      "test-id-123",
		"method":  "test.method",
	}
	body, _ := json.Marshal(requestBody)

	req := httptest.NewRequest("POST", "/rpc", bytes.NewReader(body))
	w := httptest.NewRecorder()
	ws.handleJSONRPC(w, req)

	// Should return OK with response
	if w.Code != http.StatusOK {
		t.Fatalf("expected status OK, got %d", w.Code)
	}

	// Verify response body is NOT empty
	if w.Body.Len() == 0 {
		t.Error("expected non-empty response body for request with id")
	}

	var resp JSONRPCResponse
	if err := json.NewDecoder(w.Body).Decode(&resp); err != nil {
		t.Fatalf("failed to decode response: %v", err)
	}

	// Verify id is echoed back
	if resp.ID != "test-id-123" {
		t.Errorf("expected id 'test-id-123', got %v", resp.ID)
	}

	if resp.Error != nil {
		t.Fatalf("unexpected error: %s", resp.Error.Message)
	}
}

func TestWebhookServer_RPCStream_RequiresAcceptHeader(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()

	req := httptest.NewRequest("GET", "/rpc/stream", nil)
	w := httptest.NewRecorder()
	ws.handleRPCStream(w, req)
	if w.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 when Accept header missing, got %d", w.Code)
	}
}

func TestAcceptsNDJSON(t *testing.T) {
	tests := []struct {
		header string
		want   bool
	}{
		{header: "", want: false},
		{header: "application/x-ndjson", want: true},
		{header: "application/x-ndjson; charset=utf-8", want: true},
		{header: "application/x-ndjson, application/json;q=0.9", want: true},
		{header: "*/*", want: true},
		{header: "application/json", want: false},
	}
	for _, tc := range tests {
		if got := acceptsNDJSON(tc.header); got != tc.want {
			t.Fatalf("acceptsNDJSON(%q)=%v, want %v", tc.header, got, tc.want)
		}
	}
}

func TestWebhookServer_RPCStream_ReceivesTurnEvents(t *testing.T) {
	td := t.TempDir()
	handler := &mockEventHandler{}
	ws, err := NewWebhookServer(WebhookConfig{
		Port:     8080,
		StateDir: td,
		Handler:  handler,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer ws.Close()
	ws.runtime.setTurnIdleTTLForTest(80 * time.Millisecond)

	ctx, cancel := context.WithCancel(context.Background())
	req := httptest.NewRequest("GET", "/rpc/stream", nil).WithContext(ctx)
	req.Header.Set("Accept", "application/x-ndjson")
	w := httptest.NewRecorder()

	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		ws.handleRPCStream(w, req)
	}()

	time.Sleep(50 * time.Millisecond)

	threadReqBody, _ := json.Marshal(map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      1,
		"method":  "thread/start",
		"params":  map[string]interface{}{},
	})
	threadReq := httptest.NewRequest("POST", "/rpc", bytes.NewReader(threadReqBody))
	threadResp := httptest.NewRecorder()
	ws.handleJSONRPC(threadResp, threadReq)
	if threadResp.Code != http.StatusOK {
		t.Fatalf("thread/start expected 200, got %d", threadResp.Code)
	}
	var threadRPCResp JSONRPCResponse
	if err := json.Unmarshal(threadResp.Body.Bytes(), &threadRPCResp); err != nil {
		t.Fatalf("failed to decode thread/start response: %v", err)
	}
	var threadStartResp ThreadStartResponse
	if err := json.Unmarshal(threadRPCResp.Result, &threadStartResp); err != nil {
		t.Fatalf("failed to decode thread/start result: %v", err)
	}

	turnReqBody, _ := json.Marshal(map[string]interface{}{
		"jsonrpc": "2.0",
		"id":      2,
		"method":  "turn/start",
		"params": map[string]interface{}{
			"thread_id": threadStartResp.ThreadID,
			"input": []map[string]interface{}{
				{
					"type": "message",
					"role": "user",
					"content": []map[string]interface{}{
						{"type": "input_text", "text": "hello"},
					},
				},
			},
		},
	})
	turnReq := httptest.NewRequest("POST", "/rpc", bytes.NewReader(turnReqBody))
	turnResp := httptest.NewRecorder()
	ws.handleJSONRPC(turnResp, turnReq)
	if turnResp.Code != http.StatusOK {
		t.Fatalf("turn/start expected 200, got %d", turnResp.Code)
	}

	deadline := time.Now().Add(5 * time.Second)
	for {
		output := w.Body.String()
		if strings.Contains(output, "turn/completed") {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for turn/completed in stream output; got: %s", output)
		}
		time.Sleep(10 * time.Millisecond)
	}

	cancel()
	wg.Wait()

	output := w.Body.String()
	if !strings.Contains(output, "thread/started") {
		t.Fatalf("expected thread/started in stream output, got: %s", output)
	}
	if !strings.Contains(output, "turn/started") {
		t.Fatalf("expected turn/started in stream output, got: %s", output)
	}
	if !strings.Contains(output, "item/created") {
		t.Fatalf("expected item/created in stream output, got: %s", output)
	}
	if !strings.Contains(output, "turn/completed") {
		t.Fatalf("expected turn/completed in stream output, got: %s", output)
	}
}

package serve

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

// WebhookServer handles incoming GitHub webhook HTTP requests
type WebhookServer struct {
	server         *http.Server
	eventChan      chan []byte
	handler        EventHandler
	repoHint       string
	statePath      string
	eventsLog      *ndjsonWriter
	decLog         *ndjsonWriter
	actionsLog     *ndjsonWriter
	state          persistentState
	runtime        *Runtime
	now            func() time.Time
	mu             sync.RWMutex
	maxBodySize    int64
	channelTimeout time.Duration
	rpcRegistry    *MethodRegistry
	broadcaster    *NotificationBroadcaster
	port           int
}

// WebhookConfig configures the webhook server
type WebhookConfig struct {
	Port           int
	RepoHint       string
	StateDir       string
	Handler        EventHandler
	TurnDispatcher TurnDispatcher
	ReadTimeout    time.Duration
	WriteTimeout   time.Duration
	IdleTimeout    time.Duration
	MaxBodySize    int64
	ChannelTimeout time.Duration
}

// NewWebhookServer creates a new webhook server for GitHub events
func NewWebhookServer(cfg WebhookConfig) (*WebhookServer, error) {
	if cfg.Handler == nil {
		return nil, fmt.Errorf("event handler is required")
	}
	if cfg.StateDir == "" {
		return nil, fmt.Errorf("state dir is required")
	}
	if cfg.Port <= 0 || cfg.Port > 65535 {
		return nil, fmt.Errorf("invalid port: %d", cfg.Port)
	}
	if err := os.MkdirAll(cfg.StateDir, 0755); err != nil {
		return nil, err
	}

	// Set default timeouts if not specified
	readTimeout := cfg.ReadTimeout
	if readTimeout == 0 {
		readTimeout = 10 * time.Second
	}
	writeTimeout := cfg.WriteTimeout
	if writeTimeout == 0 {
		writeTimeout = 10 * time.Second
	}
	idleTimeout := cfg.IdleTimeout
	if idleTimeout == 0 {
		idleTimeout = 60 * time.Second
	}

	// Set default max body size (10MB)
	maxBodySize := cfg.MaxBodySize
	if maxBodySize == 0 {
		maxBodySize = 10 * 1024 * 1024
	}

	// Set default channel timeout (5 seconds)
	channelTimeout := cfg.ChannelTimeout
	if channelTimeout == 0 {
		channelTimeout = 5 * time.Second
	}

	eventsLog, err := newNDJSONWriter(filepath.Join(cfg.StateDir, "events.ndjson"))
	if err != nil {
		return nil, err
	}
	decLog, err := newNDJSONWriter(filepath.Join(cfg.StateDir, "decisions.ndjson"))
	if err != nil {
		eventsLog.Close()
		return nil, err
	}
	actionsLog, err := newNDJSONWriter(filepath.Join(cfg.StateDir, "actions.ndjson"))
	if err != nil {
		eventsLog.Close()
		decLog.Close()
		return nil, err
	}

	// Initialize runtime state manager
	runtime, err := NewRuntime(cfg.StateDir)
	if err != nil {
		eventsLog.Close()
		decLog.Close()
		actionsLog.Close()
		return nil, fmt.Errorf("failed to initialize runtime: %w", err)
	}

	ws := &WebhookServer{
		eventChan:      make(chan []byte, 100),
		handler:        cfg.Handler,
		repoHint:       cfg.RepoHint,
		statePath:      filepath.Join(cfg.StateDir, "serve-state.json"),
		eventsLog:      eventsLog,
		decLog:         decLog,
		actionsLog:     actionsLog,
		runtime:        runtime,
		now:            time.Now,
		maxBodySize:    maxBodySize,
		channelTimeout: channelTimeout,
		rpcRegistry:    NewMethodRegistry(),
		broadcaster:    NewNotificationBroadcaster(),
		port:           cfg.Port,
		state: persistentState{
			ProcessedAt:  make(map[string]string),
			ProcessedMax: 2000,
		},
	}
	ws.runtime.SetBroadcaster(ws.broadcaster)
	ws.runtime.SetTurnDispatcher(cfg.TurnDispatcher)

	if err := ws.loadState(); err != nil {
		eventsLog.Close()
		decLog.Close()
		actionsLog.Close()
		return nil, err
	}

	// Register holon control methods
	ws.rpcRegistry.RegisterMethod("holon/status", ws.runtime.HandleStatus)
	ws.rpcRegistry.RegisterMethod("holon/pause", ws.runtime.HandlePause)
	ws.rpcRegistry.RegisterMethod("holon/resume", ws.runtime.HandleResume)
	ws.rpcRegistry.RegisterMethod("holon/logStream", ws.runtime.HandleLogStream(filepath.Join(cfg.StateDir, "events.ndjson")))

	// Register Codex-compatible session/turn methods
	ws.rpcRegistry.RegisterMethod("thread/start", ws.runtime.HandleThreadStart)
	ws.rpcRegistry.RegisterMethod("turn/start", ws.runtime.HandleTurnStart)
	ws.rpcRegistry.RegisterMethod("turn/steer", ws.runtime.HandleTurnSteer)
	ws.rpcRegistry.RegisterMethod("turn/interrupt", ws.runtime.HandleTurnInterrupt)

	mux := http.NewServeMux()
	// JSON-RPC control plane
	mux.HandleFunc("/rpc", ws.handleJSONRPC)
	mux.HandleFunc("/rpc/stream", ws.handleRPCStream)
	// Provider-specific ingress path (new)
	mux.HandleFunc("/ingress/github/webhook", ws.handleWebhook)
	// Legacy path (deprecated for backward compatibility)
	mux.HandleFunc("/webhook", ws.handleLegacyWebhook)
	mux.HandleFunc("/health", ws.handleHealth)

	ws.server = &http.Server{
		Addr:         fmt.Sprintf(":%d", cfg.Port),
		Handler:      mux,
		ReadTimeout:  readTimeout,
		WriteTimeout: writeTimeout,
		IdleTimeout:  idleTimeout,
	}

	return ws, nil
}

// Start begins accepting webhook requests
func (ws *WebhookServer) Start(ctx context.Context) error {
	holonlog.Info("webhook server listening", "port", ws.server.Addr, "path", "/ingress/github/webhook")

	// Start event processor in background
	go ws.processEvents(ctx)

	// Start HTTP server
	errChan := make(chan error, 1)
	go func() {
		if err := ws.server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			errChan <- fmt.Errorf("webhook server failed: %w", err)
		}
	}()

	// Wait for context cancellation or server error
	select {
	case <-ctx.Done():
		holonlog.Info("shutting down webhook server")
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if err := ws.server.Shutdown(shutdownCtx); err != nil {
			holonlog.Error("webhook server shutdown error", "error", err)
			return fmt.Errorf("server shutdown failed: %w", err)
		}
		holonlog.Info("webhook server shutdown complete")
		return ctx.Err()
	case err := <-errChan:
		return err
	}
}

// Close stops the webhook server and closes log files
func (ws *WebhookServer) Close() error {
	var firstErr error
	for _, w := range []*ndjsonWriter{ws.eventsLog, ws.decLog, ws.actionsLog} {
		if w == nil {
			continue
		}
		if err := w.Close(); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	return firstErr
}

// Port returns the port the webhook server is listening on
func (ws *WebhookServer) Port() int {
	return ws.port
}

func (ws *WebhookServer) handleWebhook(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	// Limit body size to prevent memory exhaustion
	limitedReader := http.MaxBytesReader(w, r.Body, ws.maxBodySize)

	// Read body
	body, err := io.ReadAll(limitedReader)
	limitedReader.Close()
	if err != nil {
		holonlog.Error("failed to read webhook body", "error", err)
		http.Error(w, "failed to read body", http.StatusBadRequest)
		return
	}

	if len(body) == 0 {
		http.Error(w, "empty body", http.StatusBadRequest)
		return
	}

	// Extract GitHub headers for normalization
	headers := map[string]string{}
	if ghEvent := r.Header.Get("X-GitHub-Event"); ghEvent != "" {
		headers["x_github_event"] = ghEvent
	}
	if ghDelivery := r.Header.Get("X-GitHub-Delivery"); ghDelivery != "" {
		headers["x_github_delivery"] = ghDelivery
	}

	// Wrap payload with headers for normalization
	wrapped := ws.wrapWithHeaders(body, headers)

	// Send to event channel with timeout to prevent indefinite blocking
	select {
	case ws.eventChan <- wrapped:
		w.WriteHeader(http.StatusAccepted)
		holonlog.Debug("webhook accepted", "event", headers["x_github_event"], "delivery", headers["x_github_delivery"])
	case <-time.After(ws.channelTimeout):
		holonlog.Warn("webhook channel timeout after waiting, dropping event", "event", headers["x_github_event"], "timeout", ws.channelTimeout)
		http.Error(w, "server busy", http.StatusServiceUnavailable)
		return
	}
}

// handleLegacyWebhook handles requests to the old /webhook path with deprecation warning
func (ws *WebhookServer) handleLegacyWebhook(w http.ResponseWriter, r *http.Request) {
	holonlog.Warn("webhook legacy path accessed", "path", r.URL.Path, "deprecated_path", "/webhook", "new_path", "/ingress/github/webhook")
	// Forward to the new handler
	ws.handleWebhook(w, r)
}

func (ws *WebhookServer) handleHealth(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	json.NewEncoder(w).Encode(map[string]string{
		"status": "ok",
		"time":   ws.now().UTC().Format(time.RFC3339Nano),
	})
}

// handleJSONRPC handles JSON-RPC 2.0 requests on /rpc
func (ws *WebhookServer) handleJSONRPC(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	// Limit body size to prevent memory exhaustion
	r.Body = http.MaxBytesReader(w, r.Body, ws.maxBodySize)

	// Read and parse JSON-RPC request
	req, rpcErr := ReadJSONRPCRequest(r)
	if rpcErr != nil {
		// Parse errors should return HTTP 400 with error response
		w.WriteHeader(http.StatusBadRequest)
		WriteJSONRPCResponse(w, nil, nil, rpcErr)
		holonlog.Error("jsonrpc parse error", "error", rpcErr.Message, "code", rpcErr.Code)
		return
	}

	// Check if this is a notification (no id field)
	// JSON-RPC 2.0 spec: notifications must not receive a response
	isNotification := (req.ID == nil)

	// Dispatch to method handler
	result, rpcErr := ws.rpcRegistry.Dispatch(req.Method, req.Params)
	if rpcErr != nil {
		// Don't send response for notifications
		if isNotification {
			w.WriteHeader(http.StatusNoContent)
			holonlog.Debug("jsonrpc notification error", "method", req.Method, "error", rpcErr.Message, "code", rpcErr.Code)
			return
		}
		WriteJSONRPCResponse(w, req.ID, nil, rpcErr)
		holonlog.Error("jsonrpc method error", "method", req.Method, "error", rpcErr.Message, "code", rpcErr.Code)
		return
	}

	// Don't send response for notifications
	if isNotification {
		w.WriteHeader(http.StatusNoContent)
		holonlog.Debug("jsonrpc notification success", "method", req.Method)
		return
	}

	WriteJSONRPCResponse(w, req.ID, result, nil)
	holonlog.Debug("jsonrpc success", "id", req.ID, "method", req.Method)
}

func (ws *WebhookServer) handleRPCStream(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	if !acceptsNDJSON(r.Header.Get("Accept")) {
		http.Error(w, "accept header must be application/x-ndjson", http.StatusBadRequest)
		return
	}

	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming not supported", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/x-ndjson")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.WriteHeader(http.StatusOK)
	flusher.Flush()

	streamWriter := NewStreamWriter(w)
	defer streamWriter.Close()
	unsubscribe := ws.broadcaster.Subscribe(streamWriter)
	defer unsubscribe()

	threadID := ws.runtime.GetState().ControllerSession
	if threadID != "" {
		threadNotif := NewThreadNotification(threadID, ThreadNotificationStarted, StateRunning)
		if err := streamWriter.WriteThreadNotification(threadNotif); err == nil {
			flusher.Flush()
		}
	}

	// Keep-alive must be shorter than server WriteTimeout (default 10s),
	// otherwise long-lived stream responses can be closed by net/http.
	ticker := time.NewTicker(3 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-r.Context().Done():
			return
		case <-ticker.C:
			// Keep-alive: blank line is valid NDJSON separator and keeps intermediaries from timing out.
			if _, err := w.Write([]byte("\n")); err != nil {
				return
			}
			flusher.Flush()
		}
	}
}

func acceptsNDJSON(acceptHeader string) bool {
	if strings.TrimSpace(acceptHeader) == "" {
		return false
	}
	for _, token := range strings.Split(acceptHeader, ",") {
		mediaType, _, err := mime.ParseMediaType(strings.TrimSpace(token))
		if err != nil {
			continue
		}
		if mediaType == "application/x-ndjson" || mediaType == "*/*" {
			return true
		}
	}
	return false
}

func (ws *WebhookServer) wrapWithHeaders(body []byte, headers map[string]string) []byte {
	// Parse the original body
	var payload map[string]interface{}
	if err := json.Unmarshal(body, &payload); err != nil {
		holonlog.Error("failed to parse webhook body", "error", err)
		// Return body as-is if parsing fails
		return body
	}

	// Add headers to payload
	for k, v := range headers {
		payload[k] = v
	}

	// Re-serialize with headers
	wrapped, err := json.Marshal(payload)
	if err != nil {
		holonlog.Error("failed to marshal wrapped webhook", "error", err)
		return body
	}
	return wrapped
}

func (ws *WebhookServer) processEvents(ctx context.Context) {
	for {
		select {
		case <-ctx.Done():
			return
		case raw := <-ws.eventChan:
			if err := ws.processOne(ctx, raw); err != nil {
				holonlog.Error("failed to process event", "error", err)
			}
		}
	}
}

func (ws *WebhookServer) processOne(ctx context.Context, raw []byte) error {
	// Check if runtime is paused
	if ws.runtime.IsPaused() {
		holonlog.Info("event skipped: runtime paused", "reason", "paused")
		return nil
	}

	// Normalize event
	env, err := normalizeLine(raw, ws.repoHint, ws.now)
	if err != nil {
		return fmt.Errorf("failed to normalize event: %w", err)
	}
	return ws.processEnvelope(ctx, env)
}

// InjectEvent allows internal producers (e.g. timer source) to route events
// through the same webhook processing pipeline.
func (ws *WebhookServer) InjectEvent(ctx context.Context, env EventEnvelope) error {
	if ctx == nil {
		ctx = context.Background()
	}
	if ws.runtime.IsPaused() {
		holonlog.Info("event skipped: runtime paused", "reason", "paused")
		return nil
	}
	if env.ID == "" {
		env.ID = newID("evt", ws.now().UTC())
	}
	if env.At.IsZero() {
		env.At = ws.now().UTC()
	}
	if env.Scope.Repo == "" {
		env.Scope.Repo = ws.repoHint
	}
	if env.DedupeKey == "" {
		env.DedupeKey = buildDedupeKey(env)
	}
	return ws.processEnvelope(ctx, env)
}

func (ws *WebhookServer) processEnvelope(ctx context.Context, env EventEnvelope) error {

	// Write to events log
	if err := ws.eventsLog.Write(env); err != nil {
		return err
	}

	// Check for duplicates
	decision := DecisionRecord{
		ID:        newID("decision", ws.now().UTC()),
		EventID:   env.ID,
		Type:      "forward_event",
		CreatedAt: ws.now().UTC(),
	}
	if env.DedupeKey != "" {
		ws.mu.RLock()
		_, exists := ws.state.ProcessedAt[env.DedupeKey]
		ws.mu.RUnlock()

		if exists {
			decision.Skipped = true
			decision.Reason = "duplicate dedupe_key"
			if err := ws.decLog.Write(decision); err != nil {
				return err
			}
			// Advance cursor state even for duplicates
			if err := ws.updateCursor(env); err != nil {
				holonlog.Error("failed to update cursor for duplicate event", "error", err, "event_id", env.ID)
			}
			return nil
		}
	}

	if err := ws.decLog.Write(decision); err != nil {
		return err
	}

	// Handle event with request context
	start := ws.now().UTC()
	result := ActionResult{
		ID:        newID("actres", start),
		EventID:   env.ID,
		StartedAt: start,
		EndedAt:   start,
	}

	// Create a context for event handling with timeout
	handlerCtx := ctx
	if handlerCtx == nil {
		handlerCtx = context.Background()
	}
	handlerCtx, cancel := context.WithTimeout(handlerCtx, 5*time.Minute)
	defer cancel()

	if err := ws.handler.HandleEvent(handlerCtx, env); err != nil {
		if IsSkipEventError(err) {
			result.Status = "skipped"
			result.Message = err.Error()
		} else {
			result.Status = "failed"
			result.Message = err.Error()
		}
	} else {
		result.Status = "ok"
		// Record successful event in runtime
		ws.runtime.RecordEvent(env.ID)
	}
	result.EndedAt = ws.now().UTC()

	if err := ws.actionsLog.Write(result); err != nil {
		return err
	}

	if err := ws.updateCursor(env); err != nil {
		holonlog.Error("failed to update cursor after event processing", "error", err, "event_id", env.ID)
	}
	return nil
}

func (ws *WebhookServer) updateCursor(env EventEnvelope) error {
	ws.mu.Lock()
	defer ws.mu.Unlock()

	if env.DedupeKey != "" {
		ws.state.ProcessedAt[env.DedupeKey] = ws.now().UTC().Format(time.RFC3339Nano)
	}
	ws.state.LastEventID = env.ID

	// Compact state if needed
	if len(ws.state.ProcessedAt) > ws.state.ProcessedMax {
		ws.compactStateLocked()
	}

	// Save state
	if err := ws.saveStateLocked(); err != nil {
		return fmt.Errorf("failed to save state: %w", err)
	}
	return nil
}

func (ws *WebhookServer) loadState() error {
	ws.mu.Lock()
	defer ws.mu.Unlock()

	data, err := os.ReadFile(ws.statePath)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			ws.state = persistentState{ProcessedAt: make(map[string]string), ProcessedMax: 2000}
			return nil
		}
		return fmt.Errorf("failed to read serve state: %w", err)
	}

	if err := json.Unmarshal(data, &ws.state); err != nil {
		return fmt.Errorf("failed to parse serve state: %w", err)
	}

	if ws.state.ProcessedAt == nil {
		ws.state.ProcessedAt = make(map[string]string)
	}
	if ws.state.ProcessedMax <= 0 {
		ws.state.ProcessedMax = 2000
	}
	return nil
}

func (ws *WebhookServer) saveStateLocked() error {
	data, err := json.MarshalIndent(ws.state, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal serve state: %w", err)
	}
	if err := os.WriteFile(ws.statePath, data, 0644); err != nil {
		return fmt.Errorf("failed to write serve state: %w", err)
	}
	return nil
}

func (ws *WebhookServer) compactStateLocked() {
	// Keep newest ProcessedMax entries
	type stateItem struct {
		key string
		at  time.Time
	}

	items := make([]stateItem, 0, len(ws.state.ProcessedAt))
	for k, v := range ws.state.ProcessedAt {
		t, err := time.Parse(time.RFC3339Nano, v)
		if err != nil {
			t = time.Time{}
		}
		items = append(items, stateItem{key: k, at: t})
	}

	// Sort by time descending (newest first)
	sort.Slice(items, func(i, j int) bool {
		return items[i].at.After(items[j].at)
	})

	// Delete old entries
	for idx := ws.state.ProcessedMax; idx < len(items); idx++ {
		delete(ws.state.ProcessedAt, items[idx].key)
	}

	holonlog.Info("compacted serve state", "entries", len(ws.state.ProcessedAt))
}

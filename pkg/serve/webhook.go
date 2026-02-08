package serve

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

// WebhookServer handles incoming GitHub webhook HTTP requests
type WebhookServer struct {
	server     *http.Server
	eventChan  chan []byte
	handler    EventHandler
	repoHint   string
	statePath  string
	eventsLog  *ndjsonWriter
	decLog     *ndjsonWriter
	actionsLog *ndjsonWriter
	state      persistentState
	now        func() time.Time
	mu         sync.RWMutex
}

// WebhookConfig configures the webhook server
type WebhookConfig struct {
	Port       int
	RepoHint   string
	StateDir   string
	Handler    EventHandler
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
	if err := ensureStateDir(cfg.StateDir); err != nil {
		return nil, err
	}

	eventsLog, err := newNDJSONWriter(joinPath(cfg.StateDir, "events.ndjson"))
	if err != nil {
		return nil, err
	}
	decLog, err := newNDJSONWriter(joinPath(cfg.StateDir, "decisions.ndjson"))
	if err != nil {
		eventsLog.Close()
		return nil, err
	}
	actionsLog, err := newNDJSONWriter(joinPath(cfg.StateDir, "actions.ndjson"))
	if err != nil {
		eventsLog.Close()
		decLog.Close()
		return nil, err
	}

	ws := &WebhookServer{
		eventChan:  make(chan []byte, 100),
		handler:    cfg.Handler,
		repoHint:   cfg.RepoHint,
		statePath:  joinPath(cfg.StateDir, "serve-state.json"),
		eventsLog:  eventsLog,
		decLog:     decLog,
		actionsLog: actionsLog,
		now:        time.Now,
		state: persistentState{
			ProcessedAt: make(map[string]string),
			ProcessedMax: 2000,
		},
	}

	if err := ws.loadState(); err != nil {
		eventsLog.Close()
		decLog.Close()
		actionsLog.Close()
		return nil, err
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/webhook", ws.handleWebhook)
	mux.HandleFunc("/health", ws.handleHealth)

	ws.server = &http.Server{
		Addr:    fmt.Sprintf(":%d", cfg.Port),
		Handler: mux,
	}

	return ws, nil
}

// Start begins accepting webhook requests
func (ws *WebhookServer) Start(ctx context.Context) error {
	holonlog.Info("webhook server listening", "port", ws.server.Addr, "path", "/webhook")

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
		_ = ws.server.Shutdown(shutdownCtx)
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

func (ws *WebhookServer) handleWebhook(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}

	// Read body
	body, err := io.ReadAll(r.Body)
	r.Body.Close()
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

	// Send to event channel (non-blocking)
	select {
	case ws.eventChan <- wrapped:
		w.WriteHeader(http.StatusAccepted)
		holonlog.Debug("webhook accepted", "event", headers["x_github_event"], "delivery", headers["x_github_delivery"])
	default:
		holonlog.Warn("webhook channel full, dropping event", "event", headers["x_github_event"])
		http.Error(w, "server busy", http.StatusServiceUnavailable)
		return
	}
}

func (ws *WebhookServer) handleHealth(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	json.NewEncoder(w).Encode(map[string]string{
		"status": "ok",
		"time":   ws.now().UTC().Format(time.RFC3339Nano),
	})
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
			if err := ws.processOne(raw); err != nil {
				holonlog.Error("failed to process event", "error", err)
			}
		}
	}
}

func (ws *WebhookServer) processOne(raw []byte) error {
	// Normalize event
	env, err := normalizeLine(raw, ws.repoHint, ws.now)
	if err != nil {
		return fmt.Errorf("failed to normalize event: %w", err)
	}

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
			ws.updateCursor(env)
			return nil
		}
	}

	if err := ws.decLog.Write(decision); err != nil {
		return err
	}

	// Handle event
	start := ws.now().UTC()
	result := ActionResult{
		ID:        newID("actres", start),
		EventID:   env.ID,
		StartedAt: start,
		EndedAt:   start,
	}

	if err := ws.handler.HandleEvent(context.Background(), env); err != nil {
		if IsSkipEventError(err) {
			result.Status = "skipped"
			result.Message = err.Error()
		} else {
			result.Status = "failed"
			result.Message = err.Error()
		}
	} else {
		result.Status = "ok"
	}
	result.EndedAt = ws.now().UTC()

	if err := ws.actionsLog.Write(result); err != nil {
		return err
	}

	ws.updateCursor(env)
	return nil
}

func (ws *WebhookServer) updateCursor(env EventEnvelope) {
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
	ws.saveStateLocked()
}

func (ws *WebhookServer) loadState() error {
	ws.mu.Lock()
	defer ws.mu.Unlock()

	data, err := readFile(ws.statePath)
	if err != nil {
		if isNotExist(err) {
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
	if err := writeFile(ws.statePath, data, 0644); err != nil {
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

// Helper functions
func ensureStateDir(dir string) error {
	return os.MkdirAll(dir, 0755)
}

func joinPath(dir, file string) string {
	return filepath.Join(dir, file)
}

func readFile(path string) ([]byte, error) {
	return os.ReadFile(path)
}

func writeFile(path string, data []byte, perm int) error {
	return os.WriteFile(path, data, os.FileMode(perm))
}

func isNotExist(err error) bool {
	return errors.Is(err, os.ErrNotExist)
}

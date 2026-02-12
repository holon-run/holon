package serve

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sync"
)

// StreamWriter handles writing notifications to a streaming connection
type StreamWriter struct {
	mu      sync.Mutex
	writer  io.Writer
	enc     *json.Encoder
	flusher interface{ Flush() }
	closed  bool
}

// NewStreamWriter creates a new stream writer for NDJSON streaming
func NewStreamWriter(w io.Writer) *StreamWriter {
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	return &StreamWriter{
		writer: w,
		enc:    enc,
		flusher: func() interface{ Flush() } {
			if f, ok := w.(interface{ Flush() }); ok {
				return f
			}
			return nil
		}(),
		closed: false,
	}
}

// WriteNotification writes a notification to the stream
func (sw *StreamWriter) WriteNotification(n Notification) error {
	sw.mu.Lock()
	defer sw.mu.Unlock()

	if sw.closed {
		return fmt.Errorf("stream writer is closed")
	}

	if err := sw.enc.Encode(n); err != nil {
		return fmt.Errorf("failed to encode notification: %w", err)
	}
	if sw.flusher != nil {
		sw.flusher.Flush()
	}

	return nil
}

// WriteItemNotification writes an item notification to the stream
func (sw *StreamWriter) WriteItemNotification(n ItemNotification) error {
	rpcNotif, err := n.ToJSONRPCNotification()
	if err != nil {
		return err
	}
	return sw.WriteNotification(rpcNotif)
}

// WriteTurnNotification writes a turn notification to the stream
func (sw *StreamWriter) WriteTurnNotification(n TurnNotification) error {
	rpcNotif, err := n.ToJSONRPCNotification()
	if err != nil {
		return err
	}
	return sw.WriteNotification(rpcNotif)
}

// WriteThreadNotification writes a thread notification to the stream
func (sw *StreamWriter) WriteThreadNotification(n ThreadNotification) error {
	rpcNotif, err := n.ToJSONRPCNotification()
	if err != nil {
		return err
	}
	return sw.WriteNotification(rpcNotif)
}

// Close closes the stream writer
func (sw *StreamWriter) Close() error {
	sw.mu.Lock()
	defer sw.mu.Unlock()
	sw.closed = true
	return nil
}

// StreamHandler handles bidirectional streaming for JSON-RPC
type StreamHandler struct {
	runtime *Runtime
}

// NewStreamHandler creates a new stream handler
func NewStreamHandler(runtime *Runtime) *StreamHandler {
	return &StreamHandler{
		runtime: runtime,
	}
}

// HandleStream processes a streaming connection
// This implements server-sent events style streaming over NDJSON
func (sh *StreamHandler) HandleStream(ctx context.Context, w http.ResponseWriter, r *http.Request) error {
	// Set headers for streaming
	w.Header().Set("Content-Type", "application/x-ndjson")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")

	// Ensure the client supports streaming
	if r.Header.Get("Accept") != "application/x-ndjson" {
		return fmt.Errorf("client must accept application/x-ndjson")
	}

	// Create stream writer
	streamWriter := NewStreamWriter(w)
	defer streamWriter.Close()

	// Send initial thread started notification
	sessionID := sh.runtime.GetState().ControllerSession
	if sessionID == "" {
		sessionID = fmt.Sprintf("thread_%d", sh.runtime.GetState().LastEventAt.Unix())
	}

	threadNotif := NewThreadNotification(sessionID, ThreadNotificationStarted, StateRunning)
	if err := streamWriter.WriteThreadNotification(threadNotif); err != nil {
		return fmt.Errorf("failed to send thread notification: %w", err)
	}

	// Handle incoming requests from client (bidirectional streaming)
	scanner := bufio.NewScanner(r.Body)
	for scanner.Scan() {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		line := scanner.Text()
		if line == "" {
			continue
		}

		// Parse incoming JSON-RPC request
		var req JSONRPCRequest
		if err := json.Unmarshal([]byte(line), &req); err != nil {
			// Send error notification
			errorResp := JSONRPCResponse{
				JSONRPC: "2.0",
				ID:      nil,
				Error:   NewJSONRPCError(ErrCodeParseError, "parse error"),
			}
			data, _ := json.Marshal(errorResp)
			w.Write(append(data, '\n'))
			continue
		}

		// Process request and send response
		sh.handleRequest(&req, streamWriter)
	}

	if err := scanner.Err(); err != nil {
		return fmt.Errorf("error reading stream: %w", err)
	}

	return nil
}

// handleRequest processes a single JSON-RPC request in the stream
func (sh *StreamHandler) handleRequest(req *JSONRPCRequest, writer *StreamWriter) {
	// Create method registry
	registry := NewMethodRegistry()

	// Register control methods
	registry.RegisterMethod("holon/status", sh.runtime.HandleStatus)
	registry.RegisterMethod("holon/pause", sh.runtime.HandlePause)
	registry.RegisterMethod("holon/resume", sh.runtime.HandleResume)
	// Note: holon/logStream is not available in streaming mode
	// Use the HTTP /rpc endpoint for log streaming with proper log path configuration

	// Register session/turn methods
	registry.RegisterMethod("thread/start", sh.runtime.HandleThreadStart)
	registry.RegisterMethod("turn/start", sh.runtime.HandleTurnStart)
	registry.RegisterMethod("turn/steer", sh.runtime.HandleTurnSteer)
	registry.RegisterMethod("turn/interrupt", sh.runtime.HandleTurnInterrupt)

	// Dispatch request
	result, rpcErr := registry.Dispatch(req.Method, req.Params)

	// Check if this is a notification (no id field)
	// JSON-RPC 2.0 spec: notifications must not receive a response
	isNotification := (req.ID == nil)

	// Don't send response for notifications
	if isNotification {
		return
	}

	// Send response as a notification (server-sent style)
	resp := JSONRPCResponse{
		JSONRPC: "2.0",
		ID:      req.ID,
	}

	if rpcErr != nil {
		resp.Error = rpcErr
	} else {
		rawResult, err := json.Marshal(result)
		if err != nil {
			resp.Error = NewJSONRPCError(ErrCodeInternalError, "failed to marshal result")
		} else {
			resp.Result = json.RawMessage(rawResult)
		}
	}

	// Write response as NDJSON
	data, _ := json.Marshal(resp)
	writer.mu.Lock()
	writer.writer.Write(append(data, '\n'))
	writer.mu.Unlock()
}

// NotificationBroadcaster broadcasts notifications to multiple subscribers
type NotificationBroadcaster struct {
	mu          sync.RWMutex
	subscribers map[*StreamWriter]struct{}
}

// NewNotificationBroadcaster creates a new notification broadcaster
func NewNotificationBroadcaster() *NotificationBroadcaster {
	return &NotificationBroadcaster{
		subscribers: make(map[*StreamWriter]struct{}),
	}
}

// Subscribe adds a new subscriber to receive notifications
func (nb *NotificationBroadcaster) Subscribe(sw *StreamWriter) func() {
	nb.mu.Lock()
	defer nb.mu.Unlock()

	nb.subscribers[sw] = struct{}{}

	// Return unsubscribe function
	return func() {
		nb.Unsubscribe(sw)
	}
}

// Unsubscribe removes a subscriber
func (nb *NotificationBroadcaster) Unsubscribe(sw *StreamWriter) {
	nb.mu.Lock()
	defer nb.mu.Unlock()

	delete(nb.subscribers, sw)
}

func (nb *NotificationBroadcaster) snapshotSubscribers() []*StreamWriter {
	nb.mu.RLock()
	defer nb.mu.RUnlock()

	out := make([]*StreamWriter, 0, len(nb.subscribers))
	for sw := range nb.subscribers {
		out = append(out, sw)
	}
	return out
}

func (nb *NotificationBroadcaster) broadcast(rpcNotif Notification) {
	for _, sw := range nb.snapshotSubscribers() {
		if err := sw.WriteNotification(rpcNotif); err != nil {
			nb.Unsubscribe(sw)
		}
	}
}

// BroadcastItemNotification broadcasts an item notification to all subscribers
func (nb *NotificationBroadcaster) BroadcastItemNotification(n ItemNotification) {
	rpcNotif, err := n.ToJSONRPCNotification()
	if err != nil {
		return
	}
	nb.broadcast(rpcNotif)
}

// BroadcastTurnNotification broadcasts a turn notification to all subscribers
func (nb *NotificationBroadcaster) BroadcastTurnNotification(n TurnNotification) {
	rpcNotif, err := n.ToJSONRPCNotification()
	if err != nil {
		return
	}
	nb.broadcast(rpcNotif)
}

// BroadcastThreadNotification broadcasts a thread notification to all subscribers
func (nb *NotificationBroadcaster) BroadcastThreadNotification(n ThreadNotification) {
	rpcNotif, err := n.ToJSONRPCNotification()
	if err != nil {
		return
	}
	nb.broadcast(rpcNotif)
}

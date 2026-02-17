package tui

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"path"
	"strings"
	"sync/atomic"
	"time"
)

// RPCClient is a JSON-RPC client for the TUI
type RPCClient struct {
	rpcURL    string
	client    *http.Client
	streamCli *http.Client
	requestID atomic.Int64
}

// NewRPCClient creates a new RPC client
func NewRPCClient(rpcURL string) *RPCClient {
	c := &RPCClient{
		rpcURL: rpcURL,
		client: &http.Client{
			Timeout: 10 * time.Second,
		},
		streamCli: &http.Client{
			Timeout: 0,
		},
	}
	c.requestID.Store(0)
	return c
}

// SetRPCTimeout updates JSON-RPC request timeout for non-stream calls.
func (c *RPCClient) SetRPCTimeout(timeout time.Duration) {
	if timeout <= 0 {
		return
	}
	c.client.Timeout = timeout
}

// StatusResponse is the response for holon/status
type StatusResponse struct {
	State           string    `json:"state"`
	EventsProcessed int64     `json:"events_processed"`
	LastEventAt     time.Time `json:"last_event_at"`
	SessionID       string    `json:"session_id"`
	PausedAt        time.Time `json:"paused_at,omitempty"`
	ResumedAt       time.Time `json:"resumed_at,omitempty"`
}

// PauseResponse is the response for holon/pause
type PauseResponse struct {
	Success bool   `json:"success"`
	Message string `json:"message"`
}

// ResumeResponse is the response for holon/resume
type ResumeResponse struct {
	Success bool   `json:"success"`
	Message string `json:"message"`
}

// LogEntry represents a single log line
type LogEntry struct {
	Level   string    `json:"level"`
	Time    time.Time `json:"time"`
	Message string    `json:"message"`
}

// LogStreamResponse is the response for holon/logStream
type LogStreamResponse struct {
	StreamID string     `json:"stream_id"`
	Logs     []LogEntry `json:"logs"`
}

// jsonrpcRequest represents a JSON-RPC 2.0 request
type jsonrpcRequest struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      int             `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
}

// jsonrpcResponse represents a JSON-RPC 2.0 response
type jsonrpcResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      interface{}     `json:"id"` // Can be int, string, or null for notifications
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *rpcError       `json:"error,omitempty"`
	Method  string          `json:"method,omitempty"` // For notifications
	Params  json.RawMessage `json:"params,omitempty"` // For notifications
}

// rpcError represents a JSON-RPC error
type rpcError struct {
	Code    int             `json:"code"`
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data,omitempty"`
}

// StreamNotification represents one JSON-RPC notification from /rpc/stream.
type StreamNotification struct {
	JSONRPC string          `json:"jsonrpc"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
}

// NotificationHandler is called for each decoded stream notification.
type NotificationHandler func(StreamNotification)

// call makes a JSON-RPC call
func (c *RPCClient) call(method string, params interface{}, result interface{}) error {
	requestID := int(c.requestID.Add(1))

	var paramsJSON json.RawMessage
	if params != nil {
		var err error
		paramsJSON, err = json.Marshal(params)
		if err != nil {
			return fmt.Errorf("failed to marshal params: %w", err)
		}
	} else {
		paramsJSON = []byte("null")
	}

	reqBody := jsonrpcRequest{
		JSONRPC: "2.0",
		ID:      requestID,
		Method:  method,
		Params:  paramsJSON,
	}

	bodyBytes, err := json.Marshal(reqBody)
	if err != nil {
		return fmt.Errorf("failed to marshal request: %w", err)
	}

	resp, err := c.client.Post(c.rpcURL, "application/json", bytes.NewReader(bodyBytes))
	if err != nil {
		return fmt.Errorf("rpc call failed: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("failed to read response: %w", err)
	}

	var rpcResp jsonrpcResponse
	if err := json.Unmarshal(respBody, &rpcResp); err != nil {
		return fmt.Errorf("failed to unmarshal response: %w", err)
	}

	if rpcResp.Error != nil {
		return fmt.Errorf("rpc error (code %d): %s", rpcResp.Error.Code, rpcResp.Error.Message)
	}

	if result != nil {
		if err := json.Unmarshal(rpcResp.Result, result); err != nil {
			return fmt.Errorf("failed to unmarshal result: %w", err)
		}
	}

	return nil
}

// GetStatus retrieves the current runtime status
func (c *RPCClient) GetStatus() (*StatusResponse, error) {
	var resp StatusResponse
	if err := c.call("holon/status", nil, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// Pause pauses the runtime
func (c *RPCClient) Pause() (*PauseResponse, error) {
	var resp PauseResponse
	if err := c.call("holon/pause", nil, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// Resume resumes the runtime
func (c *RPCClient) Resume() (*ResumeResponse, error) {
	var resp ResumeResponse
	if err := c.call("holon/resume", nil, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// GetLogs retrieves logs from the server
func (c *RPCClient) GetLogs(maxLines int) (*LogStreamResponse, error) {
	params := map[string]interface{}{
		"max_lines": maxLines,
	}
	var resp LogStreamResponse
	if err := c.call("holon/logStream", params, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// TestConnection tests if the RPC endpoint is reachable
func (c *RPCClient) TestConnection() error {
	_, err := c.GetStatus()
	return err
}

// StreamNotifications consumes JSON-RPC notifications from the server stream endpoint.
func (c *RPCClient) StreamNotifications(ctx context.Context, handler NotificationHandler) error {
	streamURL, err := c.streamURL()
	if err != nil {
		return err
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, streamURL, nil)
	if err != nil {
		return fmt.Errorf("failed to create stream request: %w", err)
	}
	req.Header.Set("Accept", "application/x-ndjson")

	resp, err := c.streamCli.Do(req)
	if err != nil {
		return fmt.Errorf("stream request failed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("stream request returned status %d", resp.StatusCode)
	}

	scanner := bufio.NewScanner(resp.Body)
	scanner.Buffer(make([]byte, 0, 64*1024), 256*1024)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}

		var msg jsonrpcResponse
		if err := json.Unmarshal([]byte(line), &msg); err != nil {
			return fmt.Errorf("failed to decode stream message: %w", err)
		}

		// Stream can include responses and notifications; only forward notifications.
		if msg.Method == "" {
			continue
		}

		handler(StreamNotification{
			JSONRPC: msg.JSONRPC,
			Method:  msg.Method,
			Params:  msg.Params,
		})
	}

	if err := scanner.Err(); err != nil {
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			return nil
		}
		return fmt.Errorf("stream read failed: %w", err)
	}

	return nil
}

func (c *RPCClient) streamURL() (string, error) {
	u, err := url.Parse(c.rpcURL)
	if err != nil {
		return "", fmt.Errorf("invalid rpc url: %w", err)
	}

	switch {
	case strings.HasSuffix(u.Path, "/rpc"):
		u.Path = strings.TrimSuffix(u.Path, "/rpc") + "/rpc/stream"
	case u.Path == "" || u.Path == "/":
		u.Path = "/rpc/stream"
	default:
		u.Path = path.Join(u.Path, "stream")
	}

	return u.String(), nil
}

// TurnStartRequest represents a request to start a new turn
type TurnStartRequest struct {
	ThreadID string             `json:"thread_id,omitempty"`
	Input    []TurnInputMessage `json:"input,omitempty"`
}

// TurnInputMessage represents a user message
type TurnInputMessage struct {
	Type    string                 `json:"type,omitempty"`
	Role    string                 `json:"role,omitempty"`
	Content []TurnInputContentPart `json:"content,omitempty"`
}

// TurnInputContentPart represents content in a message
type TurnInputContentPart struct {
	Type string `json:"type,omitempty"`
	Text string `json:"text,omitempty"`
}

// TurnStartResponse is the response for turn/start
type TurnStartResponse struct {
	TurnID    string `json:"turn_id"`
	State     string `json:"state"`
	StartedAt string `json:"started_at"`
}

// StartTurn starts a new turn with a user message
func (c *RPCClient) StartTurn(threadID string, message string) (*TurnStartResponse, error) {
	input := []TurnInputMessage{{
		Type: "message",
		Role: "user",
		Content: []TurnInputContentPart{{
			Type: "input_text",
			Text: message,
		}},
	}}

	params := TurnStartRequest{
		ThreadID: threadID,
		Input:    input,
	}

	var resp TurnStartResponse
	if err := c.call("turn/start", params, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ThreadStartResponse is the response for thread/start
type ThreadStartResponse struct {
	ThreadID  string `json:"thread_id"`
	SessionID string `json:"session_id"`
	StartedAt string `json:"started_at"`
}

// StartThread starts a new thread
func (c *RPCClient) StartThread() (*ThreadStartResponse, error) {
	var resp ThreadStartResponse
	if err := c.call("thread/start", nil, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

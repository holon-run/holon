package tui

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sync/atomic"
	"time"
)

// RPCClient is a JSON-RPC client for the TUI
type RPCClient struct {
	rpcURL    string
	client    *http.Client
	requestID atomic.Int64
}

// NewRPCClient creates a new RPC client
func NewRPCClient(rpcURL string) *RPCClient {
	c := &RPCClient{
		rpcURL: rpcURL,
		client: &http.Client{
			Timeout: 10 * time.Second,
		},
	}
	c.requestID.Store(0)
	return c
}

// StatusResponse is the response for holon/status
type StatusResponse struct {
	State             string    `json:"state"`
	EventsProcessed   int64     `json:"events_processed"`
	LastEventAt       time.Time `json:"last_event_at"`
	ControllerSession string    `json:"controller_session_id"`
	PausedAt          time.Time `json:"paused_at,omitempty"`
	ResumedAt         time.Time `json:"resumed_at,omitempty"`
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
	ID      int             `json:"id"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *rpcError       `json:"error,omitempty"`
}

// rpcError represents a JSON-RPC error
type rpcError struct {
	Code    int             `json:"code"`
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data,omitempty"`
}

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

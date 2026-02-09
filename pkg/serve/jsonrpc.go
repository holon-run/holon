package serve

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
)

// JSON-RPC 2.0 specification types
// See: https://www.jsonrpc.org/specification

// JSONRPCRequest represents a JSON-RPC 2.0 request object
type JSONRPCRequest struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      interface{}     `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
}

// JSONRPCResponse represents a JSON-RPC 2.0 response object
type JSONRPCResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      interface{}     `json:"id"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *JSONRPCError   `json:"error,omitempty"`
}

// JSONRPCError represents a JSON-RPC 2.0 error object
type JSONRPCError struct {
	Code    int             `json:"code"`
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data,omitempty"`
}

// Standard JSON-RPC 2.0 error codes
const (
	// Parse error: Invalid JSON was received by the server
	ErrCodeParseError = -32700

	// Invalid request: The JSON sent is not a valid Request object
	ErrCodeInvalidRequest = -32600

	// Method not found: The method does not exist / is not available
	ErrCodeMethodNotFound = -32601

	// Invalid params: Invalid method parameter(s)
	ErrCodeInvalidParams = -32602

	// Internal error: Internal JSON-RPC error
	ErrCodeInternalError = -32603
)

// Standard error messages
const (
	ErrMsgParseError     = "Parse error"
	ErrMsgInvalidRequest = "Invalid Request"
	ErrMsgMethodNotFound = "Method not found"
	ErrMsgInvalidParams  = "Invalid params"
	ErrMsgInternalError  = "Internal error"
)

// NewJSONRPCError creates a new JSON-RPC error with the given code and message
func NewJSONRPCError(code int, message string) *JSONRPCError {
	return &JSONRPCError{
		Code:    code,
		Message: message,
	}
}

// NewJSONRPCErrorWithData creates a new JSON-RPC error with additional data
func NewJSONRPCErrorWithData(code int, message string, data interface{}) (*JSONRPCError, error) {
	rpcErr := &JSONRPCError{
		Code:    code,
		Message: message,
	}
	if data != nil {
		rawData, err := json.Marshal(data)
		if err != nil {
			return nil, fmt.Errorf("failed to marshal error data: %w", err)
		}
		rpcErr.Data = json.RawMessage(rawData)
	}
	return rpcErr, nil
}

// MethodHandler is a function that handles a JSON-RPC method call
type MethodHandler func(params json.RawMessage) (interface{}, *JSONRPCError)

// MethodRegistry holds registered JSON-RPC methods
type MethodRegistry struct {
	methods map[string]MethodHandler
}

// NewMethodRegistry creates a new method registry
func NewMethodRegistry() *MethodRegistry {
	return &MethodRegistry{
		methods: make(map[string]MethodHandler),
	}
}

// RegisterMethod registers a new method handler
func (r *MethodRegistry) RegisterMethod(name string, handler MethodHandler) {
	r.methods[name] = handler
}

// Dispatch calls the appropriate method handler based on the method name
func (r *MethodRegistry) Dispatch(method string, params json.RawMessage) (interface{}, *JSONRPCError) {
	handler, ok := r.methods[method]
	if !ok {
		return nil, NewJSONRPCError(ErrCodeMethodNotFound, ErrMsgMethodNotFound)
	}

	result, err := handler(params)
	if err != nil {
		return nil, err
	}
	return result, nil
}

// ValidateJSONRPCRequest validates a JSON-RPC request envelope
func ValidateJSONRPCRequest(req *JSONRPCRequest) *JSONRPCError {
	// Check jsonrpc version
	if req.JSONRPC != "2.0" {
		return NewJSONRPCError(ErrCodeInvalidRequest, "jsonrpc version must be '2.0'")
	}

	// Check method name
	if req.Method == "" {
		return NewJSONRPCError(ErrCodeInvalidRequest, "method is required")
	}

	// ID can be any JSON value (string, number, null) or omitted for notifications
	// We don't validate ID here as per JSON-RPC spec

	// Params can be omitted, or be a structured value (object or array)
	// We don't validate params structure here as it's method-specific

	return nil
}

// ParseJSONRPCRequest parses a JSON-RPC request from a byte slice
func ParseJSONRPCRequest(data []byte) (*JSONRPCRequest, *JSONRPCError) {
	var req JSONRPCRequest
	if err := json.Unmarshal(data, &req); err != nil {
		return nil, NewJSONRPCError(ErrCodeParseError, ErrMsgParseError)
	}

	// Validate the request envelope
	if validationErr := ValidateJSONRPCRequest(&req); validationErr != nil {
		return nil, validationErr
	}

	return &req, nil
}

// WriteJSONRPCResponse writes a JSON-RPC response to the HTTP response writer
func WriteJSONRPCResponse(w http.ResponseWriter, id interface{}, result interface{}, rpcErr *JSONRPCError) {
	w.Header().Set("Content-Type", "application/json")

	resp := JSONRPCResponse{
		JSONRPC: "2.0",
		ID:      id,
	}

	if rpcErr != nil {
		resp.Error = rpcErr
	} else {
		rawResult, err := json.Marshal(result)
		if err != nil {
			// Failed to marshal result - return internal error
			resp.Error = NewJSONRPCError(ErrCodeInternalError, ErrMsgInternalError)
		} else {
			resp.Result = json.RawMessage(rawResult)
		}
	}

	if err := json.NewEncoder(w).Encode(resp); err != nil {
		http.Error(w, "failed to encode response", http.StatusInternalServerError)
		return
	}
}

// ReadJSONRPCRequest reads and parses a JSON-RPC request from an HTTP request
func ReadJSONRPCRequest(r *http.Request) (*JSONRPCRequest, *JSONRPCError) {
	// Read body
	body, err := io.ReadAll(r.Body)
	if err != nil {
		return nil, NewJSONRPCError(ErrCodeParseError, "failed to read request body")
	}
	defer r.Body.Close()

	if len(body) == 0 {
		return nil, NewJSONRPCError(ErrCodeInvalidRequest, "empty request body")
	}

	return ParseJSONRPCRequest(body)
}

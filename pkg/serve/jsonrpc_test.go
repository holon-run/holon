package serve

import (
	"encoding/json"
	"testing"
)

func TestParseJSONRPCRequest_ValidRequest(t *testing.T) {
	data := []byte(`{
		"jsonrpc": "2.0",
		"id": 1,
		"method": "test.method",
		"params": {"key": "value"}
	}`)

	req, err := ParseJSONRPCRequest(data)
	if err != nil {
		t.Fatalf("ParseJSONRPCRequest() error = %v", err)
	}

	if req.JSONRPC != "2.0" {
		t.Errorf("JSONRPC version = %s, want '2.0'", req.JSONRPC)
	}

	if req.Method != "test.method" {
		t.Errorf("Method = %s, want 'test.method'", req.Method)
	}

	if req.ID != float64(1) {
		t.Errorf("ID = %v, want 1", req.ID)
	}

	if string(req.Params) != `{"key": "value"}` {
		t.Errorf("Params = %s, want '{\"key\": \"value\"}'", string(req.Params))
	}
}

func TestParseJSONRPCRequest_InvalidJSON(t *testing.T) {
	data := []byte(`{invalid json`)

	req, err := ParseJSONRPCRequest(data)
	if err == nil {
		t.Error("ParseJSONRPCRequest() expected error for invalid JSON, got nil")
	}

	if req != nil {
		t.Error("ParseJSONRPCRequest() expected nil request for invalid JSON")
	}

	if err.Code != ErrCodeParseError {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeParseError)
	}
}

func TestParseJSONRPCRequest_MissingJSONRPCVersion(t *testing.T) {
	data := []byte(`{
		"id": 1,
		"method": "test.method"
	}`)

	_, err := ParseJSONRPCRequest(data)
	if err == nil {
		t.Error("ParseJSONRPCRequest() expected error for missing jsonrpc version, got nil")
	}

	if err.Code != ErrCodeInvalidRequest {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeInvalidRequest)
	}
}

func TestParseJSONRPCRequest_WrongJSONRPCVersion(t *testing.T) {
	data := []byte(`{
		"jsonrpc": "1.0",
		"id": 1,
		"method": "test.method"
	}`)

	_, err := ParseJSONRPCRequest(data)
	if err == nil {
		t.Error("ParseJSONRPCRequest() expected error for wrong jsonrpc version, got nil")
	}

	if err.Code != ErrCodeInvalidRequest {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeInvalidRequest)
	}
}

func TestParseJSONRPCRequest_MissingMethod(t *testing.T) {
	data := []byte(`{
		"jsonrpc": "2.0",
		"id": 1
	}`)

	_, err := ParseJSONRPCRequest(data)
	if err == nil {
		t.Error("ParseJSONRPCRequest() expected error for missing method, got nil")
	}

	if err.Code != ErrCodeInvalidRequest {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeInvalidRequest)
	}
}

func TestValidateJSONRPCRequest_Valid(t *testing.T) {
	req := &JSONRPCRequest{
		JSONRPC: "2.0",
		ID:      1,
		Method:  "test.method",
		Params:  json.RawMessage(`{"key": "value"}`),
	}

	err := ValidateJSONRPCRequest(req)
	if err != nil {
		t.Errorf("ValidateJSONRPCRequest() error = %v", err)
	}
}

func TestValidateJSONRPCRequest_InvalidVersion(t *testing.T) {
	req := &JSONRPCRequest{
		JSONRPC: "1.0",
		ID:      1,
		Method:  "test.method",
	}

	err := ValidateJSONRPCRequest(req)
	if err == nil {
		t.Error("ValidateJSONRPCRequest() expected error for invalid version, got nil")
	}

	if err.Code != ErrCodeInvalidRequest {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeInvalidRequest)
	}
}

func TestValidateJSONRPCRequest_EmptyMethod(t *testing.T) {
	req := &JSONRPCRequest{
		JSONRPC: "2.0",
		ID:      1,
		Method:  "",
	}

	err := ValidateJSONRPCRequest(req)
	if err == nil {
		t.Error("ValidateJSONRPCRequest() expected error for empty method, got nil")
	}

	if err.Code != ErrCodeInvalidRequest {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeInvalidRequest)
	}
}

func TestMethodRegistry_RegisterAndDispatch(t *testing.T) {
	registry := NewMethodRegistry()

	// Register a test method
	handlerCalled := false
	registry.RegisterMethod("test.method", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		handlerCalled = true
		return map[string]string{"status": "ok"}, nil
	})

	// Dispatch to the registered method
	result, err := registry.Dispatch("test.method", nil)
	if err != nil {
		t.Fatalf("Dispatch() error = %v", err)
	}

	if !handlerCalled {
		t.Error("Dispatch() did not call the registered handler")
	}

 resultMap, ok := result.(map[string]string)
	if !ok {
		t.Fatalf("Result type = %T, want map[string]string", result)
	}

	if resultMap["status"] != "ok" {
		t.Errorf("Result status = %s, want 'ok'", resultMap["status"])
	}
}

func TestMethodRegistry_MethodNotFound(t *testing.T) {
	registry := NewMethodRegistry()

	result, err := registry.Dispatch("unknown.method", nil)
	if err == nil {
		t.Error("Dispatch() expected error for unknown method, got nil")
	}

	if err.Code != ErrCodeMethodNotFound {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeMethodNotFound)
	}

	if result != nil {
		t.Error("Dispatch() expected nil result for error, got non-nil")
	}
}

func TestMethodRegistry_HandlerReturnsError(t *testing.T) {
	registry := NewMethodRegistry()

	// Register a method that returns an error
	registry.RegisterMethod("failing.method", func(params json.RawMessage) (interface{}, *JSONRPCError) {
		return nil, NewJSONRPCError(ErrCodeInvalidParams, "invalid parameter")
	})

	result, err := registry.Dispatch("failing.method", nil)
	if err == nil {
		t.Error("Dispatch() expected error from handler, got nil")
	}

	if err.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeInvalidParams)
	}

	if result != nil {
		t.Error("Dispatch() expected nil result for error, got non-nil")
	}
}

func TestNewJSONRPCError(t *testing.T) {
	err := NewJSONRPCError(ErrCodeParseError, "test error")

	if err.Code != ErrCodeParseError {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeParseError)
	}

	if err.Message != "test error" {
		t.Errorf("Error message = %s, want 'test error'", err.Message)
	}
}

func TestNewJSONRPCErrorWithData(t *testing.T) {
	data := map[string]string{"field": "value"}
	err, marshalErr := NewJSONRPCErrorWithData(ErrCodeInvalidParams, "invalid params", data)
	if marshalErr != nil {
		t.Fatalf("NewJSONRPCErrorWithData() error = %v", marshalErr)
	}

	if err.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", err.Code, ErrCodeInvalidParams)
	}

	if err.Message != "invalid params" {
		t.Errorf("Error message = %s, want 'invalid params'", err.Message)
	}

	if err.Data == nil {
		t.Error("Error data is nil, want non-nil")
	}

	var unmarshaledData map[string]string
	if unmarshalErr := json.Unmarshal(err.Data, &unmarshaledData); unmarshalErr != nil {
		t.Fatalf("Failed to unmarshal error data: %v", unmarshalErr)
	}

	if unmarshaledData["field"] != "value" {
		t.Errorf("Error data field = %s, want 'value'", unmarshaledData["field"])
	}
}

func TestParseJSONRPCRequest_WithArrayParams(t *testing.T) {
	data := []byte(`{
		"jsonrpc": "2.0",
		"id": 1,
		"method": "test.method",
		"params": [1, 2, 3]
	}`)

	req, err := ParseJSONRPCRequest(data)
	if err != nil {
		t.Fatalf("ParseJSONRPCRequest() error = %v", err)
	}

	if req.Method != "test.method" {
		t.Errorf("Method = %s, want 'test.method'", req.Method)
	}

	// Verify params is an array
	var params []interface{}
	if unmarshalErr := json.Unmarshal(req.Params, &params); unmarshalErr != nil {
		t.Fatalf("Failed to unmarshal params as array: %v", unmarshalErr)
	}

	if len(params) != 3 {
		t.Errorf("Params length = %d, want 3", len(params))
	}
}

func TestParseJSONRPCRequest_WithoutParams(t *testing.T) {
	data := []byte(`{
		"jsonrpc": "2.0",
		"id": 1,
		"method": "test.method"
	}`)

	req, err := ParseJSONRPCRequest(data)
	if err != nil {
		t.Fatalf("ParseJSONRPCRequest() error = %v", err)
	}

	if req.Method != "test.method" {
		t.Errorf("Method = %s, want 'test.method'", req.Method)
	}

	if len(req.Params) != 0 {
		t.Errorf("Params = %s, want empty", string(req.Params))
	}
}

func TestParseJSONRPCRequest_Notification(t *testing.T) {
	// JSON-RPC notification - request without ID
	data := []byte(`{
		"jsonrpc": "2.0",
		"method": "test.method",
		"params": {"key": "value"}
	}`)

	req, err := ParseJSONRPCRequest(data)
	if err != nil {
		t.Fatalf("ParseJSONRPCRequest() error = %v", err)
	}

	if req.Method != "test.method" {
		t.Errorf("Method = %s, want 'test.method'", req.Method)
	}

	if req.ID != nil {
		t.Errorf("ID = %v, want nil (notification)", req.ID)
	}
}

package serve

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestNewRuntime(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	state := rt.GetState()
	if state.State != RuntimeStateRunning {
		t.Errorf("Initial state = %s, want %s", state.State, RuntimeStateRunning)
	}

	if state.EventsProcessed != 0 {
		t.Errorf("Initial events_processed = %d, want 0", state.EventsProcessed)
	}
}

func TestRuntimePauseResume(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Test pause
	if err := rt.Pause(); err != nil {
		t.Fatalf("Pause() error = %v", err)
	}

	state := rt.GetState()
	if state.State != RuntimeStatePaused {
		t.Errorf("State after pause = %s, want %s", state.State, RuntimeStatePaused)
	}

	if !rt.IsPaused() {
		t.Error("IsPaused() after pause = false, want true")
	}

	// Test double pause
	if err := rt.Pause(); err == nil {
		t.Error("Pause() when paused expected error, got nil")
	}

	// Test resume
	if err := rt.Resume(); err != nil {
		t.Fatalf("Resume() error = %v", err)
	}

	state = rt.GetState()
	if state.State != RuntimeStateRunning {
		t.Errorf("State after resume = %s, want %s", state.State, RuntimeStateRunning)
	}

	if rt.IsPaused() {
		t.Error("IsPaused() after resume = true, want false")
	}

	// Test double resume
	if err := rt.Resume(); err == nil {
		t.Error("Resume() when running expected error, got nil")
	}
}

func TestRuntimeRecordEvent(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Record some events
	rt.RecordEvent("evt1")
	rt.RecordEvent("evt2")
	rt.RecordEvent("evt3")

	state := rt.GetState()
	if state.EventsProcessed != 3 {
		t.Errorf("EventsProcessed = %d, want 3", state.EventsProcessed)
	}

	if state.LastEventAt.IsZero() {
		t.Error("LastEventAt is zero, want non-zero")
	}
}

func TestRuntimeSetControllerSession(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	sessionID := "sess_test123"
	rt.SetControllerSession(sessionID)

	state := rt.GetState()
	if state.ControllerSession != sessionID {
		t.Errorf("ControllerSession = %s, want %s", state.ControllerSession, sessionID)
	}
}

func TestRuntimePersistence(t *testing.T) {
	tmpDir := t.TempDir()

	// Create runtime and modify state
	rt1, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	rt1.RecordEvent("evt1")
	rt1.SetControllerSession("sess_test")
	if err := rt1.Pause(); err != nil {
		t.Fatalf("Pause() error = %v", err)
	}

	// Create new runtime instance - should load persisted state
	rt2, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	state := rt2.GetState()
	if state.EventsProcessed != 1 {
		t.Errorf("Loaded EventsProcessed = %d, want 1", state.EventsProcessed)
	}

	if state.ControllerSession != "sess_test" {
		t.Errorf("Loaded ControllerSession = %s, want 'sess_test'", state.ControllerSession)
	}

	if state.State != RuntimeStatePaused {
		t.Errorf("Loaded State = %s, want %s", state.State, RuntimeStatePaused)
	}

	if !rt2.IsPaused() {
		t.Error("Loaded runtime is not paused")
	}
}

func TestHandleStatus(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	rt.RecordEvent("evt1")
	rt.SetControllerSession("sess_test")

	result, rpcErr := rt.HandleStatus(nil)
	if rpcErr != nil {
		t.Fatalf("HandleStatus() error = %v", rpcErr)
	}

	resp, ok := result.(StatusResponse)
	if !ok {
		t.Fatalf("HandleStatus() result type = %T, want StatusResponse", result)
	}

	if resp.State != RuntimeStateRunning {
		t.Errorf("Status State = %s, want %s", resp.State, RuntimeStateRunning)
	}

	if resp.EventsProcessed != 1 {
		t.Errorf("Status EventsProcessed = %d, want 1", resp.EventsProcessed)
	}

	if resp.ControllerSession != "sess_test" {
		t.Errorf("Status ControllerSession = %s, want 'sess_test'", resp.ControllerSession)
	}

	if resp.LastEventAt.IsZero() {
		t.Error("Status LastEventAt is zero")
	}
}

func TestHandlePause(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	result, rpcErr := rt.HandlePause(nil)
	if rpcErr != nil {
		t.Fatalf("HandlePause() error = %v", rpcErr)
	}

	resp, ok := result.(PauseResponse)
	if !ok {
		t.Fatalf("HandlePause() result type = %T, want PauseResponse", result)
	}

	if !resp.Success {
		t.Errorf("Pause Success = false, want true")
	}

	if resp.Message != "Paused event processing" {
		t.Errorf("Pause Message = %s, want 'Paused event processing'", resp.Message)
	}

	if !rt.IsPaused() {
		t.Error("Runtime not paused after HandlePause")
	}
}

func TestHandleResume(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Pause first
	if err := rt.Pause(); err != nil {
		t.Fatalf("Pause() error = %v", err)
	}

	result, rpcErr := rt.HandleResume(nil)
	if rpcErr != nil {
		t.Fatalf("HandleResume() error = %v", rpcErr)
	}

	resp, ok := result.(ResumeResponse)
	if !ok {
		t.Fatalf("HandleResume() result type = %T, want ResumeResponse", result)
	}

	if !resp.Success {
		t.Errorf("Resume Success = false, want true")
	}

	if resp.Message != "Resumed event processing" {
		t.Errorf("Resume Message = %s, want 'Resumed event processing'", resp.Message)
	}

	if rt.IsPaused() {
		t.Error("Runtime still paused after HandleResume")
	}
}

func TestHandleLogStream(t *testing.T) {
	tmpDir := t.TempDir()

	// Create a test log file
	logPath := filepath.Join(tmpDir, "events.ndjson")
	logEntries := []struct {
		Level   string
		Time    time.Time
		Message string
	}{
		{"info", time.Now().Add(-2 * time.Hour), "Event 1"},
		{"info", time.Now().Add(-1 * time.Hour), "Event 2"},
		{"debug", time.Now(), "Event 3"},
	}

	file, err := os.Create(logPath)
	if err != nil {
		t.Fatalf("Failed to create log file: %v", err)
	}
	for _, entry := range logEntries {
		line, _ := json.Marshal(map[string]interface{}{
			"level": entry.Level,
			"time":  entry.Time,
			"msg":   entry.Message,
		})
		file.Write(append(line, '\n'))
	}
	file.Close()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	handler := rt.HandleLogStream(logPath)

	// Test with no params
	result, rpcErr := handler(nil)
	if rpcErr != nil {
		t.Fatalf("HandleLogStream() error = %v", rpcErr)
	}

	resp, ok := result.(LogStreamResponse)
	if !ok {
		t.Fatalf("HandleLogStream() result type = %T, want LogStreamResponse", result)
	}

	if len(resp.Logs) == 0 {
		t.Error("No logs returned, expected at least 1")
	}

	if resp.StreamID == "" {
		t.Error("StreamID is empty")
	}

	// Test with params
	params, _ := json.Marshal(map[string]int{
		"max_lines": 2,
	})
	result, rpcErr = handler(params)
	if rpcErr != nil {
		t.Fatalf("HandleLogStream() with params error = %v", rpcErr)
	}

	resp, ok = result.(LogStreamResponse)
	if !ok {
		t.Fatalf("HandleLogStream() with params result type = %T, want LogStreamResponse", result)
	}

	if len(resp.Logs) > 2 {
		t.Errorf("Got %d logs with max_lines=2, want at most 2", len(resp.Logs))
	}
}

func TestHandleLogStreamInvalidParams(t *testing.T) {
	tmpDir := t.TempDir()
	logPath := filepath.Join(tmpDir, "events.ndjson")

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	handler := rt.HandleLogStream(logPath)

	// Test invalid max_lines (too large)
	params, _ := json.Marshal(map[string]int{
		"max_lines": 100000,
	})
	_, rpcErr := handler(params)
	if rpcErr == nil {
		t.Error("Expected error for max_lines > 10000, got nil")
	}

	if rpcErr.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
	}

	// Test invalid max_lines (zero)
	params, _ = json.Marshal(map[string]int{
		"max_lines": 0,
	})
	_, rpcErr = handler(params)
	if rpcErr == nil {
		t.Error("Expected error for max_lines = 0, got nil")
	}

	// Test invalid params JSON
	_, rpcErr = handler([]byte("{invalid json"))
	if rpcErr == nil {
		t.Error("Expected error for invalid JSON params, got nil")
	}

	if rpcErr.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
	}
}

func TestHandleLogStreamNonexistentFile(t *testing.T) {
	tmpDir := t.TempDir()
	logPath := filepath.Join(tmpDir, "nonexistent.ndjson")

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	handler := rt.HandleLogStream(logPath)

	result, rpcErr := handler(nil)
	if rpcErr != nil {
		t.Fatalf("HandleLogStream() with nonexistent file error = %v", rpcErr)
	}

	resp, ok := result.(LogStreamResponse)
	if !ok {
		t.Fatalf("HandleLogStream() result type = %T, want LogStreamResponse", result)
	}

	if len(resp.Logs) != 0 {
		t.Errorf("Got %d logs for nonexistent file, want 0", len(resp.Logs))
	}
}

func TestReadLogsFromPath(t *testing.T) {
	tmpDir := t.TempDir()
	logPath := filepath.Join(tmpDir, "test.ndjson")

	// Create test log file
	logEntries := []string{
		`{"level":"info","time":"2026-02-09T12:00:00Z","msg":"Message 1"}`,
		`{"level":"debug","time":"2026-02-09T12:01:00Z","msg":"Message 2"}`,
		`{"level":"error","time":"2026-02-09T12:02:00Z","msg":"Message 3"}`,
	}

	file, err := os.Create(logPath)
	if err != nil {
		t.Fatalf("Failed to create log file: %v", err)
	}
	for _, entry := range logEntries {
		file.WriteString(entry + "\n")
	}
	file.Close()

	// Read all logs
	logs, err := readLogsFromPath(logPath, 0, 10)
	if err != nil {
		t.Fatalf("readLogsFromPath() error = %v", err)
	}

	if len(logs) != 3 {
		t.Errorf("Got %d logs, want 3", len(logs))
	}

	// Test from_position
	logs, err = readLogsFromPath(logPath, 1, 10)
	if err != nil {
		t.Fatalf("readLogsFromPath() with from_position error = %v", err)
	}

	if len(logs) != 2 {
		t.Errorf("Got %d logs with from_position=1, want 2", len(logs))
	}

	// Test max_lines
	logs, err = readLogsFromPath(logPath, 0, 2)
	if err != nil {
		t.Fatalf("readLogsFromPath() with max_lines error = %v", err)
	}

	if len(logs) != 2 {
		t.Errorf("Got %d logs with max_lines=2, want 2", len(logs))
	}
}

func TestHandleThreadStart(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Test with no params
	result, rpcErr := rt.HandleThreadStart(nil)
	if rpcErr != nil {
		t.Fatalf("HandleThreadStart() error = %v", rpcErr)
	}

	resp, ok := result.(ThreadStartResponse)
	if !ok {
		t.Fatalf("HandleThreadStart() result type = %T, want ThreadStartResponse", result)
	}

	if resp.ThreadID == "" {
		t.Error("ThreadID is empty")
	}

	if resp.SessionID == "" {
		t.Error("SessionID is empty")
	}

	if resp.SessionID != resp.ThreadID {
		t.Errorf("SessionID %s != ThreadID %s", resp.SessionID, resp.ThreadID)
	}

	if resp.StartedAt == "" {
		t.Error("StartedAt is empty")
	}

	// Verify session was set
	state := rt.GetState()
	if state.ControllerSession != resp.SessionID {
		t.Errorf("ControllerSession = %s, want %s", state.ControllerSession, resp.SessionID)
	}

	// Test with params
	params, _ := json.Marshal(map[string]interface{}{
		"extended_context": map[string]string{
			"test_key": "test_value",
		},
	})
	result, rpcErr = rt.HandleThreadStart(params)
	if rpcErr != nil {
		t.Fatalf("HandleThreadStart() with params error = %v", rpcErr)
	}

	resp, ok = result.(ThreadStartResponse)
	if !ok {
		t.Fatalf("HandleThreadStart() with params result type = %T, want ThreadStartResponse", result)
	}

	if resp.ThreadID == "" {
		t.Error("ThreadID with params is empty")
	}
}

func TestHandleThreadStartResumesIfPaused(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Pause the runtime
	if err := rt.Pause(); err != nil {
		t.Fatalf("Pause() error = %v", err)
	}

	if !rt.IsPaused() {
		t.Error("Runtime should be paused")
	}

	// Thread start should resume
	result, rpcErr := rt.HandleThreadStart(nil)
	if rpcErr != nil {
		t.Fatalf("HandleThreadStart() error = %v", rpcErr)
	}

	_, ok := result.(ThreadStartResponse)
	if !ok {
		t.Fatalf("HandleThreadStart() result type = %T, want ThreadStartResponse", result)
	}

	// Verify runtime is no longer paused
	if rt.IsPaused() {
		t.Error("Runtime should be running after thread/start")
	}
}

func TestHandleTurnStart(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Test without required params should fail
	_, rpcErr := rt.HandleTurnStart(nil)
	if rpcErr == nil {
		t.Fatalf("HandleTurnStart() expected invalid params error, got nil")
	}
	if rpcErr.Code != ErrCodeInvalidParams {
		t.Fatalf("HandleTurnStart() error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
	}

	// Test with params
	params, _ := json.Marshal(map[string]interface{}{
		"thread_id": "thread_test123",
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hello"},
				},
			},
		},
		"extended_context": map[string]string{
			"test_key": "test_value",
		},
	})
	result, rpcErr := rt.HandleTurnStart(params)
	if rpcErr != nil {
		t.Fatalf("HandleTurnStart() with params error = %v", rpcErr)
	}

	resp, ok := result.(TurnStartResponse)
	if !ok {
		t.Fatalf("HandleTurnStart() with params result type = %T, want TurnStartResponse", result)
	}

	if resp.TurnID == "" {
		t.Error("TurnID with params is empty")
	}
}

func TestHandleTurnStartResumesIfPaused(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Pause the runtime
	if err := rt.Pause(); err != nil {
		t.Fatalf("Pause() error = %v", err)
	}

	if !rt.IsPaused() {
		t.Error("Runtime should be paused")
	}

	params, _ := json.Marshal(map[string]interface{}{
		"thread_id": "thread_test123",
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hello"},
				},
			},
		},
	})

	// Turn start should resume
	result, rpcErr := rt.HandleTurnStart(params)
	if rpcErr != nil {
		t.Fatalf("HandleTurnStart() error = %v", rpcErr)
	}

	_, ok := result.(TurnStartResponse)
	if !ok {
		t.Fatalf("HandleTurnStart() result type = %T, want TurnStartResponse", result)
	}

	// Verify runtime is no longer paused
	if rt.IsPaused() {
		t.Error("Runtime should be running after turn/start")
	}
}

func TestHandleTurnStartInputValidation(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	tests := []struct {
		name   string
		params map[string]interface{}
	}{
		{
			name: "missing thread_id",
			params: map[string]interface{}{
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
		},
		{
			name: "missing input",
			params: map[string]interface{}{
				"thread_id": "thread_test123",
			},
		},
		{
			name: "empty text",
			params: map[string]interface{}{
				"thread_id": "thread_test123",
				"input": []map[string]interface{}{
					{
						"type": "message",
						"role": "user",
						"content": []map[string]interface{}{
							{"type": "input_text", "text": ""},
						},
					},
				},
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			params, _ := json.Marshal(tc.params)
			_, rpcErr := rt.HandleTurnStart(params)
			if rpcErr == nil {
				t.Fatalf("expected invalid params error")
			}
			if rpcErr.Code != ErrCodeInvalidParams {
				t.Fatalf("error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
			}
		})
	}
}

func TestHandleTurnInterrupt(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	startParams, _ := json.Marshal(map[string]interface{}{
		"thread_id": "thread_test123",
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hello"},
				},
			},
		},
	})
	startResult, startErr := rt.HandleTurnStart(startParams)
	if startErr != nil {
		t.Fatalf("HandleTurnStart() setup error = %v", startErr)
	}
	startResp, ok := startResult.(TurnStartResponse)
	if !ok {
		t.Fatalf("HandleTurnStart() setup result type = %T, want TurnStartResponse", startResult)
	}

	// Test with params
	params, _ := json.Marshal(map[string]interface{}{
		"turn_id": startResp.TurnID,
		"reason":  "User requested interruption",
	})
	result, rpcErr := rt.HandleTurnInterrupt(params)
	if rpcErr != nil {
		t.Fatalf("HandleTurnInterrupt() error = %v", rpcErr)
	}

	resp, ok := result.(TurnInterruptResponse)
	if !ok {
		t.Fatalf("HandleTurnInterrupt() result type = %T, want TurnInterruptResponse", result)
	}

	if resp.TurnID == "" {
		t.Error("TurnID is empty")
	}

	if resp.State != "interrupted" {
		t.Errorf("State = %s, want 'interrupted'", resp.State)
	}

	if resp.InterruptedAt == "" {
		t.Error("InterruptedAt is empty")
	}

	if resp.Message == "" {
		t.Error("Message is empty")
	}

	// Verify runtime remains active for targeted turn interruption
	if rt.IsPaused() {
		t.Error("Runtime should remain running after targeted turn/interrupt")
	}

	result, rpcErr = rt.HandleTurnInterrupt(nil)
	if rpcErr != nil {
		t.Fatalf("HandleTurnInterrupt() without params error = %v", rpcErr)
	}

	resp, ok = result.(TurnInterruptResponse)
	if !ok {
		t.Fatalf("HandleTurnInterrupt() without params result type = %T, want TurnInterruptResponse", result)
	}

	if resp.TurnID == "" {
		t.Error("TurnID should be auto-generated when omitted")
	}

	if !rt.IsPaused() {
		t.Error("Runtime should be paused after turn/interrupt without turn_id")
	}
}

func TestHandleTurnInterruptWhenAlreadyPaused(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Pause the runtime
	if err := rt.Pause(); err != nil {
		t.Fatalf("Pause() error = %v", err)
	}

	if !rt.IsPaused() {
		t.Error("Runtime should be paused")
	}

	// Turn interrupt should return error when already paused
	_, rpcErr := rt.HandleTurnInterrupt(nil)
	if rpcErr == nil {
		t.Error("Expected error when interrupting already paused runtime, got nil")
	}

	if rpcErr.Code != ErrCodeInternalError {
		t.Errorf("Error code = %d, want %d", rpcErr.Code, ErrCodeInternalError)
	}
}

func TestHandleTurnSteer(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	startParams, _ := json.Marshal(map[string]interface{}{
		"thread_id": "thread_test123",
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hello"},
				},
			},
		},
	})
	startResult, startErr := rt.HandleTurnStart(startParams)
	if startErr != nil {
		t.Fatalf("HandleTurnStart() setup error = %v", startErr)
	}
	startResp, ok := startResult.(TurnStartResponse)
	if !ok {
		t.Fatalf("HandleTurnStart() setup result type = %T, want TurnStartResponse", startResult)
	}

	steerParams, _ := json.Marshal(map[string]interface{}{
		"turn_id": startResp.TurnID,
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "continue"},
				},
			},
		},
	})
	result, rpcErr := rt.HandleTurnSteer(steerParams)
	if rpcErr != nil {
		t.Fatalf("HandleTurnSteer() error = %v", rpcErr)
	}
	resp, ok := result.(TurnSteerResponse)
	if !ok {
		t.Fatalf("HandleTurnSteer() result type = %T, want TurnSteerResponse", result)
	}
	if resp.TurnID != startResp.TurnID {
		t.Errorf("TurnID = %s, want %s", resp.TurnID, startResp.TurnID)
	}
	if resp.AcceptedItems != 1 {
		t.Errorf("AcceptedItems = %d, want 1", resp.AcceptedItems)
	}
	if resp.State != StateActive {
		t.Errorf("State = %s, want %s", resp.State, StateActive)
	}
}

func TestHandleTurnSteerInvalidParams(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Missing turn_id
	params, _ := json.Marshal(map[string]interface{}{
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hi"},
				},
			},
		},
	})
	_, rpcErr := rt.HandleTurnSteer(params)
	if rpcErr == nil || rpcErr.Code != ErrCodeInvalidParams {
		t.Fatalf("expected invalid params for missing turn_id, got %v", rpcErr)
	}

	// Unknown turn_id
	params, _ = json.Marshal(map[string]interface{}{
		"turn_id": "turn_missing",
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hi"},
				},
			},
		},
	})
	_, rpcErr = rt.HandleTurnSteer(params)
	if rpcErr == nil || rpcErr.Code != ErrCodeInvalidParams {
		t.Fatalf("expected invalid params for missing active turn, got %v", rpcErr)
	}
}

func TestCodexMethodsInvalidParams(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	// Test thread/start with invalid JSON
	_, rpcErr := rt.HandleThreadStart([]byte("{invalid json"))
	if rpcErr == nil {
		t.Error("Expected error for thread/start with invalid JSON, got nil")
	}
	if rpcErr.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
	}

	// Test turn/start with invalid JSON
	_, rpcErr = rt.HandleTurnStart([]byte("{invalid json"))
	if rpcErr == nil {
		t.Error("Expected error for turn/start with invalid JSON, got nil")
	}
	if rpcErr.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
	}

	// Test turn/interrupt with invalid JSON
	_, rpcErr = rt.HandleTurnInterrupt([]byte("{invalid json"))
	if rpcErr == nil {
		t.Error("Expected error for turn/interrupt with invalid JSON, got nil")
	}
	if rpcErr.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
	}

	// Test turn/steer with invalid JSON
	_, rpcErr = rt.HandleTurnSteer([]byte("{invalid json"))
	if rpcErr == nil {
		t.Error("Expected error for turn/steer with invalid JSON, got nil")
	}
	if rpcErr.Code != ErrCodeInvalidParams {
		t.Errorf("Error code = %d, want %d", rpcErr.Code, ErrCodeInvalidParams)
	}
}

func TestTurnStartInvalidParamsIncludeFieldData(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}

	params, _ := json.Marshal(map[string]interface{}{
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hello"},
				},
			},
		},
	})

	_, rpcErr := rt.HandleTurnStart(params)
	if rpcErr == nil {
		t.Fatalf("expected error, got nil")
	}
	if len(rpcErr.Data) == 0 {
		t.Fatalf("expected error data for invalid params")
	}
	var data map[string]string
	if err := json.Unmarshal(rpcErr.Data, &data); err != nil {
		t.Fatalf("failed to decode error data: %v", err)
	}
	if data["field"] != "thread_id" {
		t.Fatalf("expected field=thread_id, got %q", data["field"])
	}
}

func TestTurnAutoCompleteEmitsOnceAfterReschedule(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}
	rt.setTurnIdleTTLForTest(40 * time.Millisecond)

	b := NewNotificationBroadcaster()
	var buf bytes.Buffer
	sw := NewStreamWriter(&buf)
	b.Subscribe(sw)
	rt.SetBroadcaster(b)

	turnID, _ := rt.beginTurn("thread_test")
	rt.scheduleTurnAutoComplete(turnID)
	rt.scheduleTurnAutoComplete(turnID)

	deadline := time.Now().Add(2 * time.Second)
	for {
		if strings.Count(buf.String(), "\"method\":\"turn/completed\"") >= 1 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for turn/completed notification; got: %s", buf.String())
		}
		time.Sleep(10 * time.Millisecond)
	}

	time.Sleep(80 * time.Millisecond)
	if got := strings.Count(buf.String(), "\"method\":\"turn/completed\""); got != 1 {
		t.Fatalf("expected exactly one turn/completed notification, got %d (%s)", got, buf.String())
	}
}

func TestTurnInterruptNotificationIncludesTurnContext(t *testing.T) {
	tmpDir := t.TempDir()

	rt, err := NewRuntime(tmpDir)
	if err != nil {
		t.Fatalf("NewRuntime() error = %v", err)
	}
	rt.setTurnIdleTTLForTest(5 * time.Second)

	b := NewNotificationBroadcaster()
	var buf bytes.Buffer
	sw := NewStreamWriter(&buf)
	b.Subscribe(sw)
	rt.SetBroadcaster(b)

	startParams, _ := json.Marshal(map[string]interface{}{
		"thread_id": "thread_test_ctx",
		"input": []map[string]interface{}{
			{
				"type": "message",
				"role": "user",
				"content": []map[string]interface{}{
					{"type": "input_text", "text": "hello"},
				},
			},
		},
	})
	startResult, rpcErr := rt.HandleTurnStart(startParams)
	if rpcErr != nil {
		t.Fatalf("HandleTurnStart() error = %v", rpcErr)
	}
	startResp := startResult.(TurnStartResponse)

	interruptParams, _ := json.Marshal(map[string]interface{}{
		"turn_id": startResp.TurnID,
		"reason":  "stop",
	})
	_, rpcErr = rt.HandleTurnInterrupt(interruptParams)
	if rpcErr != nil {
		t.Fatalf("HandleTurnInterrupt() error = %v", rpcErr)
	}

	output := buf.String()
	if !strings.Contains(output, "\"method\":\"turn/interrupted\"") {
		t.Fatalf("expected turn/interrupted notification in output: %s", output)
	}
	if !strings.Contains(output, "\"thread_id\":\"thread_test_ctx\"") {
		t.Fatalf("expected interrupted notification to include thread_id, got: %s", output)
	}
}

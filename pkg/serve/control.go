package serve

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"time"
)

// RuntimeState tracks the current state of the serve runtime
type RuntimeState struct {
	mu                sync.RWMutex
	State             string    `json:"state"` // running, paused
	EventsProcessed   int64     `json:"events_processed"`
	LastEventAt       time.Time `json:"last_event_at"`
	ControllerSession string    `json:"controller_session_id"`
	PausedAt          time.Time `json:"paused_at,omitempty"`
	ResumedAt         time.Time `json:"resumed_at,omitempty"`
}

const (
	RuntimeStateRunning = "running"
	RuntimeStatePaused  = "paused"
)

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

// Runtime manages the serve runtime state
type Runtime struct {
	statePath string
	state     RuntimeState
	now       func() time.Time
	mu        sync.Mutex
}

// NewRuntime creates a new runtime manager
func NewRuntime(stateDir string) (*Runtime, error) {
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create runtime state dir: %w", err)
	}

	rt := &Runtime{
		statePath: filepath.Join(stateDir, "runtime-state.json"),
		now:       time.Now,
		state: RuntimeState{
			State:           RuntimeStateRunning,
			EventsProcessed: 0,
		},
	}

	if err := rt.load(); err != nil {
		return nil, err
	}

	return rt, nil
}

// load loads the runtime state from disk
func (rt *Runtime) load() error {
	rt.mu.Lock()
	defer rt.mu.Unlock()

	data, err := os.ReadFile(rt.statePath)
	if err != nil {
		if os.IsNotExist(err) {
			// Initialize with default state
			rt.state = RuntimeState{
				State:           RuntimeStateRunning,
				EventsProcessed: 0,
			}
			return nil
		}
		return fmt.Errorf("failed to read runtime state: %w", err)
	}

	if err := json.Unmarshal(data, &rt.state); err != nil {
		return fmt.Errorf("failed to parse runtime state: %w", err)
	}

	// Ensure state is valid
	if rt.state.State == "" {
		rt.state.State = RuntimeStateRunning
	}

	return nil
}

// save saves the runtime state to disk
func (rt *Runtime) save() error {
	data, err := json.MarshalIndent(rt.state, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal runtime state: %w", err)
	}

	if err := os.WriteFile(rt.statePath, data, 0644); err != nil {
		return fmt.Errorf("failed to write runtime state: %w", err)
	}

	return nil
}

// GetState returns a copy of the current runtime state
func (rt *Runtime) GetState() RuntimeState {
	rt.mu.Lock()
	defer rt.mu.Unlock()

	return RuntimeState{
		State:             rt.state.State,
		EventsProcessed:   rt.state.EventsProcessed,
		LastEventAt:       rt.state.LastEventAt,
		ControllerSession: rt.state.ControllerSession,
		PausedAt:          rt.state.PausedAt,
		ResumedAt:         rt.state.ResumedAt,
	}
}

// IsPaused returns true if the runtime is paused
func (rt *Runtime) IsPaused() bool {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	return rt.state.State == RuntimeStatePaused
}

// RecordEvent records that an event was processed
func (rt *Runtime) RecordEvent(eventID string) {
	rt.mu.Lock()
	defer rt.mu.Unlock()

	rt.state.EventsProcessed++
	rt.state.LastEventAt = rt.now()
	_ = rt.save()
}

// SetControllerSession sets the current controller session ID
func (rt *Runtime) SetControllerSession(sessionID string) {
	rt.mu.Lock()
	defer rt.mu.Unlock()

	rt.state.ControllerSession = sessionID
	_ = rt.save()
}

// Pause pauses the runtime
func (rt *Runtime) Pause() error {
	rt.mu.Lock()
	defer rt.mu.Unlock()

	if rt.state.State == RuntimeStatePaused {
		return fmt.Errorf("already paused")
	}

	rt.state.State = RuntimeStatePaused
	rt.state.PausedAt = rt.now()

	if err := rt.save(); err != nil {
		rt.state.State = RuntimeStateRunning
		rt.state.PausedAt = time.Time{}
		return err
	}

	return nil
}

// Resume resumes the runtime
func (rt *Runtime) Resume() error {
	rt.mu.Lock()
	defer rt.mu.Unlock()

	if rt.state.State != RuntimeStatePaused {
		return fmt.Errorf("not paused")
	}

	rt.state.State = RuntimeStateRunning
	rt.state.ResumedAt = rt.now()

	if err := rt.save(); err != nil {
		rt.state.State = RuntimeStatePaused
		rt.state.ResumedAt = time.Time{}
		return err
	}

	return nil
}

// HandleStatus is the JSON-RPC handler for holon/status
func (rt *Runtime) HandleStatus(params json.RawMessage) (interface{}, *JSONRPCError) {
	state := rt.GetState()

	return StatusResponse{
		State:             state.State,
		EventsProcessed:   state.EventsProcessed,
		LastEventAt:       state.LastEventAt,
		ControllerSession: state.ControllerSession,
		PausedAt:          state.PausedAt,
		ResumedAt:         state.ResumedAt,
	}, nil
}

// HandlePause is the JSON-RPC handler for holon/pause
func (rt *Runtime) HandlePause(params json.RawMessage) (interface{}, *JSONRPCError) {
	if err := rt.Pause(); err != nil {
		return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to pause: %s", err))
	}

	return PauseResponse{
		Success: true,
		Message: "Paused event processing",
	}, nil
}

// HandleResume is the JSON-RPC handler for holon/resume
func (rt *Runtime) HandleResume(params json.RawMessage) (interface{}, *JSONRPCError) {
	if err := rt.Resume(); err != nil {
		return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to resume: %s", err))
	}

	return ResumeResponse{
		Success: true,
		Message: "Resumed event processing",
	}, nil
}

// HandleLogStream is the JSON-RPC handler for holon/logStream
func (rt *Runtime) HandleLogStream(logPath string) func(json.RawMessage) (interface{}, *JSONRPCError) {
	return func(params json.RawMessage) (interface{}, *JSONRPCError) {
		// Parse params (optional: from_position, max_lines)
		type LogStreamParams struct {
			FromPosition *int `json:"from_position,omitempty"`
			MaxLines     *int `json:"max_lines,omitempty"`
		}

		var p LogStreamParams
		if len(params) > 0 && string(params) != "null" {
			if err := json.Unmarshal(params, &p); err != nil {
				return nil, NewJSONRPCError(ErrCodeInvalidParams, fmt.Sprintf("invalid params: %s", err))
			}
		}

		fromPosition := 0
		if p.FromPosition != nil {
			fromPosition = *p.FromPosition
		}

		maxLines := 100
		if p.MaxLines != nil {
			if *p.MaxLines <= 0 || *p.MaxLines > 10000 {
				return nil, NewJSONRPCError(ErrCodeInvalidParams, "max_lines must be between 1 and 10000")
			}
			maxLines = *p.MaxLines
		}

		// Read log file
		logs, err := readLogsFromPath(logPath, fromPosition, maxLines)
		if err != nil {
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to read logs: %s", err))
		}

		return LogStreamResponse{
			StreamID: fmt.Sprintf("stream_%d", rt.now().Unix()),
			Logs:     logs,
		}, nil
	}
}

// readLogsFromPath reads logs from a file
func readLogsFromPath(logPath string, fromPosition int, maxLines int) ([]LogEntry, error) {
	data, err := os.ReadFile(logPath)
	if err != nil {
		if os.IsNotExist(err) {
			return []LogEntry{}, nil
		}
		return nil, err
	}

	// Parse NDJSON log file
	lines := splitLines(data)
	var logs []LogEntry
	entriesFound := 0

	// Iterate from the end (most recent logs first)
	for i := len(lines) - 1; i >= 0; i-- {
		line := lines[i]
		if len(line) == 0 {
			continue
		}

		// Skip entries until we reach fromPosition
		if entriesFound < fromPosition {
			entriesFound++
			continue
		}

		if len(logs) >= maxLines {
			break
		}

		var entry struct {
			Level   string    `json:"level"`
			Time    time.Time `json:"time"`
			Message string    `json:"msg"`
		}

		if err := json.Unmarshal(line, &entry); err != nil {
			// Skip unparseable lines
			continue
		}

		// Prepend to maintain chronological order
		logs = append([]LogEntry{{
			Level:   entry.Level,
			Time:    entry.Time,
			Message: entry.Message,
		}}, logs...)
		entriesFound++
	}

	return logs, nil
}

func splitLines(data []byte) [][]byte {
	var lines [][]byte
	start := 0
	for i, b := range data {
		if b == '\n' {
			lines = append(lines, data[start:i])
			start = i + 1
		}
	}
	if start < len(data) {
		lines = append(lines, data[start:])
	}
	return lines
}

// Codex-compatible session/turn types

// ThreadStartRequest represents parameters for thread/start
type ThreadStartRequest struct {
	// ExtendedContext is optional context for the thread
	ExtendedContext map[string]interface{} `json:"extended_context,omitempty"`
}

// ThreadStartResponse is the response for thread/start
type ThreadStartResponse struct {
	ThreadID string `json:"thread_id"`
	// Holon maps thread concept to controller session
	SessionID string `json:"session_id"`
	StartedAt string `json:"started_at"`
}

// TurnStartRequest represents parameters for turn/start
type TurnStartRequest struct {
	ThreadID string `json:"thread_id,omitempty"`
	// ExtendedContext is optional context for the turn
	ExtendedContext map[string]interface{} `json:"extended_context,omitempty"`
}

// TurnStartResponse is the response for turn/start
type TurnStartResponse struct {
	TurnID string `json:"turn_id"`
	// In Holon, a turn maps to an event processing cycle
	State string `json:"state"`
	StartedAt string `json:"started_at"`
}

// TurnInterruptRequest represents parameters for turn/interrupt
type TurnInterruptRequest struct {
	TurnID string `json:"turn_id,omitempty"`
	Reason string `json:"reason,omitempty"`
}

// TurnInterruptResponse is the response for turn/interrupt
type TurnInterruptResponse struct {
	TurnID string `json:"turn_id"`
	State string `json:"state"`
	InterruptedAt string `json:"interrupted_at"`
	Message string `json:"message"`
}

// HandleThreadStart is the JSON-RPC handler for thread/start
// This maps to starting/resuming the controller session in Holon
func (rt *Runtime) HandleThreadStart(params json.RawMessage) (interface{}, *JSONRPCError) {
	var req ThreadStartRequest
	if len(params) > 0 && string(params) != "null" {
		if err := json.Unmarshal(params, &req); err != nil {
			return nil, NewJSONRPCError(ErrCodeInvalidParams, fmt.Sprintf("invalid params: %s", err))
		}
	}

	// In Holon, thread maps to controller session
	// Generate a new session ID for this thread
	sessionID := fmt.Sprintf("thread_%d", rt.now().UnixNano())
	rt.SetControllerSession(sessionID)

	// Resume if paused to ensure thread is active
	if rt.IsPaused() {
		if err := rt.Resume(); err != nil {
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to resume: %s", err))
		}
	}

	return ThreadStartResponse{
		ThreadID:   sessionID,
		SessionID:  sessionID,
		StartedAt:  rt.now().Format(time.RFC3339),
	}, nil
}

// HandleTurnStart is the JSON-RPC handler for turn/start
// This maps to starting a new turn (event processing cycle) in Holon
func (rt *Runtime) HandleTurnStart(params json.RawMessage) (interface{}, *JSONRPCError) {
	var req TurnStartRequest
	if len(params) > 0 && string(params) != "null" {
		if err := json.Unmarshal(params, &req); err != nil {
			return nil, NewJSONRPCError(ErrCodeInvalidParams, fmt.Sprintf("invalid params: %s", err))
		}
	}

	// In Holon, a turn represents an event processing cycle
	// Resume if paused to ensure turn can process
	if rt.IsPaused() {
		if err := rt.Resume(); err != nil {
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to resume: %s", err))
		}
	}

	// Generate turn ID
	turnID := fmt.Sprintf("turn_%d", rt.now().UnixNano())

	return TurnStartResponse{
		TurnID:    turnID,
		State:     "active",
		StartedAt: rt.now().Format(time.RFC3339),
	}, nil
}

// HandleTurnInterrupt is the JSON-RPC handler for turn/interrupt
// This maps to pausing event processing in Holon
func (rt *Runtime) HandleTurnInterrupt(params json.RawMessage) (interface{}, *JSONRPCError) {
	var req TurnInterruptRequest
	if len(params) > 0 && string(params) != "null" {
		if err := json.Unmarshal(params, &req); err != nil {
			return nil, NewJSONRPCError(ErrCodeInvalidParams, fmt.Sprintf("invalid params: %s", err))
		}
	}

	// In Holon, interrupting a turn maps to pausing the runtime
	if err := rt.Pause(); err != nil {
		return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to interrupt: %s", err))
	}

	message := "Turn interrupted"
	if req.Reason != "" {
		message = fmt.Sprintf("Turn interrupted: %s", req.Reason)
	}

	// Use provided turn ID or generate one
	turnID := req.TurnID
	if turnID == "" {
		turnID = fmt.Sprintf("turn_%d", rt.now().UnixNano())
	}

	return TurnInterruptResponse{
		TurnID:        turnID,
		State:         "interrupted",
		InterruptedAt: rt.now().Format(time.RFC3339),
		Message:       message,
	}, nil
}

package serve

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
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
	defaultTurnIdleTTL  = 10 * time.Minute

	// DefaultPort is the default port for holon serve control plane and webhook/server
	DefaultPort = 8080
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
	// defaultSessionID is used when the persisted ControllerSession is empty.
	// This provides an immediately usable interaction surface for serve.
	defaultSessionID string
	noDefaultSession bool
	now              func() time.Time
	mu               sync.Mutex
	broadcaster      *NotificationBroadcaster
	turns            map[string]*activeTurn
	turnIdleTTL      time.Duration
	dispatcher       TurnDispatcher
}

type activeTurn struct {
	ID              string
	ThreadID        string
	StartedAt       time.Time
	CompletionTimer *time.Timer
	Generation      uint64
}

// TurnDispatcher handles user turn input and forwards it to the real controller runtime.
type TurnDispatcher func(ctx context.Context, req TurnStartRequest, turnID string) error

// NewRuntime creates a new runtime manager
func NewRuntime(stateDir string) (*Runtime, error) {
	return NewRuntimeWithOptions(stateDir, RuntimeOptions{
		DefaultSessionID: "main",
	})
}

type RuntimeOptions struct {
	// DefaultSessionID is the thread/session id that will be created on startup
	// when no persisted ControllerSession exists.
	// If empty, "main" is used.
	DefaultSessionID string
	// NoDefaultSession disables creating/loading a default session id.
	NoDefaultSession bool
}

func NewRuntimeWithOptions(stateDir string, opts RuntimeOptions) (*Runtime, error) {
	if err := os.MkdirAll(stateDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create runtime state dir: %w", err)
	}

	defaultID := strings.TrimSpace(opts.DefaultSessionID)
	if defaultID == "" {
		defaultID = "main"
	}

	rt := &Runtime{
		statePath:        filepath.Join(stateDir, "runtime-state.json"),
		now:              time.Now,
		turns:            make(map[string]*activeTurn),
		turnIdleTTL:      defaultTurnIdleTTL,
		defaultSessionID: defaultID,
		noDefaultSession: opts.NoDefaultSession,
		state: RuntimeState{
			State:           RuntimeStateRunning,
			EventsProcessed: 0,
		},
	}

	if err := rt.load(); err != nil {
		return nil, err
	}

	if err := rt.ensureDefaultSession(); err != nil {
		return nil, err
	}

	return rt, nil
}

func (rt *Runtime) ensureDefaultSession() error {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	if rt.noDefaultSession {
		return nil
	}
	if strings.TrimSpace(rt.state.ControllerSession) != "" {
		return nil
	}
	// Persist only when we actually transition from an empty controller session
	// to the default session id.
	rt.state.ControllerSession = rt.defaultSessionID
	if err := rt.save(); err != nil {
		return err
	}
	return nil
}

func (rt *Runtime) effectiveSessionID() string {
	state := rt.GetState()
	if strings.TrimSpace(state.ControllerSession) != "" {
		return strings.TrimSpace(state.ControllerSession)
	}
	if rt.noDefaultSession {
		return ""
	}
	return strings.TrimSpace(rt.defaultSessionID)
}

// SetBroadcaster injects a notification broadcaster for turn/thread/item events.
func (rt *Runtime) SetBroadcaster(b *NotificationBroadcaster) {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	rt.broadcaster = b
}

func (rt *Runtime) setTurnIdleTTLForTest(ttl time.Duration) {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	rt.turnIdleTTL = ttl
}

// SetTurnDispatcher injects the runtime dispatcher for turn/start requests.
func (rt *Runtime) SetTurnDispatcher(dispatcher TurnDispatcher) {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	rt.dispatcher = dispatcher
}

func (rt *Runtime) getBroadcaster() *NotificationBroadcaster {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	return rt.broadcaster
}

func (rt *Runtime) getTurnDispatcher() TurnDispatcher {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	return rt.dispatcher
}

func (rt *Runtime) emitThreadNotification(n ThreadNotification) {
	if broadcaster := rt.getBroadcaster(); broadcaster != nil {
		broadcaster.BroadcastThreadNotification(n)
	}
}

func (rt *Runtime) emitTurnNotification(n TurnNotification) {
	if broadcaster := rt.getBroadcaster(); broadcaster != nil {
		broadcaster.BroadcastTurnNotification(n)
	}
}

func (rt *Runtime) emitItemNotification(n ItemNotification) {
	if broadcaster := rt.getBroadcaster(); broadcaster != nil {
		broadcaster.BroadcastItemNotification(n)
	}
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
	threadID := rt.GetState().ControllerSession
	if threadID != "" {
		notif := NewThreadNotification(threadID, ThreadNotificationPaused, StatePaused)
		rt.emitThreadNotification(notif)
	}

	return PauseResponse{
		Success: true,
		Message: "Paused event processing",
	}, nil
}

// HandleResume is the JSON-RPC handler for holon/resume
func (rt *Runtime) HandleResume(params json.RawMessage) (interface{}, *JSONRPCError) {
	if !rt.IsPaused() {
		return ResumeResponse{
			Success: true,
			Message: "Runtime already running",
		}, nil
	}
	if err := rt.Resume(); err != nil {
		return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to resume: %s", err))
	}
	threadID := rt.GetState().ControllerSession
	if threadID != "" {
		notif := NewThreadNotification(threadID, ThreadNotificationResumed, StateRunning)
		rt.emitThreadNotification(notif)
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
	ThreadID string             `json:"thread_id,omitempty"`
	Input    []TurnInputMessage `json:"input,omitempty"`
	// ExtendedContext is optional context for the turn
	ExtendedContext map[string]interface{} `json:"extended_context,omitempty"`
}

// TurnSteerRequest represents parameters for turn/steer
type TurnSteerRequest struct {
	TurnID string             `json:"turn_id,omitempty"`
	Input  []TurnInputMessage `json:"input,omitempty"`
	// ExtendedContext is optional context for steering
	ExtendedContext map[string]interface{} `json:"extended_context,omitempty"`
}

// TurnInputMessage represents a user message item for turn/start or turn/steer.
type TurnInputMessage struct {
	Type    string                 `json:"type,omitempty"`
	Role    string                 `json:"role,omitempty"`
	Content []TurnInputContentPart `json:"content,omitempty"`
}

// TurnInputContentPart represents one message content fragment.
type TurnInputContentPart struct {
	Type string `json:"type,omitempty"`
	Text string `json:"text,omitempty"`
}

// TurnStartResponse is the response for turn/start
type TurnStartResponse struct {
	TurnID string `json:"turn_id"`
	// In Holon, a turn maps to an event processing cycle
	State     string `json:"state"`
	StartedAt string `json:"started_at"`
}

// TurnSteerResponse is the response for turn/steer.
type TurnSteerResponse struct {
	TurnID        string `json:"turn_id"`
	State         string `json:"state"`
	AcceptedItems int    `json:"accepted_items"`
	AcceptedAt    string `json:"accepted_at"`
}

// TurnInterruptRequest represents parameters for turn/interrupt
type TurnInterruptRequest struct {
	TurnID string `json:"turn_id,omitempty"`
	Reason string `json:"reason,omitempty"`
}

// TurnInterruptResponse is the response for turn/interrupt
type TurnInterruptResponse struct {
	TurnID        string `json:"turn_id"`
	State         string `json:"state"`
	InterruptedAt string `json:"interrupted_at"`
	Message       string `json:"message"`
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
	threadNotif := NewThreadNotification(sessionID, ThreadNotificationStarted, StateRunning)
	rt.emitThreadNotification(threadNotif)

	// Resume if paused to ensure thread is active
	if rt.IsPaused() {
		if err := rt.Resume(); err != nil {
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to resume: %s", err))
		}
	}

	return ThreadStartResponse{
		ThreadID:  sessionID,
		SessionID: sessionID,
		StartedAt: rt.now().Format(time.RFC3339),
	}, nil
}

func newInvalidParamFieldError(field string, reason string) *JSONRPCError {
	rpcErr, err := NewJSONRPCErrorWithData(ErrCodeInvalidParams, reason, map[string]string{
		"field":  field,
		"reason": reason,
	})
	if err != nil {
		return NewJSONRPCError(ErrCodeInvalidParams, reason)
	}
	return rpcErr
}

func validateTurnInput(input []TurnInputMessage) ([]TurnInputMessage, []string, *JSONRPCError) {
	if len(input) == 0 {
		return nil, nil, newInvalidParamFieldError("input", "input is required")
	}

	normalized := make([]TurnInputMessage, 0, len(input))
	texts := make([]string, 0, len(input))
	for idx, item := range input {
		itemType := strings.TrimSpace(item.Type)
		if itemType == "" {
			itemType = "message"
		}
		if itemType != "message" {
			return nil, nil, newInvalidParamFieldError(
				fmt.Sprintf("input[%d].type", idx),
				fmt.Sprintf("input[%d].type must be 'message'", idx),
			)
		}

		role := strings.TrimSpace(item.Role)
		if role == "" {
			role = "user"
		}

		if len(item.Content) == 0 {
			return nil, nil, newInvalidParamFieldError(
				fmt.Sprintf("input[%d].content", idx),
				fmt.Sprintf("input[%d].content is required", idx),
			)
		}

		normalizedParts := make([]TurnInputContentPart, 0, len(item.Content))
		hasText := false
		for partIdx, part := range item.Content {
			partType := strings.TrimSpace(part.Type)
			if partType == "" {
				partType = "input_text"
			}
			if partType != "input_text" && partType != "text" {
				return nil, nil, newInvalidParamFieldError(
					fmt.Sprintf("input[%d].content[%d].type", idx, partIdx),
					fmt.Sprintf("input[%d].content[%d].type must be 'input_text' or 'text'", idx, partIdx),
				)
			}
			text := strings.TrimSpace(part.Text)
			if text == "" {
				return nil, nil, newInvalidParamFieldError(
					fmt.Sprintf("input[%d].content[%d].text", idx, partIdx),
					fmt.Sprintf("input[%d].content[%d].text is required", idx, partIdx),
				)
			}
			normalizedParts = append(normalizedParts, TurnInputContentPart{Type: "input_text", Text: text})
			texts = append(texts, text)
			hasText = true
		}
		if !hasText {
			return nil, nil, newInvalidParamFieldError(
				fmt.Sprintf("input[%d]", idx),
				fmt.Sprintf("input[%d] must include at least one input_text part", idx),
			)
		}

		normalized = append(normalized, TurnInputMessage{
			Type:    "message",
			Role:    role,
			Content: normalizedParts,
		})
	}

	return normalized, texts, nil
}

func (rt *Runtime) beginTurn(threadID string) (string, time.Time) {
	now := rt.now()
	turnID := fmt.Sprintf("turn_%d", now.UnixNano())

	rt.mu.Lock()
	rt.turns[turnID] = &activeTurn{
		ID:        turnID,
		ThreadID:  threadID,
		StartedAt: now,
	}
	rt.mu.Unlock()

	rt.scheduleTurnAutoComplete(turnID)
	return turnID, now
}

func (rt *Runtime) scheduleTurnAutoComplete(turnID string) {
	rt.mu.Lock()
	defer rt.mu.Unlock()

	turn, ok := rt.turns[turnID]
	if !ok {
		return
	}
	if turn.CompletionTimer != nil {
		turn.CompletionTimer.Stop()
	}
	turn.Generation++
	generation := turn.Generation
	ttl := rt.turnIdleTTL
	if ttl <= 0 {
		ttl = defaultTurnIdleTTL
	}
	turn.CompletionTimer = time.AfterFunc(ttl, func() {
		rt.completeTurn(turnID, generation)
	})
}

func (rt *Runtime) completeTurn(turnID string, generation uint64) {
	var threadID string
	var startedAt time.Time
	rt.mu.Lock()
	if existing, ok := rt.turns[turnID]; ok {
		if existing.Generation != generation {
			rt.mu.Unlock()
			return
		}
		threadID = existing.ThreadID
		startedAt = existing.StartedAt
		delete(rt.turns, turnID)
	}
	rt.mu.Unlock()

	if threadID == "" {
		return
	}

	notif := NewTurnNotification(turnID, TurnNotificationCompleted, StateCompleted)
	notif.ThreadID = threadID
	notif.StartedAt = startedAt.Format(time.RFC3339)
	notif.CompletedAt = rt.now().Format(time.RFC3339)
	rt.emitTurnNotification(notif)
}

func (rt *Runtime) loadTurn(turnID string) (activeTurn, bool) {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	turn, ok := rt.turns[turnID]
	if !ok || turn == nil {
		return activeTurn{}, false
	}
	return activeTurn{
		ID:        turn.ID,
		ThreadID:  turn.ThreadID,
		StartedAt: turn.StartedAt,
	}, true
}

func (rt *Runtime) stopAndRemoveTurn(turnID string) (activeTurn, bool) {
	rt.mu.Lock()
	defer rt.mu.Unlock()
	turn, ok := rt.turns[turnID]
	if !ok {
		return activeTurn{}, false
	}
	snapshot := activeTurn{
		ID:        turn.ID,
		ThreadID:  turn.ThreadID,
		StartedAt: turn.StartedAt,
	}
	if turn.CompletionTimer != nil {
		turn.CompletionTimer.Stop()
	}
	delete(rt.turns, turnID)
	return snapshot, true
}

// HandleTurnAck completes or interrupts an active turn from external runtime acknowledgements.
// Returns true when the ack matched an active turn.
func (rt *Runtime) HandleTurnAck(turnID string, success bool, message string) bool {
	activeTurn, ok := rt.stopAndRemoveTurn(strings.TrimSpace(turnID))
	if !ok {
		return false
	}

	if success {
		if strings.TrimSpace(message) != "" {
			item := NewItemNotification(fmt.Sprintf("item_%d", rt.now().UnixNano()), ItemNotificationCreated, StateCompleted, map[string]interface{}{
				"type": "message",
				"role": "assistant",
				"content": []TurnInputContentPart{
					{Type: "text", Text: strings.TrimSpace(message)},
				},
			})
			item.ThreadID = activeTurn.ThreadID
			item.TurnID = activeTurn.ID
			rt.emitItemNotification(item)
		}
		notif := NewTurnNotification(activeTurn.ID, TurnNotificationCompleted, StateCompleted)
		notif.ThreadID = activeTurn.ThreadID
		notif.StartedAt = activeTurn.StartedAt.Format(time.RFC3339)
		notif.CompletedAt = rt.now().Format(time.RFC3339)
		if strings.TrimSpace(message) != "" {
			notif.Message = message
		}
		rt.emitTurnNotification(notif)
	} else {
		notif := NewTurnNotification(activeTurn.ID, TurnNotificationInterrupted, StateInterrupted)
		notif.ThreadID = activeTurn.ThreadID
		notif.StartedAt = activeTurn.StartedAt.Format(time.RFC3339)
		if strings.TrimSpace(message) != "" {
			notif.Message = message
		}
		rt.emitTurnNotification(notif)
	}

	return true
}

func (rt *Runtime) emitUserInputItems(threadID, turnID string, input []TurnInputMessage) {
	if rt.getBroadcaster() == nil {
		return
	}
	for _, item := range input {
		itemID := fmt.Sprintf("item_%d", rt.now().UnixNano())
		notif := NewItemNotification(itemID, ItemNotificationCreated, StateActive, map[string]interface{}{
			"type":    item.Type,
			"role":    item.Role,
			"content": item.Content,
		})
		notif.ThreadID = threadID
		notif.TurnID = turnID
		rt.emitItemNotification(notif)
	}
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
	if strings.TrimSpace(req.ThreadID) == "" {
		req.ThreadID = rt.effectiveSessionID()
	}
	if strings.TrimSpace(req.ThreadID) == "" {
		return nil, newInvalidParamFieldError("thread_id", "thread_id is required")
	}
	req.ThreadID = strings.TrimSpace(req.ThreadID)
	normalizedInput, _, rpcErr := validateTurnInput(req.Input)
	if rpcErr != nil {
		return nil, rpcErr
	}

	// In Holon, a turn represents an event processing cycle
	// Resume if paused to ensure turn can process
	if rt.IsPaused() {
		if err := rt.Resume(); err != nil {
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to resume: %s", err))
		}
	}
	rt.SetControllerSession(req.ThreadID)

	turnID, startedAt := rt.beginTurn(req.ThreadID)
	req.Input = normalizedInput

	turnStarted := NewTurnNotification(turnID, TurnNotificationStarted, StateActive)
	turnStarted.ThreadID = req.ThreadID
	turnStarted.StartedAt = startedAt.Format(time.RFC3339)
	rt.emitTurnNotification(turnStarted)
	rt.emitUserInputItems(req.ThreadID, turnID, normalizedInput)
	if dispatcher := rt.getTurnDispatcher(); dispatcher != nil {
		if err := dispatcher(context.Background(), req, turnID); err != nil {
			if _, removed := rt.stopAndRemoveTurn(turnID); !removed {
				rt.emitTurnNotification(TurnNotification{
					TurnID:   turnID,
					Type:     TurnNotificationInterrupted,
					State:    StateInterrupted,
					ThreadID: req.ThreadID,
					Message:  "turn dispatch failed and cleanup could not confirm removal",
				})
			}
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to dispatch turn %s for thread %s: %s", turnID, req.ThreadID, err))
		}
	}
	return TurnStartResponse{
		TurnID:    turnID,
		State:     "active",
		StartedAt: startedAt.Format(time.RFC3339),
	}, nil
}

// HandleTurnSteer is the JSON-RPC handler for turn/steer.
func (rt *Runtime) HandleTurnSteer(params json.RawMessage) (interface{}, *JSONRPCError) {
	var req TurnSteerRequest
	if len(params) > 0 && string(params) != "null" {
		if err := json.Unmarshal(params, &req); err != nil {
			return nil, NewJSONRPCError(ErrCodeInvalidParams, fmt.Sprintf("invalid params: %s", err))
		}
	}
	if strings.TrimSpace(req.TurnID) == "" {
		return nil, newInvalidParamFieldError("turn_id", "turn_id is required")
	}
	normalizedInput, _, rpcErr := validateTurnInput(req.Input)
	if rpcErr != nil {
		return nil, rpcErr
	}

	turn, ok := rt.loadTurn(req.TurnID)
	if !ok {
		return nil, newInvalidParamFieldError("turn_id", "turn_id is not active")
	}

	rt.emitUserInputItems(turn.ThreadID, turn.ID, normalizedInput)
	if dispatcher := rt.getTurnDispatcher(); dispatcher != nil {
		steerAsTurn := TurnStartRequest{
			ThreadID:        turn.ThreadID,
			Input:           normalizedInput,
			ExtendedContext: req.ExtendedContext,
		}
		if err := dispatcher(context.Background(), steerAsTurn, turn.ID); err != nil {
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to dispatch turn/steer for turn %s: %s", turn.ID, err))
		}
	}
	rt.scheduleTurnAutoComplete(turn.ID)

	return TurnSteerResponse{
		TurnID:        turn.ID,
		State:         StateActive,
		AcceptedItems: len(normalizedInput),
		AcceptedAt:    rt.now().Format(time.RFC3339),
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

	message := "Turn interrupted"
	if req.Reason != "" {
		message = fmt.Sprintf("Turn interrupted: %s", req.Reason)
	}

	// Use provided turn ID or generate one
	turnID := req.TurnID
	if turnID == "" {
		// In Holon, interrupting without turn_id maps to pausing the runtime.
		if err := rt.Pause(); err != nil {
			return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to interrupt: %s", err))
		}
		turnID = fmt.Sprintf("turn_%d", rt.now().UnixNano())
		turnInterrupted := NewTurnNotification(turnID, TurnNotificationInterrupted, StateInterrupted)
		turnInterrupted.Message = message
		rt.emitTurnNotification(turnInterrupted)
	} else {
		activeTurn, ok := rt.stopAndRemoveTurn(turnID)
		if !ok {
			return nil, newInvalidParamFieldError("turn_id", "turn_id is not active")
		}
		turnInterrupted := NewTurnNotification(turnID, TurnNotificationInterrupted, StateInterrupted)
		turnInterrupted.ThreadID = activeTurn.ThreadID
		turnInterrupted.StartedAt = activeTurn.StartedAt.Format(time.RFC3339)
		turnInterrupted.Message = message
		rt.emitTurnNotification(turnInterrupted)
	}

	if req.TurnID != "" {
		// Keep runtime active for targeted turn interruption.
		if rt.IsPaused() {
			if err := rt.Resume(); err != nil {
				return nil, NewJSONRPCError(ErrCodeInternalError, fmt.Sprintf("failed to resume runtime after turn interrupt: %s", err))
			}
		}
	}

	return TurnInterruptResponse{
		TurnID:        turnID,
		State:         "interrupted",
		InterruptedAt: rt.now().Format(time.RFC3339),
		Message:       message,
	}, nil
}

package serve

import (
	"encoding/json"
	"fmt"
	"time"
)

// Notification represents a server-sent notification following Codex protocol
// Notifications are JSON-RPC messages sent from server to client
type Notification struct {
	JSONRPC string          `json:"jsonrpc"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
}

// NotificationParams is the base interface for notification parameters
type NotificationParams interface{}

// ItemNotification represents an item lifecycle notification
// Corresponds to Codex "item/*" notifications
type ItemNotification struct {
	ItemID    string                 `json:"item_id"`
	Type      string                 `json:"type"` // created, updated, deleted
	Status    string                 `json:"status"`
	Content   map[string]interface{} `json:"content,omitempty"`
	Timestamp string                 `json:"timestamp"`
	ThreadID  string                 `json:"thread_id,omitempty"`
	TurnID    string                 `json:"turn_id,omitempty"`
}

// TurnNotification represents a turn lifecycle notification
// Corresponds to Codex "turn/*" notifications
type TurnNotification struct {
	TurnID      string `json:"turn_id"`
	Type        string `json:"type"` // started, completed, interrupted
	State       string `json:"state"`
	ThreadID    string `json:"thread_id,omitempty"`
	StartedAt   string `json:"started_at,omitempty"`
	CompletedAt string `json:"completed_at,omitempty"`
	Message     string `json:"message,omitempty"`
}

// TurnProgressNotification represents non-terminal turn lifecycle updates.
// Corresponds to Codex "turn/progress" notifications.
type TurnProgressNotification struct {
	TurnID    string `json:"turn_id"`
	ThreadID  string `json:"thread_id,omitempty"`
	State     string `json:"state"`
	Message   string `json:"message,omitempty"`
	EventID   string `json:"event_id,omitempty"`
	UpdatedAt string `json:"updated_at,omitempty"`
	ElapsedMS int64  `json:"elapsed_ms,omitempty"`
}

// ThreadNotification represents a thread lifecycle notification
// Corresponds to Codex "thread/*" notifications
type ThreadNotification struct {
	ThreadID  string `json:"thread_id"`
	Type      string `json:"type"` // started, resumed, paused, closed
	State     string `json:"state"`
	StartedAt string `json:"started_at,omitempty"`
	Message   string `json:"message,omitempty"`
}

// LogNotification represents a log streaming notification
type LogNotification struct {
	StreamID string    `json:"stream_id"`
	Level    string    `json:"level"`
	Time     time.Time `json:"time"`
	Message  string    `json:"message"`
}

// NewItemNotification creates a new item notification
func NewItemNotification(itemID, notificationType, status string, content map[string]interface{}) ItemNotification {
	return ItemNotification{
		ItemID:    itemID,
		Type:      notificationType,
		Status:    status,
		Content:   content,
		Timestamp: time.Now().Format(time.RFC3339),
	}
}

// NewTurnNotification creates a new turn notification
func NewTurnNotification(turnID, notificationType, state string) TurnNotification {
	return TurnNotification{
		TurnID:    turnID,
		Type:      notificationType,
		State:     state,
		StartedAt: time.Now().Format(time.RFC3339),
	}
}

// NewTurnProgressNotification creates a new non-terminal turn progress notification.
func NewTurnProgressNotification(turnID, state string) TurnProgressNotification {
	return TurnProgressNotification{
		TurnID:    turnID,
		State:     state,
		UpdatedAt: time.Now().Format(time.RFC3339),
	}
}

// NewThreadNotification creates a new thread notification
func NewThreadNotification(threadID, notificationType, state string) ThreadNotification {
	return ThreadNotification{
		ThreadID:  threadID,
		Type:      notificationType,
		State:     state,
		StartedAt: time.Now().Format(time.RFC3339),
	}
}

// ToJSONRPCNotification converts an ItemNotification to a JSON-RPC notification
func (n ItemNotification) ToJSONRPCNotification() (Notification, error) {
	params, err := json.Marshal(n)
	if err != nil {
		return Notification{}, fmt.Errorf("failed to marshal item notification: %w", err)
	}

	method := fmt.Sprintf("item/%s", n.Type)
	return Notification{
		JSONRPC: "2.0",
		Method:  method,
		Params:  params,
	}, nil
}

// ToJSONRPCNotification converts a TurnNotification to a JSON-RPC notification
func (n TurnNotification) ToJSONRPCNotification() (Notification, error) {
	params, err := json.Marshal(n)
	if err != nil {
		return Notification{}, fmt.Errorf("failed to marshal turn notification: %w", err)
	}

	method := fmt.Sprintf("turn/%s", n.Type)
	return Notification{
		JSONRPC: "2.0",
		Method:  method,
		Params:  params,
	}, nil
}

// ToJSONRPCNotification converts a TurnProgressNotification to a JSON-RPC notification.
func (n TurnProgressNotification) ToJSONRPCNotification() (Notification, error) {
	params, err := json.Marshal(n)
	if err != nil {
		return Notification{}, fmt.Errorf("failed to marshal turn progress notification: %w", err)
	}

	return Notification{
		JSONRPC: "2.0",
		Method:  "turn/progress",
		Params:  params,
	}, nil
}

// ToJSONRPCNotification converts a ThreadNotification to a JSON-RPC notification
func (n ThreadNotification) ToJSONRPCNotification() (Notification, error) {
	params, err := json.Marshal(n)
	if err != nil {
		return Notification{}, fmt.Errorf("failed to marshal thread notification: %w", err)
	}

	method := fmt.Sprintf("thread/%s", n.Type)
	return Notification{
		JSONRPC: "2.0",
		Method:  method,
		Params:  params,
	}, nil
}

// Notification constants
const (
	// Item notification types
	ItemNotificationCreated = "created"
	ItemNotificationUpdated = "updated"
	ItemNotificationDeleted = "deleted"

	// Turn notification types
	TurnNotificationStarted     = "started"
	TurnNotificationCompleted   = "completed"
	TurnNotificationInterrupted = "interrupted"

	// Thread notification types
	ThreadNotificationStarted = "started"
	ThreadNotificationResumed = "resumed"
	ThreadNotificationPaused  = "paused"
	ThreadNotificationClosed  = "closed"

	// Notification states
	StateActive      = "active"
	StateCompleted   = "completed"
	StateInterrupted = "interrupted"
	StateRunning     = "running"
	StatePaused      = "paused"
	StateClosed      = "closed"
)

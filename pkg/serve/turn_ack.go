package serve

// TurnAckRecord carries turn completion status from the controller runtime
// back into the serve runtime.
type TurnAckRecord struct {
	EventID       string `json:"event_id,omitempty"`
	TurnID        string `json:"turn_id,omitempty"`
	ThreadID      string `json:"thread_id,omitempty"`
	Status        string `json:"status"`
	Message       string `json:"message,omitempty"`
	At            string `json:"at,omitempty"`
	WorkspaceRef  string `json:"workspace_ref,omitempty"`
	WorkspacePath string `json:"workspace_path,omitempty"`
}

// TurnAckSource exposes an in-memory stream of turn ack events.
type TurnAckSource interface {
	TurnAcks() <-chan TurnAckRecord
}

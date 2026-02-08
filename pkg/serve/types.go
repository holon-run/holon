package serve

import (
	"encoding/json"
	"time"
)

// EventEnvelope is the normalized internal event format for serve mode.
type EventEnvelope struct {
	ID        string          `json:"id"`
	Source    string          `json:"source"`
	Type      string          `json:"type"`
	At        time.Time       `json:"at"`
	Scope     EventScope      `json:"scope"`
	Subject   EventSubject    `json:"subject"`
	DedupeKey string          `json:"dedupe_key,omitempty"`
	Payload   json.RawMessage `json:"payload,omitempty"`
}

type EventScope struct {
	Tenant string `json:"tenant,omitempty"`
	Repo   string `json:"repo,omitempty"`
}

type EventSubject struct {
	Kind string `json:"kind,omitempty"`
	ID   string `json:"id,omitempty"`
}

type DecisionRecord struct {
	ID       string    `json:"id"`
	EventID  string    `json:"event_id"`
	Type     string    `json:"type"`
	Reason   string    `json:"reason,omitempty"`
	Skipped  bool      `json:"skipped,omitempty"`
	CreateAt time.Time `json:"created_at"`
}

type ActionResult struct {
	ID        string    `json:"id"`
	EventID   string    `json:"event_id"`
	Status    string    `json:"status"`
	Message   string    `json:"message,omitempty"`
	StartedAt time.Time `json:"started_at"`
	EndedAt   time.Time `json:"ended_at"`
}

package tui

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const tuiTraceEnvKey = "HOLON_TUI_TRACE_FILE"

type tuiDebugTracer struct {
	mu       sync.Mutex
	file     *os.File
	enc      *json.Encoder
	reported bool
	seq      atomic.Uint64
}

func newTUIDebugTracerFromEnv() *tuiDebugTracer {
	path := strings.TrimSpace(os.Getenv(tuiTraceEnvKey))
	if path == "" {
		return &tuiDebugTracer{}
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		fmt.Fprintf(os.Stderr, "holon tui: failed to open debug trace file %s: %v\n", path, err)
		return &tuiDebugTracer{}
	}
	enc := json.NewEncoder(f)
	enc.SetEscapeHTML(false)
	return &tuiDebugTracer{file: f, enc: enc}
}

func (t *tuiDebugTracer) enabled() bool {
	return t != nil && t.enc != nil
}

func (t *tuiDebugTracer) trace(kind string, fields map[string]interface{}) {
	if !t.enabled() {
		return
	}

	entry := make(map[string]interface{}, len(fields)+4)
	entry["ts"] = time.Now().UTC().Format(time.RFC3339Nano)
	entry["component"] = "tui"
	entry["kind"] = strings.TrimSpace(kind)
	entry["seq"] = t.seq.Add(1)
	for k, v := range fields {
		entry[k] = v
	}

	t.mu.Lock()
	defer t.mu.Unlock()
	if err := t.enc.Encode(entry); err != nil && !t.reported {
		t.reported = true
		fmt.Fprintf(os.Stderr, "holon tui: failed to write debug trace: %v\n", err)
	}
}

func (t *tuiDebugTracer) close() error {
	if t == nil || t.file == nil {
		return nil
	}
	t.mu.Lock()
	defer t.mu.Unlock()
	if err := t.file.Close(); err != nil {
		return fmt.Errorf("failed to close tui debug trace file: %w", err)
	}
	t.file = nil
	t.enc = nil
	return nil
}

func traceFieldsFromNotification(notif StreamNotification) map[string]interface{} {
	fields := map[string]interface{}{
		"method": strings.TrimSpace(notif.Method),
	}

	var base struct {
		ThreadID string          `json:"thread_id,omitempty"`
		TurnID   string          `json:"turn_id,omitempty"`
		EventID  string          `json:"event_id,omitempty"`
		ItemID   string          `json:"item_id,omitempty"`
		Content  json.RawMessage `json:"content,omitempty"`
	}
	if err := json.Unmarshal(notif.Params, &base); err == nil {
		if strings.TrimSpace(base.ThreadID) != "" {
			fields["thread_id"] = strings.TrimSpace(base.ThreadID)
		}
		if strings.TrimSpace(base.TurnID) != "" {
			fields["turn_id"] = strings.TrimSpace(base.TurnID)
		}
		if strings.TrimSpace(base.EventID) != "" {
			fields["event_id"] = strings.TrimSpace(base.EventID)
		}
		if strings.TrimSpace(base.ItemID) != "" {
			fields["item_id"] = strings.TrimSpace(base.ItemID)
		}
		if len(base.Content) > 0 {
			var c struct {
				Type    string `json:"type,omitempty"`
				Role    string `json:"role,omitempty"`
				EventID string `json:"event_id,omitempty"`
			}
			if err := json.Unmarshal(base.Content, &c); err == nil {
				if strings.TrimSpace(c.Type) != "" {
					fields["content_type"] = strings.TrimSpace(c.Type)
				}
				if strings.TrimSpace(c.Role) != "" {
					fields["content_role"] = strings.TrimSpace(c.Role)
				}
				if strings.TrimSpace(c.EventID) != "" {
					fields["content_event_id"] = strings.TrimSpace(c.EventID)
				}
			}
		}
	}

	return fields
}

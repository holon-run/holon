package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strconv"
	"testing"
)

func TestAppendJSONLine(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	path := filepath.Join(td, "events.ndjson")

	first := map[string]any{"id": "evt-1", "type": "issue_comment"}
	second := map[string]any{"id": "evt-2", "type": "pull_request"}

	if err := appendJSONLine(path, first); err != nil {
		t.Fatalf("append first line: %v", err)
	}
	if err := appendJSONLine(path, second); err != nil {
		t.Fatalf("append second line: %v", err)
	}

	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read channel file: %v", err)
	}

	lines := bytesToLines(raw)
	if len(lines) != 2 {
		t.Fatalf("line count = %d, want 2", len(lines))
	}

	var gotFirst map[string]any
	if err := json.Unmarshal([]byte(lines[0]), &gotFirst); err != nil {
		t.Fatalf("unmarshal first line: %v", err)
	}
	if gotFirst["id"] != "evt-1" {
		t.Fatalf("first id = %v, want evt-1", gotFirst["id"])
	}

	var gotSecond map[string]any
	if err := json.Unmarshal([]byte(lines[1]), &gotSecond); err != nil {
		t.Fatalf("unmarshal second line: %v", err)
	}
	if gotSecond["id"] != "evt-2" {
		t.Fatalf("second id = %v, want evt-2", gotSecond["id"])
	}
}

func TestSessionStatePathAndReadSessionID(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	h := &cliControllerHandler{stateDir: td}
	if got := h.sessionStatePath(); got != filepath.Join(td, "controller-state", "controller-session.json") {
		t.Fatalf("sessionStatePath() = %q", got)
	}
	if got := h.readSessionID(); got != "" {
		t.Fatalf("readSessionID() for missing file = %q, want empty", got)
	}

	if err := os.MkdirAll(filepath.Dir(h.sessionStatePath()), 0o755); err != nil {
		t.Fatalf("mkdir session dir: %v", err)
	}
	if err := os.WriteFile(h.sessionStatePath(), []byte(`{"session_id":"abc123"}`), 0o644); err != nil {
		t.Fatalf("write session state: %v", err)
	}
	if got := h.readSessionID(); got != "abc123" {
		t.Fatalf("readSessionID() = %q, want abc123", got)
	}
}

func TestCompactChannelBestEffortLocked(t *testing.T) {
	t.Parallel()

	td := t.TempDir()
	channelDir := filepath.Join(td, "controller-state")
	if err := os.MkdirAll(channelDir, 0o755); err != nil {
		t.Fatalf("mkdir controller-state: %v", err)
	}
	channelPath := filepath.Join(channelDir, "event-channel.ndjson")
	cursorPath := filepath.Join(channelDir, "event-channel.cursor")

	line1 := `{"id":"evt-1"}`
	line2 := `{"id":"evt-2"}`
	content := line1 + "\n" + line2 + "\n"
	if err := os.WriteFile(channelPath, []byte(content), 0o644); err != nil {
		t.Fatalf("write channel: %v", err)
	}
	cursor := len(line1) + 1
	if err := os.WriteFile(cursorPath, []byte(strconv.Itoa(cursor)), 0o644); err != nil {
		t.Fatalf("write cursor: %v", err)
	}

	h := &cliControllerHandler{
		stateDir:          td,
		controllerChannel: channelPath,
	}
	original := maxEventChannelSizeBytes
	maxEventChannelSizeBytes = 1
	defer func() {
		maxEventChannelSizeBytes = original
	}()

	h.compactChannelBestEffortLocked()

	gotChannel, err := os.ReadFile(channelPath)
	if err != nil {
		t.Fatalf("read channel after compact: %v", err)
	}
	if string(gotChannel) != line2+"\n" {
		t.Fatalf("channel after compact = %q, want %q", string(gotChannel), line2+"\n")
	}
	gotCursor, err := os.ReadFile(cursorPath)
	if err != nil {
		t.Fatalf("read cursor after compact: %v", err)
	}
	if string(gotCursor) != "0" {
		t.Fatalf("cursor after compact = %q, want 0", string(gotCursor))
	}
}

func bytesToLines(raw []byte) []string {
	text := string(raw)
	if text == "" {
		return nil
	}
	parts := make([]string, 0, 4)
	start := 0
	for i := 0; i < len(text); i++ {
		if text[i] != '\n' {
			continue
		}
		if i > start {
			parts = append(parts, text[start:i])
		}
		start = i + 1
	}
	return parts
}

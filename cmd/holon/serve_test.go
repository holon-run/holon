package main

import (
	"encoding/json"
	"os"
	"path/filepath"
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

package serve

import (
	"encoding/json"
	"fmt"
	"os"
	"sync"
)

type ndjsonWriter struct {
	mu   sync.Mutex
	file *os.File
	enc  *json.Encoder
}

func newNDJSONWriter(path string) (*ndjsonWriter, error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return nil, fmt.Errorf("failed to open ndjson file %q: %w", path, err)
	}
	enc := json.NewEncoder(f)
	enc.SetEscapeHTML(false)
	return &ndjsonWriter{file: f, enc: enc}, nil
}

func (w *ndjsonWriter) Write(v interface{}) error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if err := w.enc.Encode(v); err != nil {
		return fmt.Errorf("failed to write ndjson entry: %w", err)
	}
	return nil
}

func (w *ndjsonWriter) Close() error {
	w.mu.Lock()
	defer w.mu.Unlock()
	if err := w.file.Close(); err != nil {
		return fmt.Errorf("failed to close ndjson file: %w", err)
	}
	return nil
}

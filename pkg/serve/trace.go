package serve

import (
	"encoding/json"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	holonlog "github.com/holon-run/holon/pkg/log"
)

const serveTraceEnvKey = "HOLON_SERVE_TRACE_FILE"

type serveDebugTracer struct {
	mu       sync.Mutex
	file     *os.File
	enc      *json.Encoder
	reported bool
	seq      atomic.Uint64
}

func safeMapCapacity(base, extra int) int {
	maxInt := int(^uint(0) >> 1)
	if base < 0 {
		base = 0
	}
	if extra < 0 {
		extra = 0
	}
	if base > maxInt-extra {
		return maxInt
	}
	return base + extra
}

func newServeDebugTracerFromEnv() *serveDebugTracer {
	path := strings.TrimSpace(os.Getenv(serveTraceEnvKey))
	if path == "" {
		return &serveDebugTracer{}
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		holonlog.Warn("failed to open serve debug trace file", "path", path, "error", err)
		return &serveDebugTracer{}
	}
	enc := json.NewEncoder(f)
	enc.SetEscapeHTML(false)
	return &serveDebugTracer{file: f, enc: enc}
}

func (t *serveDebugTracer) enabled() bool {
	return t != nil && t.enc != nil
}

func (t *serveDebugTracer) trace(kind string, fields map[string]interface{}) {
	if !t.enabled() {
		return
	}

	entry := make(map[string]interface{}, safeMapCapacity(len(fields), 4))
	entry["ts"] = time.Now().UTC().Format(time.RFC3339Nano)
	entry["component"] = "serve"
	entry["kind"] = strings.TrimSpace(kind)
	entry["seq"] = t.seq.Add(1)
	for k, v := range fields {
		entry[k] = v
	}

	t.mu.Lock()
	defer t.mu.Unlock()
	if err := t.enc.Encode(entry); err != nil && !t.reported {
		t.reported = true
		holonlog.Warn("failed to write serve debug trace", "error", err)
	}
}

var (
	serveTraceOnce sync.Once
	serveTraceInst *serveDebugTracer
)

func serveTrace() *serveDebugTracer {
	serveTraceOnce.Do(func() {
		serveTraceInst = newServeDebugTracerFromEnv()
	})
	return serveTraceInst
}

func traceServe(kind string, fields map[string]interface{}) {
	serveTrace().trace(kind, fields)
}

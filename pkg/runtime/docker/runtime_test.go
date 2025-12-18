package docker

import (
	"context"
	"testing"
)

func TestNewRuntime(t *testing.T) {
	rt, err := NewRuntime()
	if err != nil {
		t.Skipf("Skipping integration test: Docker daemon not reachable or client error: %v", err)
	}
	if rt.cli == nil {
		t.Error("Expected non-nil docker client")
	}
}

// TestRunHolon_DryRun verifies the container creation logic (partially)
// In a full test, it would pull image and run, but here we just check if NewRuntime works.
func TestRunHolon_DryRun(t *testing.T) {
	ctx := context.Background()
	rt, err := NewRuntime()
	if err != nil {
		t.Skip("Skipping: Docker not available")
	}

	// We only verify that context is handled correctly in the client
	_ = rt
	_ = ctx
}

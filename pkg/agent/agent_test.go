package agent

import (
	"context"
	"testing"

	"github.com/jolestar/holon/pkg/agent/llm"
	v1 "github.com/jolestar/holon/pkg/api/v1"
)

func TestAgent_Run(t *testing.T) {
	spec := &v1.HolonSpec{
		Metadata: v1.Metadata{Name: "test-agent"},
		Goal:     v1.Goal{Description: "Test the ReAct loop"},
		Context:  v1.Context{Workspace: "/tmp"},
	}

	mock := &llm.MockProvider{
		Responses: []func(req llm.Request) (*llm.Response, error){
			// First turn: Agent wants to see what's in the dir
			llm.ToolUseResponse("call_1", "list_dir", `{"path": "."}`),
			// Second turn: Agent is satisfied and finishes
			llm.SimpleTextResponse("Execution complete."),
		},
	}

	a := NewAgent(mock, spec)
	ctx := context.Background()

	manifest, err := a.Run(ctx)
	if err != nil {
		t.Fatalf("Agent.Run failed: %v", err)
	}

	if manifest.Status != "completed" {
		t.Errorf("Expected status 'completed', got %s", manifest.Status)
	}

	if mock.CallCount != 2 {
		t.Errorf("Expected 2 calls to LLM, got %d", mock.CallCount)
	}
}

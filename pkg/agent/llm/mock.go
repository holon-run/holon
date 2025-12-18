package llm

import (
	"context"
	"fmt"
)

type MockProvider struct {
	Responses []func(req Request) (*Response, error)
	CallCount int
}

func (m *MockProvider) CreateMessage(ctx context.Context, req Request) (*Response, error) {
	if m.CallCount >= len(m.Responses) {
		return nil, fmt.Errorf("unexpected call to CreateMessage: call count %d, response count %d", m.CallCount, len(m.Responses))
	}
	resp, err := m.Responses[m.CallCount](req)
	m.CallCount++
	return resp, err
}

func SimpleTextResponse(text string) func(req Request) (*Response, error) {
	return func(req Request) (*Response, error) {
		return &Response{
			Role: "assistant",
			Content: []Content{
				{Type: "text", Text: text},
			},
			StopReason: "end_turn",
		}, nil
	}
}

func ToolUseResponse(id, name, inputJson string) func(req Request) (*Response, error) {
	return func(req Request) (*Response, error) {
		return &Response{
			Role: "assistant",
			Content: []Content{
				{Type: "tool_use", ToolUse: &ToolUse{
					ID:    id,
					Name:  name,
					Input: []byte(inputJson),
				}},
			},
			StopReason: "tool_use",
		}, nil
	}
}

package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"log"

	"github.com/jolestar/holon/pkg/agent/llm"
	"github.com/jolestar/holon/pkg/agent/tools"
	v1 "github.com/jolestar/holon/pkg/api/v1"
)

type Agent struct {
	client llm.Provider
	spec   *v1.HolonSpec
}

func NewAgent(provider llm.Provider, spec *v1.HolonSpec) *Agent {
	return &Agent{
		client: provider,
		spec:   spec,
	}
}

func (a *Agent) Run(ctx context.Context) (*v1.HolonManifest, error) {
	systemPrompt := fmt.Sprintf(`You are a software engineering agent. Your goal is: %s.
Working Directory: %s
Primary Files: %v

You have access to tools. Focus on achieving the goal step-by-step.
Always perform validation (e.g. run tests) before finishing.`,
		a.spec.Goal.Description, a.spec.Context.Workspace, a.spec.Context.Files)

	messages := []llm.Message{
		{
			Role: "user",
			Content: []llm.Content{
				{Type: "text", Text: "Please start the task."},
			},
		},
	}

	toolDefs := []llm.Tool{
		{
			Name:        "read_file",
			Description: "Read the content of a file.",
			InputSchema: map[string]interface{}{
				"type": "object",
				"properties": map[string]interface{}{
					"path": map[string]interface{}{"type": "string"},
				},
				"required": []string{"path"},
			},
		},
		{
			Name:        "write_file",
			Description: "Write content to a file.",
			InputSchema: map[string]interface{}{
				"type": "object",
				"properties": map[string]interface{}{
					"path":    map[string]interface{}{"type": "string"},
					"content": map[string]interface{}{"type": "string"},
				},
				"required": []string{"path", "content"},
			},
		},
		{
			Name:        "execute_command",
			Description: "Execute a shell command.",
			InputSchema: map[string]interface{}{
				"type": "object",
				"properties": map[string]interface{}{
					"command": map[string]interface{}{"type": "string"},
				},
				"required": []string{"command"},
			},
		},
		{
			Name:        "list_dir",
			Description: "List files in a directory.",
			InputSchema: map[string]interface{}{
				"type": "object",
				"properties": map[string]interface{}{
					"path": map[string]interface{}{"type": "string"},
				},
				"required": []string{"path"},
			},
		},
	}

	maxSteps := a.spec.Constraints.MaxSteps
	if maxSteps == 0 {
		maxSteps = 20
	}

	for i := 0; i < maxSteps; i++ {
		log.Printf("Step %d...", i+1)
		resp, err := a.client.CreateMessage(ctx, llm.Request{
			System:    systemPrompt,
			Messages:  messages,
			Tools:     toolDefs,
			MaxTokens: 4096,
		})
		if err != nil {
			return nil, err
		}

		// Append Assistant's thought/action
		messages = append(messages, llm.Message{
			Role:    resp.Role,
			Content: resp.Content,
		})

		if resp.StopReason == "end_turn" {
			log.Println("Agent task finished.")
			break
		}

		if resp.StopReason == "tool_use" {
			var toolResults []llm.Content
			for _, content := range resp.Content {
				if content.Type == "tool_use" {
					res := a.handleToolCall(content.ToolUse)
					toolResults = append(toolResults, llm.Content{
						Type: "tool_result",
						ToolResult: &llm.ToolResult{
							ToolUseID: content.ToolUse.ID,
							Content:   res,
						},
					})
				}
			}
			messages = append(messages, llm.Message{
				Role:    "user",
				Content: toolResults,
			})
		}
	}

	return &v1.HolonManifest{
		Status:  "completed",
		Outcome: "success",
	}, nil
}

func (a *Agent) handleToolCall(toolCall *llm.ToolUse) string {
	log.Printf("Executing tool %s...", toolCall.Name)
	var args map[string]string
	json.Unmarshal(toolCall.Input, &args)

	switch toolCall.Name {
	case "read_file":
		out, err := tools.ReadFile(args["path"])
		if err != nil {
			return fmt.Sprintf("Error: %v", err)
		}
		return out
	case "write_file":
		err := tools.WriteFile(args["path"], args["content"])
		if err != nil {
			return fmt.Sprintf("Error: %v", err)
		}
		return "File written successfully."
	case "execute_command":
		out, err := tools.ExecuteCommand(args["command"])
		if err != nil {
			return fmt.Sprintf("Error: %v\nOutput: %s", err, out)
		}
		return out
	case "list_dir":
		out, err := tools.ListDir(args["path"])
		if err != nil {
			return fmt.Sprintf("Error: %v", err)
		}
		return out
	default:
		return "Unknown tool."
	}
}

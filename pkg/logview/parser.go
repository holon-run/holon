package logview

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// Parser defines the interface for parsing execution logs
type Parser interface {
	// Parse reads the log and produces a structured, readable representation
	Parse(logPath string) (string, error)
	// Name returns the parser name
	Name() string
}

// ParserRegistry holds registered parsers keyed by agent name
var parsers = make(map[string]Parser)

// RegisterParser registers a parser for a specific agent
func RegisterParser(agent string, p Parser) {
	parsers[agent] = p
}

// GetParser retrieves a parser for the given agent, returns nil if not found
func GetParser(agent string) Parser {
	return parsers[agent]
}

// LogEntry represents a single log line in Claude format
type ClaudeLogEntry struct {
	Type      string                 `json:"type"`
	Subtype   string                 `json:"subtype,omitempty"`
	SessionID string                 `json:"session_id,omitempty"`
	Message   *ClaudeMessage         `json:"message,omitempty"`
	Tools     []string               `json:"tools,omitempty"`
	Model     string                 `json:"model,omitempty"`
	Extra     map[string]interface{} `json:"-"`
}

// ClaudeMessage represents a message within a log entry
type ClaudeMessage struct {
	ID      string        `json:"id"`
	Type    string        `json:"type"`
	Role    string        `json:"role"`
	Model   string        `json:"model,omitempty"`
	Content []interface{} `json:"content"`
}

// ClaudeParser parses Claude Code agent logs
type ClaudeParser struct{}

// Name returns the parser name
func (p *ClaudeParser) Name() string {
	return "claude-code"
}

// Parse reads the Claude log and produces a structured, readable representation
func (p *ClaudeParser) Parse(logPath string) (string, error) {
	file, err := os.Open(logPath)
	if err != nil {
		return "", fmt.Errorf("failed to open log file: %w", err)
	}
	defer file.Close()

	var sb strings.Builder
	scanner := bufio.NewScanner(file)

	// Track session info
	var sessionID string
	var model string
	var tools []string

	lineNum := 0
	for scanner.Scan() {
		lineNum++
		line := scanner.Text()

		// Try to parse as JSON
		var entry ClaudeLogEntry
		if err := json.Unmarshal([]byte(line), &entry); err != nil {
			// Not a JSON line, write as-is with minimal formatting
			if line != "" {
				sb.WriteString(fmt.Sprintf("[RAW] %s\n", line))
			}
			continue
		}

		// Handle different entry types
		switch entry.Type {
		case "system":
			p.handleSystemEntry(&sb, &entry, &sessionID, &model, &tools)
		case "assistant":
			p.handleAssistantEntry(&sb, &entry)
		case "result":
			p.handleResultEntry(&sb, &entry)
		default:
			// Unknown type, write as-is
			sb.WriteString(fmt.Sprintf("[%s] %s\n", strings.ToUpper(entry.Type), line))
		}
	}

	if err := scanner.Err(); err != nil {
		return "", fmt.Errorf("error reading log file: %w", err)
	}

	return sb.String(), nil
}

func (p *ClaudeParser) handleSystemEntry(sb *strings.Builder, entry *ClaudeLogEntry, sessionID, model *string, tools *[]string) {
	// Initialize or update session info
	if entry.SessionID != "" && *sessionID == "" {
		*sessionID = entry.SessionID
		sb.WriteString("╔════════════════════════════════════════════════════════════╗\n")
		sb.WriteString("║                      SESSION START                          ║\n")
		sb.WriteString("╚════════════════════════════════════════════════════════════╝\n")
		sb.WriteString(fmt.Sprintf("Session ID: %s\n", entry.SessionID))
		if entry.Model != "" {
			*model = entry.Model
			sb.WriteString(fmt.Sprintf("Model: %s\n", entry.Model))
		}
		if entry.Subtype != "" {
			sb.WriteString(fmt.Sprintf("Subtype: %s\n", entry.Subtype))
		}
		if len(entry.Tools) > 0 {
			*tools = entry.Tools
			sb.WriteString(fmt.Sprintf("Tools: %s\n", strings.Join(entry.Tools, ", ")))
		}
		sb.WriteString("\n")
	}
}

func (p *ClaudeParser) handleAssistantEntry(sb *strings.Builder, entry *ClaudeLogEntry) {
	if entry.Message == nil {
		return
	}

	// Process message content blocks
	for _, block := range entry.Message.Content {
		switch v := block.(type) {
		case map[string]interface{}:
			blockType, _ := v["type"].(string)

			switch blockType {
			case "text":
				if text, ok := v["text"].(string); ok {
					// Check if this is a tool call indicator
					if strings.Contains(text, "I'll") || strings.Contains(text, "Let me") || strings.Contains(text, "I'm going to") {
						// Assistant reasoning
						sb.WriteString(fmt.Sprintf("🤔 [ASSISTANT] %s\n", text))
					} else {
						sb.WriteString(fmt.Sprintf("[TEXT] %s\n", text))
					}
				}
			case "tool_use":
				toolName, _ := v["name"].(string)
				toolInput, _ := v["input"].(map[string]interface{})

				sb.WriteString(fmt.Sprintf("\n🔧 [TOOL] %s", toolName))

				// Show relevant input parameters
				if toolInput != nil {
					if filePath, ok := toolInput["file_path"].(string); ok && filePath != "" {
						sb.WriteString(fmt.Sprintf(" -> %s", filepath.Base(filePath)))
					}
					if goal, ok := toolInput["goal"].(string); ok && goal != "" {
						sb.WriteString(fmt.Sprintf(" (goal: %s)", truncateString(goal, 50)))
					}
					if query, ok := toolInput["query"].(string); ok && query != "" {
						sb.WriteString(fmt.Sprintf(" (query: %s)", truncateString(query, 50)))
					}
				}
				sb.WriteString("\n")
			}
		}
	}
}

func (p *ClaudeParser) handleResultEntry(sb *strings.Builder, entry *ClaudeLogEntry) {
	subtype := entry.Subtype
	isError := false

	if ie, ok := entry.Extra["is_error"].(bool); ok {
		isError = ie
	}

	if isError {
		sb.WriteString(fmt.Sprintf("\n❌ [ERROR] Result: %s\n", subtype))
	} else {
		sb.WriteString(fmt.Sprintf("\n✅ [RESULT] %s\n", subtype))
	}

	// Extract result text if present
	if result, ok := entry.Extra["result"].(string); ok && result != "" {
		lines := strings.Split(result, "\n")
		if len(lines) > 0 && len(lines) <= 5 {
			for _, line := range lines {
				sb.WriteString(fmt.Sprintf("  %s\n", line))
			}
		} else if len(lines) > 5 {
			for i := 0; i < 3; i++ {
				sb.WriteString(fmt.Sprintf("  %s\n", lines[i]))
			}
			sb.WriteString(fmt.Sprintf("  ... (%d more lines)\n", len(lines)-3))
		}
	}
}

// truncateString truncates a string to a maximum length
func truncateString(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen] + "..."
}

// FallbackParser returns the raw log content for unknown agents
type FallbackParser struct{}

// Name returns the parser name
func (p *FallbackParser) Name() string {
	return "fallback"
}

// Parse returns the raw log content
func (p *FallbackParser) Parse(logPath string) (string, error) {
	file, err := os.ReadFile(logPath)
	if err != nil {
		return "", fmt.Errorf("failed to read log file: %w", err)
	}
	return string(file), nil
}

// ParseLog parses a log file using the appropriate parser based on the agent type
func ParseLog(manifestPath string) (string, error) {
	// Read manifest to get agent type
	manifestData, err := os.ReadFile(manifestPath)
	if err != nil {
		return "", fmt.Errorf("failed to read manifest: %w", err)
	}

	var manifest struct {
		Metadata struct {
			Agent string `json:"agent"`
		} `json:"metadata"`
	}

	if err := json.Unmarshal(manifestData, &manifest); err != nil {
		return "", fmt.Errorf("failed to parse manifest: %w", err)
	}

	// Determine log path
	manifestDir := filepath.Dir(manifestPath)
	logPath := filepath.Join(manifestDir, "evidence", "execution.log")

	// Check if log file exists
	if _, err := os.Stat(logPath); err != nil {
		return "", fmt.Errorf("log file not found: %s", logPath)
	}

	// Get parser for agent
	agent := manifest.Metadata.Agent
	if agent == "" {
		agent = "unknown"
	}

	var parser Parser
	if p := GetParser(agent); p != nil {
		parser = p
	} else {
		parser = &FallbackParser{}
	}

	// Parse log
	result, err := parser.Parse(logPath)
	if err != nil {
		return "", fmt.Errorf("parser %s failed: %w", parser.Name(), err)
	}

	return result, nil
}

// ParseLogFromPath parses a log file directly without using manifest
func ParseLogFromPath(logPath string) (string, error) {
	// Use fallback parser (raw output)
	parser := &FallbackParser{}
	return parser.Parse(logPath)
}

func init() {
	// Register built-in parsers
	RegisterParser("claude-code", &ClaudeParser{})
}

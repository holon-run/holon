package logview

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// Parser defines the interface for parsing execution logs
type Parser interface {
	// Parse reads the log and produces a structured, readable representation
	Parse(logPath string) (string, error)
	// Name returns the parser name
	Name() string
}

// ParserRegistry holds registered parsers keyed by agent name
var (
	parsers = make(map[string]Parser)
	parsersMutex sync.RWMutex
)

// RegisterParser registers a parser for a specific agent
func RegisterParser(agent string, p Parser) {
	parsersMutex.Lock()
	defer parsersMutex.Unlock()
	parsers[agent] = p
}

// GetParser retrieves a parser for the given agent, returns nil if not found
func GetParser(agent string) Parser {
	parsersMutex.RLock()
	defer parsersMutex.RUnlock()
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

// ToolResult represents a tool use result
type ToolResult struct {
	Type        string                 `json:"type"`
	ToolUseID   string                 `json:"tool_use_id"`
	Content     string                 `json:"content"`
	IsError     bool                   `json:"is_error,omitempty"`
	FileContent *FileContent           `json:"-"`
}

// FileContent represents file content in tool results
type FileContent struct {
	FilePath  string `json:"filePath"`
	Content   string `json:"content"`
	NumLines  int    `json:"numLines"`
	StartLine int    `json:"startLine"`
	TotalLines int   `json:"totalLines"`
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

	// Increase buffer size to handle large log lines (e.g., big JSON objects)
	// Start with 64KB and grow up to 10MB if needed
	const maxTokenSize = 10 * 1024 * 1024 // 10MB
	buf := make([]byte, 0, 64*1024)       // 64KB initial capacity
	scanner.Buffer(buf, maxTokenSize)

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
			// Not a JSON line, skip empty lines, write others with minimal formatting
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
		case "user":
			p.handleUserEntry(&sb, &entry)
		case "result":
			p.handleResultEntry(&sb, &entry)
		default:
			// Unknown type, skip silently
			continue
		}
	}

	if err := scanner.Err(); err != nil {
		return "", fmt.Errorf("error reading log file: %w", err)
	}

	return sb.String(), nil
}

func (p *ClaudeParser) handleSystemEntry(sb *strings.Builder, entry *ClaudeLogEntry, sessionID, model *string, tools *[]string) {
	// Show session info on first system entry or when session ID changes
	if entry.SessionID != "" {
		if *sessionID == "" {
			// First session entry
			sb.WriteString("╔════════════════════════════════════════════════════════════╗\n")
			sb.WriteString("║                      SESSION START                          ║\n")
			sb.WriteString("╚════════════════════════════════════════════════════════════╝\n")
		} else if entry.SessionID != *sessionID {
			// Session ID changed - show transition
			sb.WriteString("\n╔════════════════════════════════════════════════════════════╗\n")
			sb.WriteString("║                    SESSION TRANSITION                       ║\n")
			sb.WriteString("╚════════════════════════════════════════════════════════════╝\n")
		}

		*sessionID = entry.SessionID
		sb.WriteString(fmt.Sprintf("Session ID: %s\n", entry.SessionID))

		// Show model if present or changed
		if entry.Model != "" {
			if *model != entry.Model {
				*model = entry.Model
				sb.WriteString(fmt.Sprintf("Model: %s\n", entry.Model))
			}
		}

		// Always show subtype when present
		if entry.Subtype != "" {
			sb.WriteString(fmt.Sprintf("Subtype: %s\n", entry.Subtype))
		}

		// Show tools if present or changed
		if len(entry.Tools) > 0 {
			toolsChanged := len(*tools) != len(entry.Tools)
			if !toolsChanged {
				for i, t := range entry.Tools {
					if i >= len(*tools) || (*tools)[i] != t {
						toolsChanged = true
						break
					}
				}
			}
			if toolsChanged {
				*tools = entry.Tools
				sb.WriteString(fmt.Sprintf("Tools: %s\n", strings.Join(entry.Tools, ", ")))
			}
		}

		sb.WriteString("\n")
	}
}

func (p *ClaudeParser) handleAssistantEntry(sb *strings.Builder, entry *ClaudeLogEntry) {
	if entry.Message == nil {
		return
	}

	// Track whether we've seen any tool_use blocks
	seenToolUse := false

	// First pass: check if there are any tool_use blocks
	for _, block := range entry.Message.Content {
		if blockMap, ok := block.(map[string]interface{}); ok {
			if blockType, _ := blockMap["type"].(string); blockType == "tool_use" {
				seenToolUse = true
				break
			}
		}
	}

	// Second pass: process content blocks
	textBeforeTool := true
	for _, block := range entry.Message.Content {
		switch v := block.(type) {
		case map[string]interface{}:
			blockType, _ := v["type"].(string)

			switch blockType {
			case "text":
				if text, ok := v["text"].(string); ok && text != "" {
					// Clean up the text - remove excessive whitespace
					text = strings.TrimSpace(text)
					if text == "" {
						continue
					}

					// If text appears before tool_use, it's typically assistant reasoning
					if seenToolUse && textBeforeTool {
						sb.WriteString(fmt.Sprintf("💭 %s\n", text))
					} else if !seenToolUse {
						// Text without tools - direct response
						sb.WriteString(fmt.Sprintf("💬 %s\n", text))
					}
					// Skip text between tool_use and result (it's redundant)
				}

			case "tool_use":
				textBeforeTool = false
				toolName, _ := v["name"].(string)
				toolInput, _ := v["input"].(map[string]interface{})

				sb.WriteString(fmt.Sprintf("\n🔧 %s", toolName))

				// Show relevant input parameters
				if toolInput != nil {
					if filePath, ok := toolInput["file_path"].(string); ok && filePath != "" {
						sb.WriteString(fmt.Sprintf(" → %s", formatFilePath(filePath)))
					}
					if pattern, ok := toolInput["pattern"].(string); ok && pattern != "" {
						sb.WriteString(fmt.Sprintf(" (pattern: %s)", pattern))
					}
					if goal, ok := toolInput["goal"].(string); ok && goal != "" {
						sb.WriteString(fmt.Sprintf("\n   Goal: %s", truncateString(goal, 80)))
					}
					if prompt, ok := toolInput["prompt"].(string); ok && prompt != "" {
						sb.WriteString(fmt.Sprintf("\n   Prompt: %s", truncateString(prompt, 80)))
					}
					if query, ok := toolInput["query"].(string); ok && query != "" {
						sb.WriteString(fmt.Sprintf("\n   Query: %s", truncateString(query, 80)))
					}
					if description, ok := toolInput["description"].(string); ok && description != "" {
						sb.WriteString(fmt.Sprintf("\n   Desc: %s", truncateString(description, 80)))
					}
				}
				sb.WriteString("\n")
			}
		}
	}
}

// handleUserEntry processes user messages (tool results)
func (p *ClaudeParser) handleUserEntry(sb *strings.Builder, entry *ClaudeLogEntry) {
	if entry.Message == nil {
		return
	}

	for _, block := range entry.Message.Content {
		resultMap, ok := block.(map[string]interface{})
		if !ok {
			continue
		}

		resultType, _ := resultMap["type"].(string)
		if resultType != "tool_result" {
			continue
		}

		_, _ = resultMap["tool_use_id"].(string) // Reserved for future use
		isError, _ := resultMap["is_error"].(bool)

		// Extract content
		var contentStr string
		if content, ok := resultMap["content"].(string); ok {
			contentStr = content
		}

		// Check if this is file content
		if file, ok := resultMap["file"].(map[string]interface{}); ok {
			filePath, _ := file["filePath"].(string)
			fileContent, _ := file["content"].(string)

			if filePath != "" && fileContent != "" {
				// Show file read result
				lineCount := strings.Count(fileContent, "\n") + 1
				sb.WriteString(fmt.Sprintf("   ✓ Read %s (%d lines)\n", formatFilePath(filePath), lineCount))
				continue
			}
		}

		// Check if content is a large JSON string (unescaped)
		if strings.HasPrefix(contentStr, "{") || strings.HasPrefix(contentStr, "[") {
			// Try to parse as JSON to show structured info
			var jsonData interface{}
			if err := json.Unmarshal([]byte(contentStr), &jsonData); err == nil {
				// Successfully parsed JSON - show summary
				sb.WriteString(fmt.Sprintf("   ✓ Read JSON data\n"))
				continue
			}
		}

		// Check for error content
		if isError {
			sb.WriteString(fmt.Sprintf("   ❌ Error: %s\n", truncateString(cleanContent(contentStr), 200)))
		} else if contentStr != "" && len(contentStr) < 500 {
			// Show short non-error content
			cleaned := cleanContent(contentStr)
			if cleaned != "" {
				sb.WriteString(fmt.Sprintf("   → %s\n", truncateString(cleaned, 200)))
			}
		}
		// Skip long content (just show tool execution above)
	}
}

// truncateString truncates a string to a maximum length
func truncateString(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen] + "..."
}

// formatFilePath shortens file paths for better readability
func formatFilePath(path string) string {
	// Show just the filename for paths in common directories
	if strings.Contains(path, "/node_modules/") {
		return filepath.Base(path)
	}
	// For workspace files, show relative path with max 2 levels
	parts := strings.Split(path, "/")
	if len(parts) > 3 {
		return filepath.Join(parts[len(parts)-3:]...)
	}
	return filepath.Base(path)
}

// cleanContent removes escape sequences and cleans up content strings
func cleanContent(content string) string {
	// Remove common escape sequences
	cleaned := strings.ReplaceAll(content, "\\n", " ")
	cleaned = strings.ReplaceAll(cleaned, "\\t", " ")
	cleaned = strings.ReplaceAll(cleaned, "\\\"", "\"")
	cleaned = strings.ReplaceAll(cleaned, "\\\\", "\\")
	// Remove line numbers like "1→", "2→" etc.
	re := strings.Builder{}
	for _, line := range strings.Split(cleaned, "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "" {
			continue
		}
		// Skip lines with just line numbers
		if len(trimmed) < 10 && strings.Contains(trimmed, "→") {
			continue
		}
		re.WriteString(trimmed)
		re.WriteString(" ")
	}
	result := strings.TrimSpace(re.String())
	// Limit to reasonable length
	if len(result) > 1000 {
		result = result[:997] + "..."
	}
	return result
}

func (p *ClaudeParser) handleResultEntry(sb *strings.Builder, entry *ClaudeLogEntry) {
	subtype := entry.Subtype

	// Derive error indication from subtype
	lowered := strings.ToLower(subtype)
	isError := strings.Contains(lowered, "error") || strings.Contains(lowered, "failed") || strings.Contains(lowered, "failure")

	if isError {
		sb.WriteString(fmt.Sprintf("\n❌ Error: %s\n", subtype))
	} else if subtype != "" && subtype != "success" {
		sb.WriteString(fmt.Sprintf("\n✅ %s\n", subtype))
	}
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
	// Register the same parser under both names for compatibility:
	// - "claude-code": legacy name (current default)
	// - "agent-claude": standardized name (see https://github.com/holon-run/holon/issues/407)
	parser := &ClaudeParser{}
	RegisterParser("claude-code", parser)
	RegisterParser("agent-claude", parser)
}

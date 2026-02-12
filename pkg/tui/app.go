package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// App is the TUI application state
type App struct {
	client       *RPCClient
	status       *StatusResponse
	logs         []LogEntry
	logPosition  int
	err          error
	connected    bool
	lastUpdate   time.Time
	quitting     bool
	autoRefresh  bool
	refreshIndex int

	// Chat/conversation state
	threadID      string
	messages      []ConversationMessage
	inputText     string
	inputWidth    int
	sending       bool
	sendingError  error

	// Streaming
	streamCtx      context.Context
	streamCancel  context.CancelFunc
	streamActive  bool
}

// ConversationMessage represents a message in the conversation timeline
type ConversationMessage struct {
	ID        string    `json:"id"`
	Type      string    `json:"type"` // user, assistant, system, turn_lifecycle
	Timestamp time.Time `json:"timestamp"`
	Content   string    `json:"content"`
	State     string    `json:"state,omitempty"` // For turn lifecycle: active, completed, interrupted
}

// NewApp creates a new TUI application
func NewApp(client *RPCClient) *App {
	return &App{
		client:      client,
		logs:        make([]LogEntry, 0),
		autoRefresh: true,
		messages:    make([]ConversationMessage, 0),
		inputText:   "",
	}
}

// Styles
var (
	titleStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("cyan")).
			Bold(true).
			Padding(0, 1)

	statusStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("green")).
			Padding(0, 1)

	errorStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("red")).
			Padding(0, 1)

	pausedStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("yellow")).
			Padding(0, 1)

	borderStyle = lipgloss.NewStyle().
			Border(lipgloss.RoundedBorder()).
			BorderForeground(lipgloss.Color("blue"))

	helpStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("240")).
			Padding(0, 1)

	userMsgStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("blue")).
			Padding(0, 1)

	assistantMsgStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("green")).
			Padding(0, 1)

	systemMsgStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("magenta")).
			Padding(0, 1)

	inputStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("white")).
			Background(lipgloss.Color("blue")).
			Padding(0, 1)
)

// Messages
type statusRefreshMsg struct {
	status *StatusResponse
	err    error
}

type logRefreshMsg struct {
	logs []LogEntry
	err  error
}

type tickMsg time.Time

type streamStartedMsg struct {
	ctx    context.Context
	cancel context.CancelFunc
}

type streamErrorMsg struct {
	err error
}

type notificationMsg struct {
	notification StreamNotification
}

type messageSentMsg struct {
	response *TurnStartResponse
	err      error
}

// Init initializes the application
func (a *App) Init() tea.Cmd {
	return tea.Batch(
		a.refreshStatusCmd(),
		a.refreshLogsCmd(),
		a.startStreamCmd(),
		a.tick(),
	)
}

// Update handles messages and updates state
func (a *App) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.KeyMsg:
		switch msg.String() {
		case "q", "ctrl+c":
			a.quitting = true
			if a.streamCancel != nil {
				a.streamCancel()
			}
			return a, tea.Quit

		case "p":
			// Pause
			return a, a.sendPauseCmd()

		case "r":
			// Resume
			return a, a.sendResumeCmd()

		case "R":
			// Refresh
			return a, tea.Batch(a.refreshStatusCmd(), a.refreshLogsCmd())

		case " ":
			// Toggle auto-refresh
			a.autoRefresh = !a.autoRefresh
			if a.autoRefresh {
				return a, a.tick()
			}
			return a, nil

		case "up", "k":
			// Scroll logs up
			if a.logPosition > 0 {
				a.logPosition--
			}
			return a, nil

		case "down", "j":
			// Scroll logs down
			if a.logPosition < len(a.logs) {
				a.logPosition++
			}
			return a, nil

		case "enter":
			// Send message
			if strings.TrimSpace(a.inputText) != "" && !a.sending {
				return a, a.sendMessage()
			}
			return a, nil

		case "ctrl+h":
			// Backspace
			if len(a.inputText) > 0 {
				a.inputText = a.inputText[:len(a.inputText)-1]
			}
			return a, nil

		case "ctrl+u":
			// Clear input
			a.inputText = ""
			return a, nil
		}

		// Handle regular character input for message box
		if len(msg.String()) == 1 && !a.sending {
			// Only allow printable characters
			if msg.Runes != nil && len(msg.Runes) > 0 {
				r := msg.Runes[0]
				if r >= 32 && r <= 126 {
					a.inputText += string(r)
				}
			}
		}

	case statusRefreshMsg:
		if msg.err != nil {
			a.err = msg.err
			a.connected = false
		} else {
			a.status = msg.status
			a.err = nil
			a.connected = true
			a.lastUpdate = time.Now()
			// Update thread ID from status
			if msg.status.ControllerSession != "" && a.threadID == "" {
				a.threadID = msg.status.ControllerSession
			}
		}
		return a, nil

	case logRefreshMsg:
		if msg.err != nil {
			a.err = msg.err
			a.connected = false
		} else {
			a.logs = msg.logs
			if len(a.logs) > 0 {
				a.logPosition = len(a.logs)
			}
		}
		return a, nil

	case tickMsg:
		if !a.quitting && a.autoRefresh {
			return a, tea.Batch(a.refreshStatusCmd(), a.refreshLogsCmd())
		}
		return a, nil

	case streamStartedMsg:
		a.streamCtx = msg.ctx
		a.streamCancel = msg.cancel
		a.streamActive = true
		return a, nil

	case threadStartedMsg:
		if msg.resp != nil {
			a.threadID = msg.resp.ThreadID
			a.addSystemMessage(fmt.Sprintf("Thread started: %s", msg.resp.ThreadID))
		}
		return a, nil

	case streamErrorMsg:
		a.err = fmt.Errorf("stream error: %w", msg.err)
		a.streamActive = false
		return a, nil

	case notificationMsg:
		a.handleNotification(msg.notification)
		return a, nil

	case messageSentMsg:
		a.sending = false
		if msg.err != nil {
			a.sendingError = msg.err
			a.addSystemMessage(fmt.Sprintf("Failed to send message: %s", msg.err))
		} else if msg.response != nil {
			a.inputText = "" // Clear input on success
			a.sendingError = nil
		}
		return a, nil
	}

	return a, nil
}

func (a *App) handleNotification(notif StreamNotification) {
	switch notif.Method {
	case "thread/started":
		var params struct {
			ThreadID string `json:"thread_id"`
			Type     string `json:"type"`
			State    string `json:"state"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			a.threadID = params.ThreadID
			a.addTurnLifecycleMessage("Thread started", params.State)
		}

	case "thread/resumed":
		a.addSystemMessage("Thread resumed")

	case "thread/paused":
		a.addSystemMessage("Thread paused")

	case "turn/started":
		var params struct {
			TurnID   string `json:"turn_id"`
			ThreadID string `json:"thread_id,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			a.addTurnLifecycleMessage(fmt.Sprintf("Turn %s started", params.TurnID), "active")
		}

	case "turn/completed":
		var params struct {
			TurnID   string `json:"turn_id"`
			ThreadID string `json:"thread_id,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			a.addTurnLifecycleMessage(fmt.Sprintf("Turn %s completed", params.TurnID), "completed")
		}

	case "turn/interrupted":
		var params struct {
			TurnID   string `json:"turn_id"`
			Message  string `json:"message,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			msg := fmt.Sprintf("Turn %s interrupted", params.TurnID)
			if params.Message != "" {
				msg += fmt.Sprintf(": %s", params.Message)
			}
			a.addSystemMessage(msg)
		}

	case "item/created":
		var params struct {
			ItemID   string                 `json:"item_id"`
			ThreadID string                 `json:"thread_id,omitempty"`
			TurnID   string                 `json:"turn_id,omitempty"`
			Content  map[string]interface{} `json:"content,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			a.handleItemCreated(params)
		}
	}
}

func (a *App) handleItemCreated(params struct {
	ItemID   string                 `json:"item_id"`
	ThreadID string                 `json:"thread_id,omitempty"`
	TurnID   string                 `json:"turn_id,omitempty"`
	Content  map[string]interface{} `json:"content,omitempty"`
}) {
	role, _ := params.Content["role"].(string)
	if role == "" {
		return
	}

	// Extract text from content
	contentParts, ok := params.Content["content"].([]interface{})
	if !ok || len(contentParts) == 0 {
		return
	}

	var textParts []string
	for _, part := range contentParts {
		partMap, ok := part.(map[string]interface{})
		if !ok {
			continue
		}
		partText, _ := partMap["text"].(string)
		if partText != "" {
			textParts = append(textParts, partText)
		}
	}

	content := strings.Join(textParts, "\n")
	if content == "" {
		return
	}

	if role == "user" {
		a.addUserMessage(content)
	} else if role == "assistant" {
		a.addAssistantMessage(content)
	}
}

func (a *App) addUserMessage(content string) {
	a.messages = append(a.messages, ConversationMessage{
		ID:        fmt.Sprintf("msg_%d", time.Now().UnixNano()),
		Type:      "user",
		Timestamp: time.Now(),
		Content:   content,
	})
}

func (a *App) addAssistantMessage(content string) {
	a.messages = append(a.messages, ConversationMessage{
		ID:        fmt.Sprintf("msg_%d", time.Now().UnixNano()),
		Type:      "assistant",
		Timestamp: time.Now(),
		Content:   content,
	})
}

func (a *App) addSystemMessage(content string) {
	a.messages = append(a.messages, ConversationMessage{
		ID:        fmt.Sprintf("sys_%d", time.Now().UnixNano()),
		Type:      "system",
		Timestamp: time.Now(),
		Content:   content,
	})
}

func (a *App) addTurnLifecycleMessage(content, state string) {
	a.messages = append(a.messages, ConversationMessage{
		ID:        fmt.Sprintf("turn_%d", time.Now().UnixNano()),
		Type:      "turn_lifecycle",
		Timestamp: time.Now(),
		Content:   content,
		State:     state,
	})
}

// View renders the UI
func (a *App) View() string {
	if a.quitting {
		return "Goodbye!\n"
	}

	var b strings.Builder

	// Header
	b.WriteString(titleStyle.Render("Holon Serve TUI"))
	b.WriteString("\n\n")

	// Connection status
	if a.connected {
		connStatus := fmt.Sprintf("Connected: %s | Last Update: %s",
			a.client.rpcURL,
			a.lastUpdate.Format("15:04:05"))
		b.WriteString(statusStyle.Render(connStatus))
	} else if a.err != nil {
		b.WriteString(errorStyle.Render(fmt.Sprintf("Connection Error: %s", a.err.Error())))
	} else {
		b.WriteString(statusStyle.Render("Connecting..."))
	}
	b.WriteString("\n\n")

	// Status Panel
	if a.status != nil {
		b.WriteString(a.renderStatusPanel())
	}

	// Conversation Panel
	b.WriteString(a.renderConversationPanel())

	// Log Panel
	b.WriteString(a.renderLogPanel())

	// Help
	b.WriteString("\n\n")
	b.WriteString(a.renderHelp())

	return b.String()
}

func (a *App) renderStatusPanel() string {
	var b strings.Builder

	b.WriteString(titleStyle.Render("Runtime Status"))
	b.WriteString("\n")

	stateLabel := a.status.State
	stateStyle := statusStyle
	if a.status.State == "paused" {
		stateStyle = pausedStyle
	}
	b.WriteString(stateStyle.Render(fmt.Sprintf("State: %s", stateLabel)))
	b.WriteString("\n")

	b.WriteString(statusStyle.Render(fmt.Sprintf("Events Processed: %d", a.status.EventsProcessed)))
	b.WriteString("\n")

	if !a.status.LastEventAt.IsZero() {
		b.WriteString(statusStyle.Render(fmt.Sprintf("Last Event: %s",
			a.status.LastEventAt.Format("2006-01-02 15:04:05"))))
		b.WriteString("\n")
	}

	if a.status.ControllerSession != "" {
		b.WriteString(statusStyle.Render(fmt.Sprintf("Thread: %s", a.status.ControllerSession)))
		b.WriteString("\n")
	}

	b.WriteString("\n")

	return borderStyle.Width(60).Render(b.String())
}

func (a *App) renderConversationPanel() string {
	var b strings.Builder

	b.WriteString(titleStyle.Render("Conversation"))
	b.WriteString("\n")

	if len(a.messages) == 0 {
		b.WriteString(statusStyle.Render("No messages yet"))
		b.WriteString("\n")
	} else {
		// Show last 8 messages
		start := len(a.messages) - 8
		if start < 0 {
			start = 0
		}

		for i := start; i < len(a.messages); i++ {
			msg := a.messages[i]
			timestamp := msg.Timestamp.Format("15:04:05")

			var style lipgloss.Style
			var prefix string

			switch msg.Type {
			case "user":
				style = userMsgStyle
				prefix = "[You]"
			case "assistant":
				style = assistantMsgStyle
				prefix = "[Assistant]"
			case "system":
				style = systemMsgStyle
				prefix = "[System]"
			case "turn_lifecycle":
				style = systemMsgStyle
				if msg.State == "completed" {
					prefix = "[✓]"
				} else if msg.State == "active" {
					prefix = "[…]"
				} else {
					prefix = "[!]"
				}
			}

			// Truncate long messages
			content := msg.Content
			if len(content) > 60 {
				content = content[:57] + "..."
			}

			b.WriteString(style.Render(fmt.Sprintf("%s %s: %s",
				timestamp, prefix, content)))
			b.WriteString("\n")
		}
	}

	b.WriteString("\n")

	return borderStyle.Width(80).Height(12).Render(b.String())
}

func (a *App) renderLogPanel() string {
	var b strings.Builder

	b.WriteString(titleStyle.Render("Event Logs"))
	b.WriteString("\n")

	if len(a.logs) == 0 {
		b.WriteString(statusStyle.Render("No logs available"))
		b.WriteString("\n")
	} else {
		// Show last 5 logs before logPosition
		start := a.logPosition - 5
		if start < 0 {
			start = 0
		}
		end := a.logPosition
		if end > len(a.logs) {
			end = len(a.logs)
		}

		for i := start; i < end; i++ {
			log := a.logs[i]
			timestamp := log.Time.Format("15:04:05")
			levelStyle := statusStyle
			if log.Level == "error" || log.Level == "ERROR" {
				levelStyle = errorStyle
			}
			b.WriteString(levelStyle.Render(fmt.Sprintf("[%s] %s: %s",
				timestamp, log.Level, log.Message)))
			b.WriteString("\n")
		}

		if len(a.logs) > 5 {
			b.WriteString(helpStyle.Render(fmt.Sprintf("Showing %d-%d of %d logs (use ↑/↓ to scroll)",
				start+1, end, len(a.logs))))
			b.WriteString("\n")
		}
	}

	b.WriteString("\n")

	return borderStyle.Width(80).Height(8).Render(b.String())
}

func (a *App) renderHelp() string {
	inputState := ""
	if a.sending {
		inputState = " [Sending...]"
	} else if a.sendingError != nil {
		inputState = " [Send Failed]"
	}

	help := fmt.Sprintf("Commands: [Enter] Send Message%s | [p] Pause | [r] Resume | [R] Refresh | [Space] Toggle Auto-Refresh | [↑/↓] Scroll Logs | [Ctrl+U] Clear Input | [q] Quit",
		inputState)
	return helpStyle.Render(help)
}

// Render input box
func (a *App) renderInputBox() string {
	prefix := "Message: "
	if a.sending {
		prefix = "Sending... "
	}

	displayText := a.inputText
	if len(displayText) > 70 {
		displayText = displayText[:67] + "..."
	}

	return inputStyle.Render(prefix + displayText + "_")
}

// Commands
func (a *App) refreshStatusCmd() tea.Cmd {
	status, err := a.client.GetStatus()
	return func() tea.Msg {
		return statusRefreshMsg{status: status, err: err}
	}
}

func (a *App) refreshLogsCmd() tea.Cmd {
	logsResp, err := a.client.GetLogs(100)
	return func() tea.Msg {
		if err != nil {
			return logRefreshMsg{err: err}
		}
		return logRefreshMsg{logs: logsResp.Logs}
	}
}

func (a *App) tick() tea.Cmd {
	return tea.Tick(time.Second*2, func(t time.Time) tea.Msg {
		return tickMsg(t)
	})
}

func (a *App) sendPauseCmd() tea.Cmd {
	return func() tea.Msg {
		_, err := a.client.Pause()
		return pauseSuccessMsg{err: err}
	}
}

func (a *App) sendResumeCmd() tea.Cmd {
	return func() tea.Msg {
		_, err := a.client.Resume()
		return pauseSuccessMsg{err: err}
	}
}

func (a *App) startStreamCmd() tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithCancel(context.Background())
		go func() {
			if err := a.client.StreamNotifications(ctx, func(notif StreamNotification) {
				// Send notification to main goroutine via tea.Cmd
				// Note: This is called from a goroutine, we need to be careful
			}); err != nil {
				// Handle stream error
			}
		}()
		return streamStartedMsg{ctx: ctx, cancel: cancel}
	}
}

func (a *App) sendMessage() tea.Cmd {
	message := strings.TrimSpace(a.inputText)
	if message == "" {
		return nil
	}

	a.sending = true

	// Start thread if needed
	threadID := a.threadID
	if threadID == "" {
		return tea.Sequentially(
			func() tea.Msg {
				resp, err := a.client.StartThread()
				if err != nil {
					return messageSentMsg{err: err}
				}
				return threadStartedMsg{resp: resp}
			},
			a.doSendMessage(message),
		)
	}

	return a.doSendMessage(message)
}

func (a *App) doSendMessage(message string) tea.Cmd {
	return func() tea.Msg {
		resp, err := a.client.StartTurn(a.threadID, message)
		if err != nil {
			return messageSentMsg{err: err}
		}
		// Add user message to conversation
		a.addUserMessage(message)
		return messageSentMsg{response: resp}
	}
}

type pauseSuccessMsg struct {
	err error
}

type threadStartedMsg struct {
	resp *ThreadStartResponse
}

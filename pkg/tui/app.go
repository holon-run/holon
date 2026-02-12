package tui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/bubbles/textarea"
	"github.com/charmbracelet/bubbles/viewport"
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
	threadID     string
	messages     []ConversationMessage
	sending      bool
	sendingError error
	terminalW    int
	terminalH    int
	focus        focusArea
	hasUnread    bool

	// TUI components
	input        textarea.Model
	conversation viewport.Model
	logViewport  viewport.Model

	// Streaming
	streamCtx     context.Context
	streamCancel  context.CancelFunc
	streamActive  bool
	notifications chan StreamNotification
	streamErrors  chan error
	streamClosed  chan struct{}
	streamRetries int
}

type focusArea int

const (
	focusInput focusArea = iota
	focusConversation
	focusLogs
	focusCount
)

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
	input := textarea.New()
	input.Placeholder = "Type a message..."
	input.Prompt = "> "
	input.ShowLineNumbers = false
	input.CharLimit = 0
	input.SetHeight(3)
	input.SetWidth(80)
	input.Focus()
	input.KeyMap.InsertNewline = key.NewBinding(
		key.WithKeys("ctrl+j"),
		key.WithHelp("ctrl+j", "newline"),
	)

	conversation := viewport.New(80, 14)
	logViewport := viewport.New(80, 8)

	return &App{
		client:        client,
		logs:          make([]LogEntry, 0),
		autoRefresh:   true,
		messages:      make([]ConversationMessage, 0),
		focus:         focusInput,
		input:         input,
		conversation:  conversation,
		logViewport:   logViewport,
		notifications: make(chan StreamNotification, 128),
		streamErrors:  make(chan error, 8),
		streamClosed:  make(chan struct{}, 1),
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
			Bold(true)

	assistantMsgStyle = lipgloss.NewStyle().
				Foreground(lipgloss.Color("green")).
				Bold(true)

	systemMsgStyle = lipgloss.NewStyle().
			Foreground(lipgloss.Color("magenta")).
			Bold(true)
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

type streamReconnectMsg struct{}

type notificationMsg struct {
	notification StreamNotification
}

type messageSentMsg struct {
	response *TurnStartResponse
	err      error
	message  string
	threadID string
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
	case tea.WindowSizeMsg:
		a.terminalW = msg.Width
		a.terminalH = msg.Height
		a.resize()
		return a, nil

	case tea.KeyMsg:
		keyName := msg.String()
		switch keyName {
		case "q", "ctrl+c":
			a.quitting = true
			if a.streamCancel != nil {
				a.streamCancel()
			}
			return a, tea.Quit

		case "tab":
			return a, a.nextFocus()

		case "shift+tab":
			return a, a.prevFocus()

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

		case "enter":
			if a.focus == focusInput && strings.TrimSpace(a.input.Value()) != "" && !a.sending {
				return a, a.sendMessage()
			}

		case "ctrl+u":
			if a.focus == focusInput {
				a.input.SetValue("")
				return a, nil
			}
		}

		if a.focus == focusInput && !a.sending {
			var cmd tea.Cmd
			a.input, cmd = a.input.Update(msg)
			return a, cmd
		}

		if a.focus == focusConversation {
			var cmd tea.Cmd
			a.conversation, cmd = a.conversation.Update(msg)
			if a.conversation.AtBottom() {
				a.hasUnread = false
			}
			return a, cmd
		}

		if a.focus == focusLogs {
			var cmd tea.Cmd
			a.logViewport, cmd = a.logViewport.Update(msg)
			return a, cmd
		}
		return a, nil

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
			a.updateLogViewport()
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
		return a, a.waitForStreamEventCmd()

	case streamErrorMsg:
		a.err = fmt.Errorf("stream error: %w", msg.err)
		a.streamActive = false
		a.addSystemMessage("Notification stream disconnected")
		if a.quitting || errors.Is(msg.err, context.Canceled) {
			return a, nil
		}
		a.streamRetries++
		delay := reconnectDelay(a.streamRetries)
		return a, tea.Tick(delay, func(time.Time) tea.Msg {
			return streamReconnectMsg{}
		})

	case streamReconnectMsg:
		if a.quitting || a.streamActive {
			return a, nil
		}
		return a, a.startStreamCmd()

	case notificationMsg:
		a.streamRetries = 0
		a.handleNotification(msg.notification)
		return a, a.waitForStreamEventCmd()

	case messageSentMsg:
		a.sending = false
		if msg.err != nil {
			a.sendingError = msg.err
			a.addSystemMessage(fmt.Sprintf("Failed to send message: %s", msg.err))
		} else if msg.response != nil {
			a.input.SetValue("")
			a.sendingError = nil
			if msg.threadID != "" && a.threadID == "" {
				a.threadID = msg.threadID
				a.addSystemMessage(fmt.Sprintf("Thread started: %s", msg.threadID))
			}
			if msg.message != "" {
				a.addUserMessage(msg.message)
			}
		}
		return a, nil

	case pauseSuccessMsg:
		if msg.err != nil {
			a.addSystemMessage(fmt.Sprintf("Command failed: %s", msg.err))
		} else {
			a.addSystemMessage("Command sent successfully")
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
			TurnID  string `json:"turn_id"`
			Message string `json:"message,omitempty"`
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
	a.updateConversationViewport(true)
}

func (a *App) addAssistantMessage(content string) {
	a.messages = append(a.messages, ConversationMessage{
		ID:        fmt.Sprintf("msg_%d", time.Now().UnixNano()),
		Type:      "assistant",
		Timestamp: time.Now(),
		Content:   content,
	})
	a.updateConversationViewport(true)
}

func (a *App) addSystemMessage(content string) {
	a.messages = append(a.messages, ConversationMessage{
		ID:        fmt.Sprintf("sys_%d", time.Now().UnixNano()),
		Type:      "system",
		Timestamp: time.Now(),
		Content:   content,
	})
	a.updateConversationViewport(true)
}

func (a *App) addTurnLifecycleMessage(content, state string) {
	a.messages = append(a.messages, ConversationMessage{
		ID:        fmt.Sprintf("turn_%d", time.Now().UnixNano()),
		Type:      "turn_lifecycle",
		Timestamp: time.Now(),
		Content:   content,
		State:     state,
	})
	a.updateConversationViewport(true)
}

// View renders the UI
func (a *App) View() string {
	if a.quitting {
		return "Goodbye!\n"
	}

	var b strings.Builder
	b.WriteString(titleStyle.Render("Holon Serve TUI"))
	b.WriteString("\n")

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
	b.WriteString("\n")
	if a.status != nil {
		b.WriteString(a.renderStatusPanel())
		b.WriteString("\n")
	}
	b.WriteString(a.renderConversationPanel())
	b.WriteString("\n")
	b.WriteString(a.renderLogPanel())
	b.WriteString("\n")
	b.WriteString(a.renderHelp())
	b.WriteString("\n")
	b.WriteString(a.renderInputBox())

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
	return borderStyle.Width(a.panelWidth()).Render(b.String())
}

func (a *App) renderConversationPanel() string {
	title := "Conversation"
	if a.focus == focusConversation {
		title += " [Focus]"
	}
	if a.hasUnread && a.focus != focusConversation {
		title += " [New]"
	}
	panel := titleStyle.Render(title) + "\n" + a.conversation.View()
	return borderStyle.Width(a.panelWidth()).Height(a.conversation.Height + 3).Render(panel)
}

func (a *App) renderLogPanel() string {
	title := "Event Logs"
	if a.focus == focusLogs {
		title += " [Focus]"
	}
	panel := titleStyle.Render(title) + "\n" + a.logViewport.View()
	return borderStyle.Width(a.panelWidth()).Height(a.logViewport.Height + 3).Render(panel)
}

func (a *App) renderHelp() string {
	inputState := ""
	if a.sending {
		inputState = " [Sending...]"
	} else if a.sendingError != nil {
		inputState = " [Send Failed]"
	}

	help := fmt.Sprintf("Commands: [Tab] Switch Focus | [Enter] Send%s | [Ctrl+J] Newline | [Ctrl+U] Clear Input | [↑/↓] Scroll Line | [PgUp/PgDn] Scroll Page | [p] Pause | [r] Resume | [R] Refresh | [Space] Toggle Auto-Refresh | [q] Quit",
		inputState)
	return helpStyle.Render(help)
}

// Render input box
func (a *App) renderInputBox() string {
	title := "Input"
	if a.focus == focusInput {
		title += " [Focus]"
	}
	if a.sending {
		title += " [Sending]"
	}
	if a.sendingError != nil {
		title += " [Last Send Failed]"
	}
	content := titleStyle.Render(title) + "\n" + a.input.View()
	return borderStyle.Width(a.panelWidth()).Height(a.input.Height() + 3).Render(content)
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
		notifs := a.notifications
		errs := a.streamErrors
		closed := a.streamClosed
		go func() {
			if err := a.client.StreamNotifications(ctx, func(notif StreamNotification) {
				select {
				case notifs <- notif:
				case <-ctx.Done():
				}
			}); err != nil {
				select {
				case errs <- err:
				default:
				}
				return
			}
			select {
			case closed <- struct{}{}:
			default:
			}
		}()
		return streamStartedMsg{ctx: ctx, cancel: cancel}
	}
}

func (a *App) waitForStreamEventCmd() tea.Cmd {
	return func() tea.Msg {
		var done <-chan struct{}
		if a.streamCtx != nil {
			done = a.streamCtx.Done()
		}
		select {
		case notif := <-a.notifications:
			return notificationMsg{notification: notif}
		case err := <-a.streamErrors:
			return streamErrorMsg{err: err}
		case <-a.streamClosed:
			return streamErrorMsg{err: fmt.Errorf("stream closed")}
		case <-done:
			return streamErrorMsg{err: context.Canceled}
		}
	}
}

func reconnectDelay(retries int) time.Duration {
	switch {
	case retries <= 1:
		return 1 * time.Second
	case retries == 2:
		return 2 * time.Second
	default:
		return 5 * time.Second
	}
}

func (a *App) sendMessage() tea.Cmd {
	message := strings.TrimSpace(a.input.Value())
	if message == "" {
		return nil
	}

	a.sending = true

	// Start thread if needed
	threadID := a.threadID
	if threadID == "" {
		return func() tea.Msg {
			threadResp, err := a.client.StartThread()
			if err != nil {
				return messageSentMsg{err: err}
			}
			resp, err := a.client.StartTurn(threadResp.ThreadID, message)
			if err != nil {
				return messageSentMsg{err: err}
			}
			return messageSentMsg{response: resp, message: message, threadID: threadResp.ThreadID}
		}
	}

	return a.doSendMessage(message, threadID)
}

func (a *App) doSendMessage(message, threadID string) tea.Cmd {
	return func() tea.Msg {
		resp, err := a.client.StartTurn(threadID, message)
		if err != nil {
			return messageSentMsg{err: err}
		}
		return messageSentMsg{response: resp, message: message}
	}
}

type pauseSuccessMsg struct {
	err error
}

func (a *App) nextFocus() tea.Cmd {
	a.focus = (a.focus + 1) % focusCount
	return a.applyFocus()
}

func (a *App) prevFocus() tea.Cmd {
	a.focus = (a.focus + focusCount - 1) % focusCount
	return a.applyFocus()
}

func (a *App) applyFocus() tea.Cmd {
	if a.focus == focusInput {
		return a.input.Focus()
	}
	a.input.Blur()
	if a.focus == focusConversation && a.conversation.AtBottom() {
		a.hasUnread = false
	}
	return nil
}

func (a *App) panelWidth() int {
	if a.terminalW <= 0 {
		return 90
	}
	if a.terminalW < 40 {
		return a.terminalW
	}
	return a.terminalW - 2
}

func (a *App) resize() {
	width := a.panelWidth() - 4
	if width < 1 {
		width = 1
	}
	conversationHeight := 14
	logHeight := 8
	inputHeight := 3
	if a.terminalH > 0 {
		usable := a.terminalH - 14
		if usable > 24 {
			conversationHeight = usable - 10
			logHeight = 8
		} else if usable > 14 {
			conversationHeight = usable - 8
			logHeight = 6
		}
	}
	if conversationHeight < 8 {
		conversationHeight = 8
	}
	if logHeight < 4 {
		logHeight = 4
	}

	a.input.SetWidth(width)
	a.input.SetHeight(inputHeight)
	a.conversation.Width = width
	a.conversation.Height = conversationHeight
	a.logViewport.Width = width
	a.logViewport.Height = logHeight
	a.updateConversationViewport(false)
	a.updateLogViewport()
}

func (a *App) updateConversationViewport(autoFollow bool) {
	wasAtBottom := a.conversation.AtBottom()
	a.conversation.SetContent(a.conversationContent())
	if autoFollow && (wasAtBottom || a.focus == focusInput) {
		a.conversation.GotoBottom()
		a.hasUnread = false
	} else if autoFollow {
		a.hasUnread = true
	}
}

func (a *App) updateLogViewport() {
	wasAtBottom := a.logViewport.AtBottom()
	a.logViewport.SetContent(a.logContent())
	if wasAtBottom {
		a.logViewport.GotoBottom()
	}
}

func (a *App) conversationContent() string {
	if len(a.messages) == 0 {
		return statusStyle.Render("No messages yet.")
	}

	var lines []string
	for _, msg := range a.messages {
		timestamp := msg.Timestamp.Format("15:04:05")
		roleLabel, roleStyle := a.messageLabel(msg)
		header := fmt.Sprintf("%s %s", timestamp, roleStyle.Render(roleLabel))
		lines = append(lines, header)
		for _, contentLine := range strings.Split(msg.Content, "\n") {
			lines = append(lines, "  "+contentLine)
		}
		lines = append(lines, "")
	}
	return strings.TrimSpace(strings.Join(lines, "\n"))
}

func (a *App) messageLabel(msg ConversationMessage) (string, lipgloss.Style) {
	switch msg.Type {
	case "user":
		return "[You]", userMsgStyle
	case "assistant":
		return "[Assistant]", assistantMsgStyle
	case "turn_lifecycle":
		if msg.State == "completed" {
			return "[Turn Done]", systemMsgStyle
		}
		if msg.State == "active" {
			return "[Turn Running]", systemMsgStyle
		}
		return "[Turn]", systemMsgStyle
	default:
		return "[System]", systemMsgStyle
	}
}

func (a *App) logContent() string {
	if len(a.logs) == 0 {
		return statusStyle.Render("No logs available.")
	}

	lines := make([]string, 0, len(a.logs))
	for _, entry := range a.logs {
		timestamp := entry.Time.Format("15:04:05")
		level := strings.ToUpper(entry.Level)
		levelLabel := "[" + level + "]"
		if len(level) == 0 {
			levelLabel = "[INFO]"
		}
		lines = append(lines, timestamp+" "+padRight(levelLabel, 8)+" "+entry.Message)
	}
	return strings.Join(lines, "\n")
}

func padRight(s string, width int) string {
	if width <= len(s) {
		return s
	}
	return s + strings.Repeat(" ", width-len(s))
}

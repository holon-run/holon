package tui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/bubbles/textarea"
	"github.com/charmbracelet/bubbles/viewport"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// App is the TUI application state.
type App struct {
	client      *RPCClient
	status      *StatusResponse
	logs        []LogEntry
	logPosition int
	err         error
	connected   bool
	lastUpdate  time.Time
	quitting    bool
	autoRefresh bool

	// Conversation state.
	threadID       string
	turns          map[string]*TurnConversation
	turnOrder      []string
	activityEvents []ConversationMessage
	sending        bool
	sendingError   error
	terminalW      int
	terminalH      int
	focus          focusArea
	activeDrawer   drawerKind
	hasUnreadChat  bool
	hasUnreadAct   bool

	// TUI components.
	input        textarea.Model
	conversation viewport.Model
	activity     viewport.Model
	logViewport  viewport.Model

	// Streaming.
	streamCtx     context.Context
	streamCancel  context.CancelFunc
	streamActive  bool
	notifications chan StreamNotification
	streamErrors  chan error
	streamClosed  chan struct{}
	streamRetries int
	tracer        *tuiDebugTracer
}

type focusArea int

const (
	focusInput focusArea = iota
	focusConversation
	focusDrawer
)

type drawerKind int

const (
	drawerNone drawerKind = iota
	drawerActivity
	drawerLogs
)

// ConversationMessage represents a timeline/event message.
type ConversationMessage struct {
	ID        string    `json:"id"`
	Type      string    `json:"type"` // user, assistant, system, turn_lifecycle
	Timestamp time.Time `json:"timestamp"`
	Content   string    `json:"content"`
	State     string    `json:"state,omitempty"`
}

// TurnConversation groups one user turn and the assistant response.
type TurnConversation struct {
	TurnID        string
	ThreadID      string
	StartedAt     time.Time
	UpdatedAt     time.Time
	State         string
	ProgressText  string
	ProgressState string
	ElapsedMS     int64
	UserText      string
	AssistantText string
}

// Styles.
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

// Messages.
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

type pauseSuccessMsg struct {
	err error
}

type interruptResultMsg struct {
	turnID string
	err    error
}

// NewApp creates a new TUI application.
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
		key.WithKeys("enter", "ctrl+j"),
		key.WithHelp("enter/ctrl+j", "newline"),
	)

	conversation := viewport.New(80, 16)
	activity := viewport.New(80, 16)
	logViewport := viewport.New(80, 16)

	return &App{
		client:         client,
		logs:           make([]LogEntry, 0),
		autoRefresh:    true,
		turns:          make(map[string]*TurnConversation),
		turnOrder:      make([]string, 0),
		activityEvents: make([]ConversationMessage, 0),
		focus:          focusInput,
		activeDrawer:   drawerNone,
		input:          input,
		conversation:   conversation,
		activity:       activity,
		logViewport:    logViewport,
		notifications:  make(chan StreamNotification, 128),
		streamErrors:   make(chan error, 8),
		streamClosed:   make(chan struct{}, 1),
		tracer:         newTUIDebugTracerFromEnv(),
	}
}

// Init initializes the application.
func (a *App) Init() tea.Cmd {
	return tea.Batch(
		a.refreshStatusCmd(),
		a.refreshLogsCmd(),
		a.startStreamCmd(),
		a.tick(),
	)
}

// Update handles messages and updates state.
func (a *App) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		a.terminalW = msg.Width
		a.terminalH = msg.Height
		a.resize()
		return a, nil

	case tea.KeyMsg:
		return a.handleKey(msg)

	case statusRefreshMsg:
		if msg.err != nil {
			a.err = msg.err
			a.connected = false
		} else {
			a.status = msg.status
			a.err = nil
			a.connected = true
			a.lastUpdate = time.Now()
			if msg.status.SessionID != "" && a.threadID == "" {
				a.threadID = msg.status.SessionID
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
			return a, tea.Batch(a.refreshStatusCmd(), a.refreshLogsCmd(), a.tick())
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
			return a, nil
		}
		if msg.response != nil {
			a.input.SetValue("")
			a.sendingError = nil
			if msg.threadID != "" && a.threadID == "" {
				a.threadID = msg.threadID
			}
			if msg.message != "" {
				threadID := firstNonEmpty(msg.threadID, a.threadID)
				a.appendTurnMessage(msg.response.TurnID, threadID, "user", msg.message)
				a.setTurnState(msg.response.TurnID, "active")
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

	case interruptResultMsg:
		if msg.err != nil {
			a.addSystemMessage(fmt.Sprintf("Turn interrupt failed: %s", msg.err))
			return a, nil
		}
		if strings.TrimSpace(msg.turnID) != "" {
			a.addSystemMessage(fmt.Sprintf("Interrupt requested for turn %s", strings.TrimSpace(msg.turnID)))
		} else {
			a.addSystemMessage("Interrupt requested")
		}
		return a, nil
	}

	return a, nil
}

func (a *App) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "q", "ctrl+c":
		a.quitting = true
		if err := a.tracer.close(); err != nil {
			fmt.Fprintf(os.Stderr, "holon tui: %v\n", err)
		}
		if a.streamCancel != nil {
			a.streamCancel()
		}
		return a, tea.Quit
	case "ctrl+p":
		return a, a.sendPauseCmd()
	case "ctrl+r":
		return a, a.sendResumeCmd()
	case "ctrl+x":
		turnID := a.activeTurnID()
		if turnID == "" {
			a.addSystemMessage("No active turn to interrupt")
			return a, nil
		}
		return a, a.sendInterruptTurnCmd(turnID)
	case "ctrl+l":
		return a, tea.Batch(a.refreshStatusCmd(), a.refreshLogsCmd())
	case "ctrl+a":
		a.autoRefresh = !a.autoRefresh
		if a.autoRefresh {
			return a, a.tick()
		}
		return a, nil
	case "a":
		if a.activeDrawer != drawerNone || a.focus != focusInput {
			a.openDrawer(drawerActivity)
			return a, nil
		}
	case "l":
		if a.activeDrawer != drawerNone || a.focus != focusInput {
			a.openDrawer(drawerLogs)
			return a, nil
		}
	case "esc":
		if a.activeDrawer != drawerNone {
			a.closeDrawer()
		}
		return a, nil
	case "tab":
		if a.activeDrawer == drawerNone {
			return a, a.nextFocus()
		}
		return a, nil
	case "shift+tab":
		if a.activeDrawer == drawerNone {
			return a, a.prevFocus()
		}
		return a, nil
	case "ctrl+s":
		if a.activeDrawer == drawerNone && a.focus == focusInput && strings.TrimSpace(a.input.Value()) != "" && !a.sending {
			return a, a.sendMessage()
		}
	case "ctrl+u":
		if a.activeDrawer == drawerNone && a.focus == focusInput {
			a.input.SetValue("")
			return a, nil
		}
	}

	if a.activeDrawer != drawerNone {
		if a.activeDrawer == drawerActivity {
			var cmd tea.Cmd
			a.activity, cmd = a.activity.Update(msg)
			if a.activity.AtBottom() {
				a.hasUnreadAct = false
			}
			return a, cmd
		}
		var cmd tea.Cmd
		a.logViewport, cmd = a.logViewport.Update(msg)
		return a, cmd
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
			a.hasUnreadChat = false
		}
		return a, cmd
	}

	return a, nil
}

func (a *App) openDrawer(kind drawerKind) {
	a.activeDrawer = kind
	a.focus = focusDrawer
	a.input.Blur()
	if kind == drawerActivity && a.activity.AtBottom() {
		a.hasUnreadAct = false
	}
}

func (a *App) closeDrawer() {
	a.activeDrawer = drawerNone
	// Chat-first default: closing a drawer should let users type immediately.
	a.focus = focusInput
	_ = a.input.Focus()
	if a.conversation.AtBottom() {
		a.hasUnreadChat = false
	}
}

func (a *App) handleNotification(notif StreamNotification) {
	a.trace("notification_received", traceFieldsFromNotification(notif))
	a.addNotificationEventToActivity(notif)

	switch notif.Method {
	case "thread/started":
		var params struct {
			ThreadID string `json:"thread_id"`
			State    string `json:"state"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			a.threadID = params.ThreadID
			a.trace("route", map[string]interface{}{
				"method":    "thread/started",
				"panel":     "activity",
				"thread_id": strings.TrimSpace(params.ThreadID),
			})
			a.addTurnLifecycleMessage("Thread started", params.State)
		}

	case "thread/resumed":
		a.trace("route", map[string]interface{}{
			"method": "thread/resumed",
			"panel":  "activity",
		})
		a.addSystemMessage("Thread resumed")

	case "thread/paused":
		a.trace("route", map[string]interface{}{
			"method": "thread/paused",
			"panel":  "activity",
		})
		a.addSystemMessage("Thread paused")

	case "turn/started":
		var params struct {
			TurnID    string `json:"turn_id"`
			ThreadID  string `json:"thread_id,omitempty"`
			StartedAt string `json:"started_at,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			if params.TurnID != "" {
				a.ensureTurn(params.TurnID, params.ThreadID)
				a.setTurnState(params.TurnID, "active")
				if ts, ok := parseTimestamp(params.StartedAt); ok {
					a.setTurnStartedAt(params.TurnID, ts)
				}
			}
			a.trace("route", map[string]interface{}{
				"method":    "turn/started",
				"panel":     "both",
				"turn_id":   strings.TrimSpace(params.TurnID),
				"thread_id": strings.TrimSpace(params.ThreadID),
			})
			a.addTurnLifecycleMessage(fmt.Sprintf("Turn %s started", params.TurnID), "active")
		}

	case "turn/completed":
		var params struct {
			TurnID   string `json:"turn_id"`
			ThreadID string `json:"thread_id,omitempty"`
			Message  string `json:"message,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			if params.TurnID != "" {
				a.ensureTurn(params.TurnID, params.ThreadID)
				a.setTurnState(params.TurnID, "completed")
				if strings.TrimSpace(params.Message) != "" {
					a.appendAssistantIfEmpty(params.TurnID, params.Message)
				}
			}
			a.trace("route", map[string]interface{}{
				"method":    "turn/completed",
				"panel":     "both",
				"turn_id":   strings.TrimSpace(params.TurnID),
				"thread_id": strings.TrimSpace(params.ThreadID),
			})
			a.addTurnLifecycleMessage(fmt.Sprintf("Turn %s completed", params.TurnID), "completed")
		}

	case "turn/interrupted":
		var params struct {
			TurnID   string `json:"turn_id"`
			ThreadID string `json:"thread_id,omitempty"`
			Message  string `json:"message,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			if params.TurnID != "" {
				a.ensureTurn(params.TurnID, params.ThreadID)
				a.setTurnState(params.TurnID, "interrupted")
			}
			a.trace("route", map[string]interface{}{
				"method":    "turn/interrupted",
				"panel":     "both",
				"turn_id":   strings.TrimSpace(params.TurnID),
				"thread_id": strings.TrimSpace(params.ThreadID),
			})
			msg := fmt.Sprintf("Turn %s interrupted", params.TurnID)
			if strings.TrimSpace(params.Message) != "" {
				msg += ": " + strings.TrimSpace(params.Message)
			}
			a.addTurnLifecycleMessage(msg, "interrupted")
		}

	case "turn/progress":
		var params struct {
			TurnID    string `json:"turn_id"`
			ThreadID  string `json:"thread_id,omitempty"`
			State     string `json:"state,omitempty"`
			Message   string `json:"message,omitempty"`
			ElapsedMS int64  `json:"elapsed_ms,omitempty"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			if params.TurnID != "" {
				a.ensureTurn(params.TurnID, params.ThreadID)
				a.setTurnProgress(params.TurnID, params.State, params.Message, params.ElapsedMS)
			}
			a.trace("route", map[string]interface{}{
				"method":    "turn/progress",
				"panel":     "conversation",
				"turn_id":   strings.TrimSpace(params.TurnID),
				"thread_id": strings.TrimSpace(params.ThreadID),
				"state":     strings.TrimSpace(params.State),
			})
		}

	case "item/created":
		var params struct {
			ItemID   string          `json:"item_id"`
			ThreadID string          `json:"thread_id,omitempty"`
			TurnID   string          `json:"turn_id,omitempty"`
			Content  json.RawMessage `json:"content"`
		}
		if err := json.Unmarshal(notif.Params, &params); err == nil {
			if len(params.Content) == 0 {
				a.trace("notification_dropped", map[string]interface{}{
					"method": "item/created",
					"reason": "empty_content",
				})
				return
			}

			var announce struct {
				Type             string `json:"type"`
				EventID          string `json:"event_id,omitempty"`
				Source           string `json:"source,omitempty"`
				EventType        string `json:"event_type,omitempty"`
				SourceSessionKey string `json:"source_session_key,omitempty"`
				Decision         string `json:"decision,omitempty"`
				Action           string `json:"action,omitempty"`
				Text             string `json:"text,omitempty"`
			}
			if err := json.Unmarshal(params.Content, &announce); err == nil {
				if strings.EqualFold(strings.TrimSpace(announce.Type), "system_announce") {
					formatted := formatSystemAnnounceMessage(
						announce.EventID,
						announce.Source,
						announce.EventType,
						announce.SourceSessionKey,
						announce.Decision,
						announce.Action,
						announce.Text,
					)
					a.trace("route", map[string]interface{}{
						"method":    "item/created",
						"panel":     "both",
						"thread_id": strings.TrimSpace(params.ThreadID),
						"turn_id":   strings.TrimSpace(params.TurnID),
						"event_id":  strings.TrimSpace(announce.EventID),
						"reason":    "system_announce",
					})
					syntheticTurnID := firstNonEmpty(params.TurnID, "main_announce")
					a.appendTurnMessage(syntheticTurnID, firstNonEmpty(params.ThreadID, "main"), "assistant", "[Event] "+formatted)
					a.setTurnState(syntheticTurnID, "completed")
					a.addSystemMessage(formatted)
					return
				}
			}

			var chatContent struct {
				Role    string `json:"role"`
				Content []struct {
					Type string `json:"type"`
					Text string `json:"text"`
				} `json:"content"`
			}
			if err := json.Unmarshal(params.Content, &chatContent); err != nil {
				a.trace("notification_dropped", map[string]interface{}{
					"method": "item/created",
					"reason": "invalid_chat_content",
				})
				return
			}
			if chatContent.Role == "" {
				a.trace("notification_dropped", map[string]interface{}{
					"method": "item/created",
					"reason": "missing_role",
				})
				return
			}
			var chunks []string
			for _, part := range chatContent.Content {
				text := strings.TrimSpace(part.Text)
				if text != "" {
					chunks = append(chunks, text)
				}
			}
			if len(chunks) == 0 {
				a.trace("notification_dropped", map[string]interface{}{
					"method": "item/created",
					"reason": "empty_text_chunks",
				})
				return
			}
			a.trace("route", map[string]interface{}{
				"method":       "item/created",
				"panel":        "conversation",
				"thread_id":    strings.TrimSpace(params.ThreadID),
				"turn_id":      strings.TrimSpace(params.TurnID),
				"content_role": strings.TrimSpace(chatContent.Role),
			})
			a.appendTurnMessage(params.TurnID, params.ThreadID, chatContent.Role, strings.Join(chunks, "\n"))
		}
	}
}

func (a *App) addNotificationEventToActivity(notif StreamNotification) {
	summary := summarizeNotification(notif)
	if strings.TrimSpace(summary) == "" {
		return
	}
	a.activityEvents = append(a.activityEvents, ConversationMessage{
		ID:        fmt.Sprintf("evt_%d", time.Now().UnixNano()),
		Type:      "event",
		Timestamp: time.Now(),
		Content:   summary,
	})
	a.trace("panel_write", map[string]interface{}{
		"panel":        "activity",
		"event_kind":   "notification_event",
		"method":       strings.TrimSpace(notif.Method),
		"activity_cnt": len(a.activityEvents),
	})
	a.updateActivityViewport(true)
}

func summarizeNotification(notif StreamNotification) string {
	method := strings.TrimSpace(notif.Method)
	if method == "" {
		return ""
	}
	lines := []string{fmt.Sprintf("event=%s", method)}

	var params struct {
		ThreadID  string          `json:"thread_id,omitempty"`
		TurnID    string          `json:"turn_id,omitempty"`
		EventID   string          `json:"event_id,omitempty"`
		Source    string          `json:"source,omitempty"`
		EventType string          `json:"event_type,omitempty"`
		Repo      string          `json:"repo,omitempty"`
		State     string          `json:"state,omitempty"`
		Message   string          `json:"message,omitempty"`
		ItemID    string          `json:"item_id,omitempty"`
		Content   json.RawMessage `json:"content,omitempty"`
	}
	if err := json.Unmarshal(notif.Params, &params); err != nil {
		return strings.Join(lines, "\n")
	}
	if strings.TrimSpace(params.ThreadID) != "" {
		lines = append(lines, "thread="+strings.TrimSpace(params.ThreadID))
	}
	if strings.TrimSpace(params.TurnID) != "" {
		lines = append(lines, "turn="+strings.TrimSpace(params.TurnID))
	}
	if strings.TrimSpace(params.EventID) != "" {
		lines = append(lines, "event_id="+strings.TrimSpace(params.EventID))
	}
	if strings.TrimSpace(params.Source) != "" {
		lines = append(lines, "source="+strings.TrimSpace(params.Source))
	}
	if strings.TrimSpace(params.EventType) != "" {
		lines = append(lines, "type="+strings.TrimSpace(params.EventType))
	}
	if strings.TrimSpace(params.Repo) != "" {
		lines = append(lines, "repo="+strings.TrimSpace(params.Repo))
	}
	if strings.TrimSpace(params.State) != "" {
		lines = append(lines, "state="+strings.TrimSpace(params.State))
	}
	if strings.TrimSpace(params.Message) != "" {
		lines = append(lines, strings.TrimSpace(params.Message))
	}
	if strings.TrimSpace(params.ItemID) != "" {
		lines = append(lines, "item="+strings.TrimSpace(params.ItemID))
	}

	if len(params.Content) > 0 {
		var content struct {
			Type   string `json:"type,omitempty"`
			Role   string `json:"role,omitempty"`
			Text   string `json:"text,omitempty"`
			Event  string `json:"event_id,omitempty"`
			Action string `json:"action,omitempty"`
		}
		if err := json.Unmarshal(params.Content, &content); err == nil {
			if strings.TrimSpace(content.Type) != "" {
				lines = append(lines, "content.type="+strings.TrimSpace(content.Type))
			}
			if strings.TrimSpace(content.Role) != "" {
				lines = append(lines, "content.role="+strings.TrimSpace(content.Role))
			}
			if strings.TrimSpace(content.Event) != "" {
				lines = append(lines, "content.event_id="+strings.TrimSpace(content.Event))
			}
			if strings.TrimSpace(content.Action) != "" {
				lines = append(lines, "content.action="+strings.TrimSpace(content.Action))
			}
			if strings.TrimSpace(content.Text) != "" {
				lines = append(lines, strings.TrimSpace(content.Text))
			}
		}
	}
	return strings.Join(lines, "\n")
}

func formatSystemAnnounceMessage(eventID, source, eventType, sourceSessionKey, decision, action, text string) string {
	parts := make([]string, 0, 4)
	if strings.TrimSpace(eventID) != "" {
		parts = append(parts, fmt.Sprintf("event=%s", strings.TrimSpace(eventID)))
	}
	if strings.TrimSpace(source) != "" {
		parts = append(parts, fmt.Sprintf("source=%s", strings.TrimSpace(source)))
	}
	if strings.TrimSpace(eventType) != "" {
		parts = append(parts, fmt.Sprintf("type=%s", strings.TrimSpace(eventType)))
	}
	if strings.TrimSpace(sourceSessionKey) != "" {
		parts = append(parts, fmt.Sprintf("session=%s", strings.TrimSpace(sourceSessionKey)))
	}

	headline := "Background event update"
	if len(parts) > 0 {
		headline = headline + " (" + strings.Join(parts, ", ") + ")"
	}

	var detail []string
	if strings.TrimSpace(decision) != "" {
		detail = append(detail, fmt.Sprintf("decision=%s", strings.TrimSpace(decision)))
	}
	if strings.TrimSpace(action) != "" {
		detail = append(detail, fmt.Sprintf("action=%s", strings.TrimSpace(action)))
	}
	if strings.TrimSpace(text) != "" {
		detail = append(detail, strings.TrimSpace(text))
	}
	if len(detail) == 0 {
		return headline
	}
	return headline + "\n" + strings.Join(detail, "\n")
}

func (a *App) ensureTurn(turnID, threadID string) *TurnConversation {
	// Bubble Tea runs model Update() serially. Turn state is mutated only from
	// this model thread, so explicit locking is unnecessary under current design.
	turnID = strings.TrimSpace(turnID)
	if turnID == "" {
		turnID = fmt.Sprintf("turn_unknown_%d", time.Now().UnixNano())
	}
	if turn, ok := a.turns[turnID]; ok {
		if strings.TrimSpace(threadID) != "" {
			turn.ThreadID = strings.TrimSpace(threadID)
		}
		turn.UpdatedAt = time.Now()
		return turn
	}
	turn := &TurnConversation{
		TurnID:    turnID,
		ThreadID:  strings.TrimSpace(threadID),
		StartedAt: time.Now(),
		UpdatedAt: time.Now(),
		State:     "active",
	}
	a.turns[turnID] = turn
	a.turnOrder = append(a.turnOrder, turnID)
	return turn
}

func (a *App) setTurnStartedAt(turnID string, startedAt time.Time) {
	turn := a.ensureTurn(turnID, "")
	turn.StartedAt = startedAt
	turn.UpdatedAt = time.Now()
	a.updateConversationViewport(false)
}

func (a *App) setTurnState(turnID, state string) {
	if strings.TrimSpace(turnID) == "" {
		return
	}
	turn := a.ensureTurn(turnID, "")
	turn.State = strings.TrimSpace(state)
	if turn.State == "completed" || turn.State == "interrupted" {
		turn.ProgressText = ""
		turn.ProgressState = ""
		turn.ElapsedMS = 0
	}
	turn.UpdatedAt = time.Now()
	a.trace("panel_write", map[string]interface{}{
		"panel":      "conversation",
		"event_kind": "turn_state",
		"turn_id":    strings.TrimSpace(turnID),
		"state":      strings.TrimSpace(state),
		"turn_cnt":   len(a.turnOrder),
	})
	a.updateConversationViewport(true)
}

func (a *App) setTurnProgress(turnID, state, message string, elapsedMS int64) {
	turn := a.ensureTurn(turnID, "")
	normalizedState := strings.TrimSpace(state)
	if normalizedState == "" {
		normalizedState = "running"
	}
	turn.State = normalizedState
	turn.ProgressState = normalizedState
	turn.ProgressText = strings.TrimSpace(message)
	if elapsedMS > 0 {
		turn.ElapsedMS = elapsedMS
	}
	turn.UpdatedAt = time.Now()
	a.trace("panel_write", map[string]interface{}{
		"panel":      "conversation",
		"event_kind": "turn_progress",
		"turn_id":    strings.TrimSpace(turnID),
		"state":      strings.TrimSpace(normalizedState),
		"elapsed_ms": elapsedMS,
		"turn_cnt":   len(a.turnOrder),
	})
	a.updateConversationViewport(true)
}

func (a *App) appendAssistantIfEmpty(turnID, content string) {
	turn := a.ensureTurn(turnID, "")
	if strings.TrimSpace(turn.AssistantText) != "" {
		return
	}
	turn.AssistantText = strings.TrimSpace(content)
	turn.UpdatedAt = time.Now()
	a.updateConversationViewport(true)
}

func (a *App) appendTurnMessage(turnID, threadID, role, content string) {
	role = strings.TrimSpace(role)
	content = strings.TrimSpace(content)
	// Precondition: role and content are required for message items.
	if role == "" || content == "" {
		return
	}
	turn := a.ensureTurn(turnID, threadID)
	if role == "user" {
		turn.UserText = appendText(turn.UserText, content)
	} else if role == "assistant" {
		turn.AssistantText = appendText(turn.AssistantText, content)
	}
	turn.UpdatedAt = time.Now()
	a.trace("panel_write", map[string]interface{}{
		"panel":      "conversation",
		"event_kind": "chat_message",
		"turn_id":    strings.TrimSpace(turnID),
		"thread_id":  strings.TrimSpace(threadID),
		"role":       role,
		"turn_cnt":   len(a.turnOrder),
	})
	a.updateConversationViewport(true)
}

func appendText(existing, incoming string) string {
	existingTrimmed := strings.TrimSpace(existing)
	incomingTrimmed := strings.TrimSpace(incoming)
	if incomingTrimmed == "" {
		return existingTrimmed
	}
	if existingTrimmed == "" {
		return incomingTrimmed
	}
	if existingTrimmed == incomingTrimmed {
		return existingTrimmed
	}
	return existingTrimmed + "\n" + incomingTrimmed
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		trimmed := strings.TrimSpace(value)
		if trimmed != "" {
			return trimmed
		}
	}
	return ""
}

func parseTimestamp(raw string) (time.Time, bool) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return time.Time{}, false
	}
	t, err := time.Parse(time.RFC3339, raw)
	if err != nil {
		return time.Time{}, false
	}
	return t, true
}

func (a *App) trace(kind string, fields map[string]interface{}) {
	if a == nil || a.tracer == nil {
		return
	}
	a.tracer.trace(kind, fields)
}

func (a *App) addSystemMessage(content string) {
	a.activityEvents = append(a.activityEvents, ConversationMessage{
		ID:        fmt.Sprintf("sys_%d", time.Now().UnixNano()),
		Type:      "system",
		Timestamp: time.Now(),
		Content:   content,
	})
	a.trace("panel_write", map[string]interface{}{
		"panel":         "activity",
		"event_kind":    "system_message",
		"activity_cnt":  len(a.activityEvents),
		"content_lines": len(strings.Split(content, "\n")),
	})
	a.updateActivityViewport(true)
}

func (a *App) addTurnLifecycleMessage(content, state string) {
	a.activityEvents = append(a.activityEvents, ConversationMessage{
		ID:        fmt.Sprintf("turn_%d", time.Now().UnixNano()),
		Type:      "turn_lifecycle",
		Timestamp: time.Now(),
		Content:   content,
		State:     state,
	})
	a.trace("panel_write", map[string]interface{}{
		"panel":        "activity",
		"event_kind":   "turn_lifecycle",
		"state":        strings.TrimSpace(state),
		"activity_cnt": len(a.activityEvents),
	})
	a.updateActivityViewport(true)
}

// View renders the UI.
func (a *App) View() string {
	if a.quitting {
		return "Goodbye!\n"
	}

	var b strings.Builder
	b.WriteString(titleStyle.Render("Holon Serve TUI"))
	b.WriteString("\n")
	b.WriteString(a.renderStatusBar())
	b.WriteString("\n")

	if a.activeDrawer == drawerNone {
		b.WriteString(a.renderConversationPanel())
		b.WriteString("\n")
		b.WriteString(a.renderHelp())
		b.WriteString("\n")
		b.WriteString(a.renderInputBox())
		return b.String()
	}

	b.WriteString(a.renderDrawerPanel())
	b.WriteString("\n")
	b.WriteString(a.renderHelp())
	return b.String()
}

func (a *App) renderStatusBar() string {
	if !a.connected {
		if a.err != nil {
			return errorStyle.Render(fmt.Sprintf("Connection Error: %s", a.err.Error()))
		}
		return statusStyle.Render("Connecting...")
	}

	state := "unknown"
	if a.status != nil {
		state = a.status.State
	}
	stateStyle := statusStyle
	if state == "paused" {
		stateStyle = pausedStyle
	}

	threadLabel := firstNonEmpty(a.threadID, "-")
	activityLabel := "A"
	if a.hasUnreadAct {
		activityLabel = "A*"
	}
	chatLabel := "C"
	if a.hasUnreadChat {
		chatLabel = "C*"
	}

	statusLine := fmt.Sprintf("%s | Runtime: %s | Thread: %s | Last: %s | Panels: [%s] [L] [%s]",
		a.client.rpcURL,
		state,
		threadLabel,
		a.lastUpdate.Format("15:04:05"),
		activityLabel,
		chatLabel,
	)

	return stateStyle.Render(statusLine)
}

func (a *App) renderConversationPanel() string {
	title := "Conversation"
	if a.focus == focusConversation {
		title += " [Focus]"
	}
	if a.hasUnreadChat && a.focus != focusConversation {
		title += " [New]"
	}
	panel := titleStyle.Render(title) + "\n" + a.conversation.View()
	return borderStyle.Width(a.panelWidth()).Height(a.conversation.Height + 3).Render(panel)
}

func (a *App) renderDrawerPanel() string {
	if a.activeDrawer == drawerLogs {
		title := "Logs Drawer [Esc Close]"
		panel := titleStyle.Render(title) + "\n" + a.logViewport.View()
		return borderStyle.Width(a.panelWidth()).Height(a.logViewport.Height + 3).Render(panel)
	}

	title := "Activity Drawer [Esc Close]"
	if a.hasUnreadAct {
		title += " [New]"
	}
	panel := titleStyle.Render(title) + "\n" + a.activity.View()
	return borderStyle.Width(a.panelWidth()).Height(a.activity.Height + 3).Render(panel)
}

func (a *App) renderHelp() string {
	inputState := ""
	if a.sending {
		inputState = " [Sending...]"
	} else if a.sendingError != nil {
		inputState = " [Send Failed]"
	}

	if a.activeDrawer != drawerNone {
		help := "Keys: [Esc] Close Drawer | [A] Activity | [L] Logs | [Ctrl+P] Pause | [Ctrl+R] Resume | [Ctrl+X] Interrupt Turn | [Ctrl+L] Refresh | [Ctrl+A] Auto-Refresh | [q] Quit\nScroll: [↑/↓] Line | [PgUp/PgDn] Page"
		return helpStyle.Render(help)
	}

	help := fmt.Sprintf("Keys: [Tab] Focus | [Ctrl+S] Send%s | [Enter/Ctrl+J] Newline | [Ctrl+U] Clear | [A/L] Drawer (conversation focus) | [Ctrl+P] Pause | [Ctrl+R] Resume | [Ctrl+X] Interrupt Turn | [Ctrl+L] Refresh | [Ctrl+A] Auto-Refresh\nScroll: [↑/↓] Line | [PgUp/PgDn] Page | [q] Quit", inputState)
	return helpStyle.Render(help)
}

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

// Commands.
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
	return tea.Tick(2*time.Second, func(t time.Time) tea.Msg {
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

func (a *App) sendInterruptTurnCmd(turnID string) tea.Cmd {
	turnID = strings.TrimSpace(turnID)
	return func() tea.Msg {
		_, err := a.client.InterruptTurn(turnID, "user_interrupt_from_tui")
		return interruptResultMsg{turnID: turnID, err: err}
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

func (a *App) nextFocus() tea.Cmd {
	if a.focus == focusInput {
		a.focus = focusConversation
		a.input.Blur()
		return nil
	}
	a.focus = focusInput
	return a.input.Focus()
}

func (a *App) activeTurnID() string {
	for idx := len(a.turnOrder) - 1; idx >= 0; idx-- {
		turnID := a.turnOrder[idx]
		turn := a.turns[turnID]
		if turn == nil {
			continue
		}
		switch strings.TrimSpace(turn.State) {
		case "completed", "interrupted":
			continue
		default:
			return turnID
		}
	}
	return ""
}

func (a *App) prevFocus() tea.Cmd {
	return a.nextFocus()
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

	inputHeight := 3
	panelHeight := 16
	if a.terminalH > 0 {
		usable := a.terminalH - 14
		if usable < 8 {
			usable = 8
		}
		panelHeight = usable
	}

	a.input.SetWidth(width)
	a.input.SetHeight(inputHeight)
	a.conversation.Width = width
	a.conversation.Height = panelHeight
	a.activity.Width = width
	a.activity.Height = panelHeight
	a.logViewport.Width = width
	a.logViewport.Height = panelHeight

	a.updateConversationViewport(false)
	a.updateActivityViewport(false)
	a.updateLogViewport()
}

func (a *App) updateConversationViewport(autoFollow bool) {
	wasAtBottom := a.conversation.AtBottom()
	a.conversation.SetContent(a.conversationContent())
	if autoFollow && (wasAtBottom || a.focus == focusInput || a.activeDrawer != drawerNone) {
		a.conversation.GotoBottom()
	}
	if autoFollow && !a.conversation.AtBottom() {
		a.hasUnreadChat = true
	}
	if a.conversation.AtBottom() {
		a.hasUnreadChat = false
	}
}

func (a *App) updateActivityViewport(autoFollow bool) {
	wasAtBottom := a.activity.AtBottom()
	a.activity.SetContent(a.activityContent())
	if autoFollow && wasAtBottom {
		a.activity.GotoBottom()
	}

	if a.activeDrawer == drawerActivity && a.activity.AtBottom() {
		a.hasUnreadAct = false
		return
	}
	if autoFollow {
		a.hasUnreadAct = true
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
	if len(a.turnOrder) == 0 {
		return statusStyle.Render("No messages yet.")
	}

	lines := make([]string, 0, len(a.turnOrder)*8)
	for _, turnID := range a.turnOrder {
		turn := a.turns[turnID]
		// Defensive guard: turnOrder and turns map are maintained together by
		// ensureTurn(); nil indicates corrupted state from future refactors.
		if turn == nil {
			continue
		}
		timestamp := turn.StartedAt
		if timestamp.IsZero() {
			timestamp = turn.UpdatedAt
		}
		if timestamp.IsZero() {
			timestamp = time.Now()
		}

		lines = append(lines, fmt.Sprintf("%s %s", timestamp.Format("15:04:05"), systemMsgStyle.Render(turnStateLabel(turn.State))))
		if strings.TrimSpace(turn.UserText) != "" {
			lines = append(lines, userMsgStyle.Render("You"))
			lines = append(lines, renderIndentedContent(turn.UserText)...)
		}
		if strings.TrimSpace(turn.AssistantText) != "" {
			lines = append(lines, assistantMsgStyle.Render("Agent"))
			lines = append(lines, renderIndentedContent(turn.AssistantText)...)
		} else if !isTerminalTurnState(turn.State) {
			lines = append(lines, assistantMsgStyle.Render("Agent"))
			progressLine := strings.TrimSpace(turn.ProgressText)
			if progressLine == "" {
				progressLine = "..."
			}
			if turn.ElapsedMS > 0 {
				progressLine = fmt.Sprintf("%s (%0.1fs)", progressLine, float64(turn.ElapsedMS)/1000.0)
			}
			lines = append(lines, "  "+progressLine)
		}
		lines = append(lines, "")
	}

	return strings.TrimSpace(strings.Join(lines, "\n"))
}

func turnStateLabel(state string) string {
	switch strings.TrimSpace(state) {
	case "completed":
		return "[Turn Completed]"
	case "interrupted":
		return "[Turn Interrupted]"
	case "queued":
		return "[Turn Queued]"
	case "cancel_requested":
		return "[Turn Cancel Requested]"
	case "waiting":
		return "[Turn Waiting]"
	default:
		return "[Turn Running]"
	}
}

func isTerminalTurnState(state string) bool {
	switch strings.TrimSpace(state) {
	case "completed", "interrupted":
		return true
	default:
		return false
	}
}

func renderIndentedContent(content string) []string {
	parts := strings.Split(content, "\n")
	lines := make([]string, 0, len(parts))
	for _, part := range parts {
		lines = append(lines, "  "+part)
	}
	return lines
}

func (a *App) activityContent() string {
	if len(a.activityEvents) == 0 {
		return statusStyle.Render("No activity yet.")
	}

	lines := make([]string, 0, len(a.activityEvents)*3)
	for _, msg := range a.activityEvents {
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
	case "event":
		return "[Event]", systemMsgStyle
	case "turn_lifecycle":
		if msg.State == "completed" {
			return "[Turn Done]", systemMsgStyle
		}
		if msg.State == "active" {
			return "[Turn Running]", systemMsgStyle
		}
		if msg.State == "interrupted" {
			return "[Turn Interrupted]", systemMsgStyle
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
		if level == "" {
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

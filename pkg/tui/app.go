package tui

import (
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
}

// NewApp creates a new TUI application
func NewApp(client *RPCClient) *App {
	return &App{
		client:      client,
		logs:        make([]LogEntry, 0),
		autoRefresh: true,
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

// Init initializes the application
func (a *App) Init() tea.Cmd {
	return tea.Batch(
		a.refreshStatus,
		a.refreshLogs,
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
			return a, tea.Quit

		case "p":
			// Pause
			return a, a.sendPause()

		case "r":
			// Resume
			return a, a.sendResume()

		case "R":
			// Refresh
			return a, tea.Batch(a.refreshStatus, a.refreshLogs)

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
		}
		if a.autoRefresh {
			return a, a.tick()
		}
		return a, nil

	case logRefreshMsg:
		if msg.err == nil {
			a.logs = msg.logs
			// Reset position to show latest logs
			if len(a.logs) > 0 {
				a.logPosition = len(a.logs)
			}
		}
		if a.autoRefresh {
			return a, a.tick()
		}
		return a, nil

	case tickMsg:
		if !a.quitting && a.autoRefresh {
			return a, tea.Batch(a.refreshStatus, a.refreshLogs)
		}
		return a, nil

	case pauseSuccessMsg:
		if msg.err != nil {
			a.err = msg.err
		} else {
			// Refresh status after pause/resume
			return a, a.refreshStatus
		}
		if a.autoRefresh {
			return a, a.tick()
		}
		return a, nil
	}

	return a, nil
}

type pauseSuccessMsg struct {
	err error
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
		b.WriteString(statusStyle.Render(fmt.Sprintf("Session: %s", a.status.ControllerSession)))
		b.WriteString("\n")
	}

	if !a.status.PausedAt.IsZero() {
		b.WriteString(pausedStyle.Render(fmt.Sprintf("Paused At: %s",
			a.status.PausedAt.Format("2006-01-02 15:04:05"))))
		b.WriteString("\n")
	}

	if !a.status.ResumedAt.IsZero() {
		b.WriteString(statusStyle.Render(fmt.Sprintf("Resumed At: %s",
			a.status.ResumedAt.Format("2006-01-02 15:04:05"))))
		b.WriteString("\n")
	}

	b.WriteString("\n")

	return borderStyle.Width(60).Render(b.String())
}

func (a *App) renderLogPanel() string {
	var b strings.Builder

	b.WriteString(titleStyle.Render("Event Logs (most recent first)"))
	b.WriteString("\n")

	if len(a.logs) == 0 {
		b.WriteString(statusStyle.Render("No logs available"))
		b.WriteString("\n")
	} else {
		// Show last 10 logs before logPosition
		start := a.logPosition - 10
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

		if len(a.logs) > 10 {
			b.WriteString(helpStyle.Render(fmt.Sprintf("Showing %d-%d of %d logs (use ↑/↓ to scroll)",
				start+1, end, len(a.logs))))
			b.WriteString("\n")
		}
	}

	b.WriteString("\n")

	return borderStyle.Width(80).Height(15).Render(b.String())
}

func (a *App) renderHelp() string {
	help := "Commands: [p] Pause | [r] Resume | [R] Refresh | [Space] Toggle Auto-Refresh | [↑/↓] Scroll Logs | [q] Quit"
	return helpStyle.Render(help)
}

// Commands
func (a *App) refreshStatus() tea.Msg {
	status, err := a.client.GetStatus()
	return statusRefreshMsg{status: status, err: err}
}

func (a *App) refreshLogs() tea.Msg {
	logsResp, err := a.client.GetLogs(100)
	if err != nil {
		return logRefreshMsg{err: err}
	}
	return logRefreshMsg{logs: logsResp.Logs}
}

func (a *App) tick() tea.Cmd {
	return tea.Tick(time.Second*2, func(t time.Time) tea.Msg {
		return tickMsg(t)
	})
}

func (a *App) sendPause() tea.Cmd {
	return func() tea.Msg {
		_, err := a.client.Pause()
		return pauseSuccessMsg{err: err}
	}
}

func (a *App) sendResume() tea.Cmd {
	return func() tea.Msg {
		_, err := a.client.Resume()
		return pauseSuccessMsg{err: err}
	}
}

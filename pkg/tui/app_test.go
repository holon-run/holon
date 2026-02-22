package tui

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	tea "github.com/charmbracelet/bubbletea"
)

func TestAppInputEditAndDelete(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("h")})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("i")})
	app = model.(*App)

	if got := app.input.Value(); got != "hi" {
		t.Fatalf("input value = %q, want %q", got, "hi")
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyBackspace})
	app = model.(*App)
	if got := app.input.Value(); got != "h" {
		t.Fatalf("after backspace input value = %q, want %q", got, "h")
	}
}

func TestAppInputSpaceIsInserted(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'h'}})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeySpace, Runes: []rune{' '}})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'i'}})
	app = model.(*App)

	if got := app.input.Value(); got != "h i" {
		t.Fatalf("input value = %q, want %q", got, "h i")
	}
}

func TestAppInputRegularKeysDontTriggerRuntimeCommands(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)
	app.autoRefresh = true

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'p'}})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'r'}})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'R'}})
	app = model.(*App)

	if got := app.input.Value(); got != "prR" {
		t.Fatalf("input value = %q, want %q", got, "prR")
	}
	if !app.autoRefresh {
		t.Fatal("expected autoRefresh to stay true after typing")
	}
}

func TestAppCtrlATogglesAutoRefreshWhileInputFocused(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	if !app.autoRefresh {
		t.Fatal("expected autoRefresh=true by default")
	}

	model, _ := app.Update(tea.KeyMsg{Type: tea.KeyCtrlA})
	app = model.(*App)
	if app.autoRefresh {
		t.Fatal("expected autoRefresh=false after Ctrl+A")
	}
}

func TestDrawerHotkeysWhileInputFocused(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	if app.activeDrawer != drawerNone {
		t.Fatalf("expected no active drawer before typing, got %v", app.activeDrawer)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'a'}})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'l'}})
	app = model.(*App)

	if got := app.input.Value(); got != "al" {
		t.Fatalf("expected input to contain typed hotkeys when focused, got %q", got)
	}
	if app.activeDrawer != drawerNone {
		t.Fatalf("expected no drawer to open when typing hotkeys into focused input, got %v", app.activeDrawer)
	}
}

func TestConversationDoesNotAutoScrollWhenUserScrolledUp(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	for i := 0; i < 24; i++ {
		turnID := "turn_" + string(rune(i+'a'))
		app.appendTurnMessage(turnID, "main", "user", strings.Repeat("line", 8))
		app.appendTurnMessage(turnID, "main", "assistant", strings.Repeat("reply", 8))
		app.setTurnState(turnID, "completed")
	}

	app.focus = focusConversation
	app.conversation.LineUp(5)
	if app.conversation.AtBottom() {
		t.Fatal("expected conversation to be scrolled up before new message")
	}

	app.appendTurnMessage("turn_new", "main", "assistant", "new message while reading history")

	if app.conversation.AtBottom() {
		t.Fatal("expected conversation to stay scrolled up after new message")
	}
	if !app.hasUnreadChat {
		t.Fatal("expected unread chat indicator when new content arrives off-screen")
	}
}

func TestAppInputCtrlJInsertsNewline(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("a")})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyCtrlJ})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("b")})
	app = model.(*App)

	if got := app.input.Value(); !strings.Contains(got, "\n") {
		t.Fatalf("input value = %q, want newline inserted", got)
	}
}

func TestAppCtrlSStartsSend(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	app.input.SetValue("hello")
	model, cmd := app.Update(tea.KeyMsg{Type: tea.KeyCtrlS})
	app = model.(*App)
	if cmd == nil {
		t.Fatalf("expected send command to be returned")
	}
	if !app.sending {
		t.Fatalf("expected sending=true after ctrl+s")
	}
}

func TestAppEnterInsertsNewline(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	app.input.SetValue("hello")
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyEnter})
	app = model.(*App)
	if got := app.input.Value(); !strings.Contains(got, "\n") {
		t.Fatalf("input value = %q, want newline inserted", got)
	}
	if app.sending {
		t.Fatalf("expected sending=false after enter")
	}
}

func TestAppTabSwitchesFocusBetweenInputAndConversation(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	if app.focus != focusInput {
		t.Fatalf("initial focus = %v, want %v", app.focus, focusInput)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyTab})
	app = model.(*App)
	if app.focus != focusConversation {
		t.Fatalf("focus after tab = %v, want %v", app.focus, focusConversation)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyTab})
	app = model.(*App)
	if app.focus != focusInput {
		t.Fatalf("focus after second tab = %v, want %v", app.focus, focusInput)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyShiftTab})
	app = model.(*App)
	if app.focus != focusConversation {
		t.Fatalf("focus after shift+tab = %v, want %v", app.focus, focusConversation)
	}
}

func TestDrawerToggleAndClose(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	if app.activeDrawer != drawerNone {
		t.Fatalf("expected no drawer initially")
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyTab})
	app = model.(*App)

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'a'}})
	app = model.(*App)
	if app.activeDrawer != drawerActivity {
		t.Fatalf("expected activity drawer, got %v", app.activeDrawer)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'l'}})
	app = model.(*App)
	if app.activeDrawer != drawerLogs {
		t.Fatalf("expected logs drawer, got %v", app.activeDrawer)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyEsc})
	app = model.(*App)
	if app.activeDrawer != drawerNone {
		t.Fatalf("expected drawer to close on esc")
	}
	if app.focus != focusInput {
		t.Fatalf("expected focus to return to input after closing drawer, got %v", app.focus)
	}
}

func TestDrawerBlocksInputEditing(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	app.input.SetValue("seed")
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyTab})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'a'}})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'x'}})
	app = model.(*App)

	if got := app.input.Value(); got != "seed" {
		t.Fatalf("expected input to remain unchanged while drawer open, got %q", got)
	}
}

func TestDefaultViewHidesActivityAndLogsPanels(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	view := app.View()
	if strings.Contains(view, "Activity Drawer [Esc Close]") {
		t.Fatalf("default view should not show activity drawer")
	}
	if strings.Contains(view, "Logs Drawer [Esc Close]") {
		t.Fatalf("default view should not show logs drawer")
	}
	if !strings.Contains(view, "Conversation") {
		t.Fatalf("default view should show conversation panel")
	}
}

func TestSystemNotificationGoesToActivity(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	app.handleNotification(StreamNotification{Method: "thread/resumed"})

	if len(app.activityEvents) != 2 {
		t.Fatalf("activityEvents len = %d, want 2", len(app.activityEvents))
	}
	if got := app.activityEvents[0].Type; got != "event" {
		t.Fatalf("first activity type = %q, want %q", got, "event")
	}
	if got := app.activityEvents[1].Content; got != "Thread resumed" {
		t.Fatalf("second activity content = %q, want %q", got, "Thread resumed")
	}
	if len(app.turnOrder) != 0 {
		t.Fatalf("turnOrder len = %d, want 0", len(app.turnOrder))
	}
}

func TestEventReceivedNotificationGoesToActivity(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	params := map[string]interface{}{
		"event_id":   "evt_ingress_123",
		"source":     "github",
		"event_type": "github.issue.opened",
		"repo":       "holon-run/holon",
	}
	raw, err := json.Marshal(params)
	if err != nil {
		t.Fatalf("marshal params: %v", err)
	}

	app.handleNotification(StreamNotification{Method: "event/received", Params: raw})

	if len(app.activityEvents) != 1 {
		t.Fatalf("activityEvents len = %d, want 1", len(app.activityEvents))
	}
	got := app.activityEvents[0].Content
	if !strings.Contains(got, "event=event/received") {
		t.Fatalf("missing method summary: %q", got)
	}
	if !strings.Contains(got, "event_id=evt_ingress_123") {
		t.Fatalf("missing event_id summary: %q", got)
	}
	if !strings.Contains(got, "type=github.issue.opened") {
		t.Fatalf("missing event_type summary: %q", got)
	}
}

func TestAssistantItemCreatedGoesToTurnConversation(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	params := map[string]interface{}{
		"item_id":   "item_1",
		"thread_id": "main",
		"turn_id":   "turn_1",
		"content": map[string]interface{}{
			"role": "assistant",
			"content": []interface{}{
				map[string]interface{}{"text": "hello from assistant"},
			},
		},
	}
	raw, err := json.Marshal(params)
	if err != nil {
		t.Fatalf("marshal params: %v", err)
	}

	app.handleNotification(StreamNotification{Method: "item/created", Params: raw})

	turn := app.turns["turn_1"]
	if turn == nil {
		t.Fatalf("expected turn_1 to be created")
	}
	if got := turn.AssistantText; got != "hello from assistant" {
		t.Fatalf("assistant message = %q, want %q", got, "hello from assistant")
	}
	if len(app.activityEvents) != 1 {
		t.Fatalf("activityEvents len = %d, want 1", len(app.activityEvents))
	}
	if got := app.activityEvents[0].Type; got != "event" {
		t.Fatalf("activity event type = %q, want %q", got, "event")
	}
}

func TestSystemAnnounceItemCreatedGoesToActivity(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	params := map[string]interface{}{
		"item_id":   "announce_1",
		"thread_id": "main",
		"content": map[string]interface{}{
			"type":               "system_announce",
			"event_id":           "evt_123",
			"source":             "github",
			"event_type":         "github.issue.comment.created",
			"source_session_key": "event:holon-run/holon",
			"decision":           "pr-fix",
			"action":             "updated_branch",
			"text":               "Addressed review feedback",
		},
	}
	raw, err := json.Marshal(params)
	if err != nil {
		t.Fatalf("marshal params: %v", err)
	}

	app.handleNotification(StreamNotification{Method: "item/created", Params: raw})

	if len(app.activityEvents) != 2 {
		t.Fatalf("activityEvents len = %d, want 2", len(app.activityEvents))
	}
	if got := app.activityEvents[1].Content; !strings.Contains(got, "decision=pr-fix") {
		t.Fatalf("activity content missing decision: %q", got)
	}
	if got := app.activityEvents[1].Content; !strings.Contains(got, "action=updated_branch") {
		t.Fatalf("activity content missing action: %q", got)
	}
	if len(app.turnOrder) != 1 {
		t.Fatalf("turnOrder len = %d, want 1", len(app.turnOrder))
	}
	turn := app.turns["main_announce"]
	if turn == nil {
		t.Fatalf("expected synthetic turn main_announce")
	}
	if got := turn.AssistantText; !strings.Contains(got, "[Event] Background event update") {
		t.Fatalf("conversation event text missing: %q", got)
	}
}

func TestTurnAggregationCombinesAssistantMessages(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	app.appendTurnMessage("turn_1", "main", "user", "question")
	app.appendTurnMessage("turn_1", "main", "assistant", "part 1")
	app.appendTurnMessage("turn_1", "main", "assistant", "part 2")

	turn := app.turns["turn_1"]
	if turn == nil {
		t.Fatalf("turn not found")
	}
	if got := turn.AssistantText; got != "part 1\npart 2" {
		t.Fatalf("assistant aggregation = %q", got)
	}
}

func TestStreamErrorSchedulesReconnect(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, cmd := app.Update(streamErrorMsg{err: context.DeadlineExceeded})
	app = model.(*App)

	if cmd == nil {
		t.Fatalf("expected reconnect command after stream error")
	}
	if app.streamActive {
		t.Fatalf("expected streamActive=false after stream error")
	}
}

func TestStreamErrorCanceledDoesNotReconnect(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, cmd := app.Update(streamErrorMsg{err: context.Canceled})
	app = model.(*App)

	if cmd != nil {
		t.Fatalf("expected no reconnect command for canceled stream")
	}
}

func TestReconnectDelay(t *testing.T) {
	if got := reconnectDelay(1); got != 1*time.Second {
		t.Fatalf("reconnectDelay(1)=%v, want 1s", got)
	}
	if got := reconnectDelay(2); got != 2*time.Second {
		t.Fatalf("reconnectDelay(2)=%v, want 2s", got)
	}
	if got := reconnectDelay(3); got != 5*time.Second {
		t.Fatalf("reconnectDelay(3)=%v, want 5s", got)
	}
}

func TestTurnProgressNotificationUpdatesConversation(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	params := map[string]interface{}{
		"turn_id":    "turn_progress_1",
		"thread_id":  "main",
		"state":      "running",
		"message":    "controller event status: running",
		"elapsed_ms": 2500,
	}
	raw, err := json.Marshal(params)
	if err != nil {
		t.Fatalf("marshal params: %v", err)
	}

	app.handleNotification(StreamNotification{Method: "turn/progress", Params: raw})

	turn := app.turns["turn_progress_1"]
	if turn == nil {
		t.Fatalf("expected turn_progress_1 to exist")
	}
	if turn.State != "running" {
		t.Fatalf("turn state = %q, want running", turn.State)
	}
	if turn.ProgressText != "controller event status: running" {
		t.Fatalf("turn progress text = %q", turn.ProgressText)
	}
	if turn.ElapsedMS != 2500 {
		t.Fatalf("turn elapsed_ms = %d, want 2500", turn.ElapsedMS)
	}
}

func TestCtrlXWithoutActiveTurnAddsSystemMessage(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	if len(app.activityEvents) != 0 {
		t.Fatalf("expected empty activity events before test")
	}

	model, cmd := app.Update(tea.KeyMsg{Type: tea.KeyCtrlX})
	app = model.(*App)
	if cmd != nil {
		t.Fatalf("expected no command when no active turn exists")
	}
	if len(app.activityEvents) == 0 {
		t.Fatalf("expected activity message for missing active turn")
	}
	last := app.activityEvents[len(app.activityEvents)-1]
	if !strings.Contains(last.Content, "No active turn to interrupt") {
		t.Fatalf("unexpected system message: %q", last.Content)
	}
}

func TestCtrlXWithActiveTurnReturnsInterruptCommand(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	app.appendTurnMessage("turn_active_1", "main", "user", "hello")
	app.setTurnState("turn_active_1", "running")

	model, cmd := app.Update(tea.KeyMsg{Type: tea.KeyCtrlX})
	app = model.(*App)
	if cmd == nil {
		t.Fatalf("expected interrupt command when an active turn exists")
	}
}

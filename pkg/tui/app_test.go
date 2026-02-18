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

func TestAppEnterStartsSend(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	app.input.SetValue("hello")
	model, cmd := app.Update(tea.KeyMsg{Type: tea.KeyEnter})
	app = model.(*App)
	if cmd == nil {
		t.Fatalf("expected send command to be returned")
	}
	if !app.sending {
		t.Fatalf("expected sending=true after enter")
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

	if len(app.activityEvents) != 1 {
		t.Fatalf("activityEvents len = %d, want 1", len(app.activityEvents))
	}
	if len(app.turnOrder) != 0 {
		t.Fatalf("turnOrder len = %d, want 0", len(app.turnOrder))
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
	if len(app.activityEvents) != 0 {
		t.Fatalf("activityEvents len = %d, want 0", len(app.activityEvents))
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

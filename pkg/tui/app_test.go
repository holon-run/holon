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
		app.addAssistantMessage(strings.Repeat("line", 8))
	}
	app.focus = focusConversation
	app.conversation.LineUp(5)
	if app.conversation.AtBottom() {
		t.Fatal("expected conversation to be scrolled up before new message")
	}

	app.addAssistantMessage("new message while reading history")

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

func TestAppTabSwitchesFocus(t *testing.T) {
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
	if app.focus != focusActivity {
		t.Fatalf("focus after second tab = %v, want %v", app.focus, focusActivity)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyTab})
	app = model.(*App)
	if app.focus != focusLogs {
		t.Fatalf("focus after third tab = %v, want %v", app.focus, focusLogs)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyTab})
	app = model.(*App)
	if app.focus != focusInput {
		t.Fatalf("focus after fourth tab = %v, want %v", app.focus, focusInput)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyShiftTab})
	app = model.(*App)
	if app.focus != focusLogs {
		t.Fatalf("focus after shift+tab = %v, want %v", app.focus, focusLogs)
	}
}

func TestSystemNotificationGoesToActivity(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	app.handleNotification(StreamNotification{Method: "thread/resumed"})

	if len(app.activityEvents) != 1 {
		t.Fatalf("activityEvents len = %d, want 1", len(app.activityEvents))
	}
	if len(app.chatMessages) != 0 {
		t.Fatalf("chatMessages len = %d, want 0", len(app.chatMessages))
	}
}

func TestAssistantItemCreatedGoesToConversation(t *testing.T) {
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	params := map[string]interface{}{
		"item_id": "item_1",
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

	if len(app.chatMessages) != 1 {
		t.Fatalf("chatMessages len = %d, want 1", len(app.chatMessages))
	}
	if got := app.chatMessages[0].Content; got != "hello from assistant" {
		t.Fatalf("chat message = %q, want %q", got, "hello from assistant")
	}
	if len(app.activityEvents) != 0 {
		t.Fatalf("activityEvents len = %d, want 0", len(app.activityEvents))
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

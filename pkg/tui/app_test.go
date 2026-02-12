package tui

import (
	"context"
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
	if app.focus != focusLogs {
		t.Fatalf("focus after second tab = %v, want %v", app.focus, focusLogs)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyTab})
	app = model.(*App)
	if app.focus != focusInput {
		t.Fatalf("focus after third tab = %v, want %v", app.focus, focusInput)
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyShiftTab})
	app = model.(*App)
	if app.focus != focusLogs {
		t.Fatalf("focus after shift+tab = %v, want %v", app.focus, focusLogs)
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

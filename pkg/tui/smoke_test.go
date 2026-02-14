package tui

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"testing"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/holon-run/holon/pkg/serve"
)

type smokeEventHandler struct {
	acks chan serve.TurnAckRecord
}

type roundTripperFunc func(*http.Request) (*http.Response, error)

func (f roundTripperFunc) RoundTrip(req *http.Request) (*http.Response, error) {
	return f(req)
}

func newSmokeEventHandler() *smokeEventHandler {
	return &smokeEventHandler{acks: make(chan serve.TurnAckRecord, 16)}
}

func (h *smokeEventHandler) HandleEvent(context.Context, serve.EventEnvelope) error {
	return nil
}

func (h *smokeEventHandler) TurnAcks() <-chan serve.TurnAckRecord {
	return h.acks
}

func TestTUISmoke_RPCTurnInteractionFlow(t *testing.T) {
	stateDir := t.TempDir()
	handler := newSmokeEventHandler()
	turnDispatcher := func(_ context.Context, req serve.TurnStartRequest, turnID string) error {
		message := "ok"
		if len(req.Input) > 0 && len(req.Input[0].Content) > 0 {
			message = req.Input[0].Content[0].Text
		}
		handler.acks <- serve.TurnAckRecord{
			TurnID:   turnID,
			ThreadID: req.ThreadID,
			Status:   "completed",
			Message:  "ack: " + message,
		}
		return nil
	}

	port, err := reserveLocalPort()
	if err != nil {
		t.Fatalf("reserve local port: %v", err)
	}

	server, err := serve.NewWebhookServer(serve.WebhookConfig{
		Port:           port,
		StateDir:       stateDir,
		Handler:        handler,
		TurnDispatcher: turnDispatcher,
	})
	if err != nil {
		t.Fatalf("NewWebhookServer failed: %v", err)
	}
	defer func() {
		if err := server.Close(); err != nil {
			t.Errorf("close server: %v", err)
		}
	}()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	serveErr := make(chan error, 1)
	go func() {
		serveErr <- server.Start(ctx)
	}()

	baseURL := fmt.Sprintf("http://127.0.0.1:%d", port)
	if err := waitForHealthy(baseURL+"/health", 3*time.Second); err != nil {
		t.Fatalf("wait for server health: %v", err)
	}

	client := NewRPCClient(baseURL + "/rpc")
	if err := client.TestConnection(); err != nil {
		t.Fatalf("rpc test connection failed: %v", err)
	}

	streamConnected := make(chan struct{}, 1)
	baseTransport := http.DefaultTransport
	client.streamCli.Transport = roundTripperFunc(func(req *http.Request) (*http.Response, error) {
		resp, err := baseTransport.RoundTrip(req)
		if err == nil && req.URL.Path == "/rpc/stream" {
			select {
			case streamConnected <- struct{}{}:
			default:
			}
		}
		return resp, err
	})

	streamCtx, streamCancel := context.WithCancel(context.Background())
	defer streamCancel()
	streamEvents := make(chan StreamNotification, 32)
	streamErr := make(chan error, 1)
	go func() {
		err := client.StreamNotifications(streamCtx, func(n StreamNotification) {
			streamEvents <- n
		})
		if err != nil && !errors.Is(err, context.Canceled) {
			streamErr <- err
		}
		close(streamErr)
	}()

	select {
	case <-streamConnected:
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for rpc stream connection")
	}

	app := NewApp(client)
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)
	app.input.SetValue("hello smoke")

	sendCmd := app.sendMessage()
	if sendCmd == nil {
		t.Fatal("sendMessage returned nil command")
	}

	sentMsg, ok := sendCmd().(messageSentMsg)
	if !ok {
		t.Fatalf("sendMessage cmd returned unexpected type")
	}
	if sentMsg.err != nil {
		t.Fatalf("sendMessage failed: %v", sentMsg.err)
	}

	model, _ = app.Update(sentMsg)
	app = model.(*App)
	if app.threadID == "" {
		t.Fatal("threadID should be set after first send")
	}

	seen := map[string]bool{}
	deadline := time.NewTimer(5 * time.Second)
	defer deadline.Stop()

	for !seen["thread/started"] || !seen["turn/started"] || !seen["turn/completed"] || !seen["item/created"] {
		select {
		case err, ok := <-streamErr:
			if !ok {
				streamErr = nil
				continue
			}
			if err != nil {
				t.Fatalf("stream failed: %v", err)
			}
		case notif := <-streamEvents:
			seen[notif.Method] = true
			model, _ = app.Update(notificationMsg{notification: notif})
			app = model.(*App)
		case <-deadline.C:
			t.Fatalf("timed out waiting for turn lifecycle notifications, seen=%v", seen)
		}
	}

	foundAssistant := false
	for _, msg := range app.chatMessages {
		if msg.Type == "assistant" {
			foundAssistant = true
			break
		}
	}
	if !foundAssistant {
		t.Fatalf("assistant message not found in conversation: %+v", app.chatMessages)
	}

	streamCancel()
	cancel()
	if err := <-serveErr; err != nil && !errors.Is(err, context.Canceled) {
		t.Fatalf("webhook server exited with error: %v", err)
	}
}

func TestTUISmoke_InputDeletion(t *testing.T) {
	// This test only verifies local input editing behavior and does not make RPC calls.
	app := NewApp(NewRPCClient("http://127.0.0.1:8080/rpc"))
	model, _ := app.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	app = model.(*App)

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("h")})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("i")})
	app = model.(*App)
	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyBackspace})
	app = model.(*App)

	if got := app.input.Value(); got != "h" {
		t.Fatalf("after backspace input value = %q, want %q", got, "h")
	}

	model, _ = app.Update(tea.KeyMsg{Type: tea.KeyCtrlU})
	app = model.(*App)
	if got := app.input.Value(); got != "" {
		t.Fatalf("after ctrl+u input value = %q, want empty", got)
	}
}

func reserveLocalPort() (int, error) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return 0, err
	}
	defer ln.Close()
	addr, ok := ln.Addr().(*net.TCPAddr)
	if !ok {
		return 0, fmt.Errorf("unexpected addr type %T", ln.Addr())
	}
	return addr.Port, nil
}

func waitForHealthy(url string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	client := &http.Client{Timeout: 1 * time.Second}
	var lastErr error
	for {
		reqCtx, cancel := context.WithTimeout(context.Background(), 1*time.Second)
		req, err := http.NewRequestWithContext(reqCtx, http.MethodGet, url, nil)
		if err != nil {
			cancel()
			lastErr = err
			if time.Now().After(deadline) {
				return lastErr
			}
			time.Sleep(50 * time.Millisecond)
			continue
		}
		resp, err := client.Do(req)
		cancel()
		if err == nil {
			_ = resp.Body.Close()
			if resp.StatusCode == http.StatusOK {
				return nil
			}
			lastErr = fmt.Errorf("health endpoint returned status %d", resp.StatusCode)
		} else {
			lastErr = err
		}
		if time.Now().After(deadline) {
			if lastErr != nil {
				return lastErr
			}
			return fmt.Errorf("health endpoint did not return 200 within %s", timeout)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

package serve

import (
	"context"
	"fmt"
	"net"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

type WebSocketMessageHandler func(ctx context.Context, raw []byte) error

type WebSocketSource struct {
	url           string
	mu            sync.Mutex
	started       bool
	cancel        context.CancelFunc
	wg            sync.WaitGroup
	lastError     string
	lastMessageAt time.Time
	connected     bool
}

type WebSocketSourceConfig struct {
	URL string
}

func NewWebSocketSource(cfg WebSocketSourceConfig) *WebSocketSource {
	return &WebSocketSource{
		url: cfg.URL,
	}
}

func (s *WebSocketSource) Start(ctx context.Context, handler WebSocketMessageHandler) error {
	if handler == nil {
		return fmt.Errorf("websocket handler is required")
	}
	if s.url == "" {
		return fmt.Errorf("websocket url is required")
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.started {
		return fmt.Errorf("websocket source already started")
	}

	runCtx, cancel := context.WithCancel(ctx)
	s.cancel = cancel
	s.started = true
	s.wg.Add(1)
	go s.run(runCtx, handler)
	return nil
}

func (s *WebSocketSource) Stop() error {
	s.mu.Lock()
	if !s.started {
		s.mu.Unlock()
		return nil
	}
	s.started = false
	if s.cancel != nil {
		s.cancel()
	}
	s.mu.Unlock()

	s.wg.Wait()
	return nil
}

func (s *WebSocketSource) Status() map[string]interface{} {
	s.mu.Lock()
	defer s.mu.Unlock()
	status := map[string]interface{}{
		"running":   s.started,
		"url":       s.url,
		"connected": s.connected,
	}
	if s.lastError != "" {
		status["last_error"] = s.lastError
	}
	if !s.lastMessageAt.IsZero() {
		status["last_message_at"] = s.lastMessageAt.UTC().Format(time.RFC3339Nano)
	}
	return status
}

func (s *WebSocketSource) run(ctx context.Context, handler WebSocketMessageHandler) {
	defer s.wg.Done()

	backoff := 500 * time.Millisecond
	maxBackoff := 5 * time.Second

	for ctx.Err() == nil {
		dialer := websocket.Dialer{
			HandshakeTimeout: 10 * time.Second,
		}
		conn, _, err := dialer.DialContext(ctx, s.url, nil)
		if err != nil {
			s.setConnectionState(false, err)
			select {
			case <-ctx.Done():
				return
			case <-time.After(backoff):
			}
			if backoff < maxBackoff {
				backoff *= 2
				if backoff > maxBackoff {
					backoff = maxBackoff
				}
			}
			continue
		}

		backoff = 500 * time.Millisecond
		s.setConnectionState(true, nil)
		readErr := s.readLoop(ctx, conn, handler)
		_ = conn.Close()
		if readErr != nil {
			s.setConnectionState(false, readErr)
		}
	}
}

func (s *WebSocketSource) readLoop(ctx context.Context, conn *websocket.Conn, handler WebSocketMessageHandler) error {
	for ctx.Err() == nil {
		if err := conn.SetReadDeadline(time.Now().Add(1 * time.Second)); err != nil {
			return err
		}
		_, message, err := conn.ReadMessage()
		if err != nil {
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				continue
			}
			return err
		}
		if len(message) == 0 {
			continue
		}

		s.mu.Lock()
		s.lastMessageAt = time.Now().UTC()
		s.mu.Unlock()

		if err := handler(ctx, message); err != nil {
			s.mu.Lock()
			s.lastError = err.Error()
			s.mu.Unlock()
		}
	}
	return ctx.Err()
}

func (s *WebSocketSource) setConnectionState(connected bool, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.connected = connected
	if err != nil {
		s.lastError = err.Error()
	}
}

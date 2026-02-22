package serve

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// TestNotificationCreation tests creating various notification types
func TestNotificationCreation(t *testing.T) {
	t.Run("ItemNotification", func(t *testing.T) {
		content := map[string]interface{}{
			"text":   "test content",
			"status": "pending",
		}
		notifs := []ItemNotification{
			NewItemNotification("item_1", ItemNotificationCreated, "pending", content),
			NewItemNotification("item_2", ItemNotificationUpdated, "completed", content),
			NewItemNotification("item_3", ItemNotificationDeleted, "deleted", nil),
		}

		for _, n := range notifs {
			if n.ItemID == "" {
				t.Error("ItemID is empty")
			}
			if n.Timestamp == "" {
				t.Error("Timestamp is empty")
			}
			if n.Type != ItemNotificationCreated && n.Type != ItemNotificationUpdated && n.Type != ItemNotificationDeleted {
				t.Errorf("Invalid notification type: %s", n.Type)
			}
		}
	})

	t.Run("TurnNotification", func(t *testing.T) {
		notifs := []TurnNotification{
			NewTurnNotification("turn_1", TurnNotificationStarted, StateActive),
			NewTurnNotification("turn_2", TurnNotificationCompleted, StateCompleted),
			NewTurnNotification("turn_3", TurnNotificationInterrupted, StateInterrupted),
		}

		for _, n := range notifs {
			if n.TurnID == "" {
				t.Error("TurnID is empty")
			}
			if n.StartedAt == "" {
				t.Error("StartedAt is empty")
			}
		}
	})

	t.Run("ThreadNotification", func(t *testing.T) {
		notifs := []ThreadNotification{
			NewThreadNotification("thread_1", ThreadNotificationStarted, StateRunning),
			NewThreadNotification("thread_2", ThreadNotificationPaused, StatePaused),
			NewThreadNotification("thread_3", ThreadNotificationClosed, StateClosed),
		}

		for _, n := range notifs {
			if n.ThreadID == "" {
				t.Error("ThreadID is empty")
			}
			if n.StartedAt == "" {
				t.Error("StartedAt is empty")
			}
		}
	})
}

// TestNotificationToJSONRPCConversion tests conversion to JSON-RPC format
func TestNotificationToJSONRPCConversion(t *testing.T) {
	t.Run("ItemNotification", func(t *testing.T) {
		itemNotif := NewItemNotification("item_123", ItemNotificationCreated, "pending", map[string]interface{}{
			"key": "value",
		})

		rpcNotif, err := itemNotif.ToJSONRPCNotification()
		if err != nil {
			t.Fatalf("ToJSONRPCNotification() error = %v", err)
		}

		if rpcNotif.JSONRPC != "2.0" {
			t.Errorf("JSONRPC version = %s, want '2.0'", rpcNotif.JSONRPC)
		}

		if rpcNotif.Method != "item/created" {
			t.Errorf("Method = %s, want 'item/created'", rpcNotif.Method)
		}

		if len(rpcNotif.Params) == 0 {
			t.Error("Params is empty")
		}

		// Verify params can be unmarshaled back
		var decoded ItemNotification
		if err := json.Unmarshal(rpcNotif.Params, &decoded); err != nil {
			t.Fatalf("Failed to unmarshal params: %v", err)
		}

		if decoded.ItemID != "item_123" {
			t.Errorf("Decoded ItemID = %s, want 'item_123'", decoded.ItemID)
		}
	})

	t.Run("TurnNotification", func(t *testing.T) {
		turnNotif := NewTurnNotification("turn_456", TurnNotificationCompleted, StateCompleted)

		rpcNotif, err := turnNotif.ToJSONRPCNotification()
		if err != nil {
			t.Fatalf("ToJSONRPCNotification() error = %v", err)
		}

		if rpcNotif.Method != "turn/completed" {
			t.Errorf("Method = %s, want 'turn/completed'", rpcNotif.Method)
		}

		var decoded TurnNotification
		if err := json.Unmarshal(rpcNotif.Params, &decoded); err != nil {
			t.Fatalf("Failed to unmarshal params: %v", err)
		}

		if decoded.TurnID != "turn_456" {
			t.Errorf("Decoded TurnID = %s, want 'turn_456'", decoded.TurnID)
		}
	})

	t.Run("ThreadNotification", func(t *testing.T) {
		threadNotif := NewThreadNotification("thread_789", ThreadNotificationStarted, StateRunning)

		rpcNotif, err := threadNotif.ToJSONRPCNotification()
		if err != nil {
			t.Fatalf("ToJSONRPCNotification() error = %v", err)
		}

		if rpcNotif.Method != "thread/started" {
			t.Errorf("Method = %s, want 'thread/started'", rpcNotif.Method)
		}

		var decoded ThreadNotification
		if err := json.Unmarshal(rpcNotif.Params, &decoded); err != nil {
			t.Fatalf("Failed to unmarshal params: %v", err)
		}

		if decoded.ThreadID != "thread_789" {
			t.Errorf("Decoded ThreadID = %s, want 'thread_789'", decoded.ThreadID)
		}
	})
}

// TestStreamWriter tests writing notifications to a stream
func TestStreamWriter(t *testing.T) {
	t.Run("WriteItemNotification", func(t *testing.T) {
		var buf bytes.Buffer
		sw := NewStreamWriter(&buf)

		itemNotif := NewItemNotification("item_1", ItemNotificationCreated, "pending", nil)
		if err := sw.WriteItemNotification(itemNotif); err != nil {
			t.Fatalf("WriteItemNotification() error = %v", err)
		}

		output := buf.String()
		if output == "" {
			t.Error("Output is empty")
		}

		// Verify output is valid NDJSON (one JSON object per line)
		lines := strings.Split(strings.TrimSpace(output), "\n")
		if len(lines) != 1 {
			t.Fatalf("Expected 1 line, got %d", len(lines))
		}

		var notif Notification
		if err := json.Unmarshal([]byte(lines[0]), &notif); err != nil {
			t.Fatalf("Failed to unmarshal output: %v", err)
		}

		if notif.Method != "item/created" {
			t.Errorf("Method = %s, want 'item/created'", notif.Method)
		}
	})

	t.Run("WriteTurnNotification", func(t *testing.T) {
		var buf bytes.Buffer
		sw := NewStreamWriter(&buf)

		turnNotif := NewTurnNotification("turn_1", TurnNotificationStarted, StateActive)
		if err := sw.WriteTurnNotification(turnNotif); err != nil {
			t.Fatalf("WriteTurnNotification() error = %v", err)
		}

		var notif Notification
		line := strings.TrimSpace(buf.String())
		if err := json.Unmarshal([]byte(line), &notif); err != nil {
			t.Fatalf("Failed to unmarshal output: %v", err)
		}

		if notif.Method != "turn/started" {
			t.Errorf("Method = %s, want 'turn/started'", notif.Method)
		}
	})

	t.Run("WriteThreadNotification", func(t *testing.T) {
		var buf bytes.Buffer
		sw := NewStreamWriter(&buf)

		threadNotif := NewThreadNotification("thread_1", ThreadNotificationStarted, StateRunning)
		if err := sw.WriteThreadNotification(threadNotif); err != nil {
			t.Fatalf("WriteThreadNotification() error = %v", err)
		}

		var notif Notification
		line := strings.TrimSpace(buf.String())
		if err := json.Unmarshal([]byte(line), &notif); err != nil {
			t.Fatalf("Failed to unmarshal output: %v", err)
		}

		if notif.Method != "thread/started" {
			t.Errorf("Method = %s, want 'thread/started'", notif.Method)
		}
	})

	t.Run("MultipleNotifications", func(t *testing.T) {
		var buf bytes.Buffer
		sw := NewStreamWriter(&buf)

		notifications := []struct {
			item   ItemNotification
			turn   TurnNotification
			thread ThreadNotification
		}{
			{
				item:   NewItemNotification("item_1", ItemNotificationCreated, "pending", nil),
				turn:   NewTurnNotification("turn_1", TurnNotificationStarted, StateActive),
				thread: NewThreadNotification("thread_1", ThreadNotificationStarted, StateRunning),
			},
		}

		for _, n := range notifications {
			if err := sw.WriteItemNotification(n.item); err != nil {
				t.Fatalf("WriteItemNotification() error = %v", err)
			}
			if err := sw.WriteTurnNotification(n.turn); err != nil {
				t.Fatalf("WriteTurnNotification() error = %v", err)
			}
			if err := sw.WriteThreadNotification(n.thread); err != nil {
				t.Fatalf("WriteThreadNotification() error = %v", err)
			}
		}

		output := strings.TrimSpace(buf.String())
		lines := strings.Split(output, "\n")

		if len(lines) != 3 {
			t.Fatalf("Expected 3 lines, got %d", len(lines))
		}

		// Verify each line is valid JSON
		for i, line := range lines {
			var notif Notification
			if err := json.Unmarshal([]byte(line), &notif); err != nil {
				t.Errorf("Line %d: failed to unmarshal: %v", i, err)
			}
			if notif.JSONRPC != "2.0" {
				t.Errorf("Line %d: JSONRPC version = %s, want '2.0'", i, notif.JSONRPC)
			}
		}
	})
}

// TestNotificationBroadcaster tests broadcasting notifications to multiple subscribers
func TestNotificationBroadcaster(t *testing.T) {
	t.Run("SingleSubscriber", func(t *testing.T) {
		broadcaster := NewNotificationBroadcaster()

		var buf bytes.Buffer
		sw := NewStreamWriter(&buf)

		unsubscribe := broadcaster.Subscribe(sw)
		defer unsubscribe()

		itemNotif := NewItemNotification("item_1", ItemNotificationCreated, "pending", nil)
		broadcaster.BroadcastItemNotification(itemNotif)

		output := strings.TrimSpace(buf.String())
		if output == "" {
			t.Error("Expected output, got empty string")
		}

		var notif Notification
		if err := json.Unmarshal([]byte(output), &notif); err != nil {
			t.Fatalf("Failed to unmarshal: %v", err)
		}

		if notif.Method != "item/created" {
			t.Errorf("Method = %s, want 'item/created'", notif.Method)
		}
	})

	t.Run("MultipleSubscribers", func(t *testing.T) {
		broadcaster := NewNotificationBroadcaster()

		var buf1, buf2, buf3 bytes.Buffer
		sw1 := NewStreamWriter(&buf1)
		sw2 := NewStreamWriter(&buf2)
		sw3 := NewStreamWriter(&buf3)

		broadcaster.Subscribe(sw1)
		broadcaster.Subscribe(sw2)
		broadcaster.Subscribe(sw3)

		turnNotif := NewTurnNotification("turn_1", TurnNotificationStarted, StateActive)
		broadcaster.BroadcastTurnNotification(turnNotif)

		// Verify all subscribers received the notification
		for i, buf := range []*bytes.Buffer{&buf1, &buf2, &buf3} {
			output := strings.TrimSpace(buf.String())
			if output == "" {
				t.Errorf("Subscriber %d: expected output, got empty string", i)
				continue
			}

			var notif Notification
			if err := json.Unmarshal([]byte(output), &notif); err != nil {
				t.Errorf("Subscriber %d: failed to unmarshal: %v", i, err)
				continue
			}

			if notif.Method != "turn/started" {
				t.Errorf("Subscriber %d: method = %s, want 'turn/started'", i, notif.Method)
			}
		}
	})

	t.Run("Unsubscribe", func(t *testing.T) {
		broadcaster := NewNotificationBroadcaster()

		var buf1, buf2 bytes.Buffer
		sw1 := NewStreamWriter(&buf1)
		sw2 := NewStreamWriter(&buf2)

		unsub1 := broadcaster.Subscribe(sw1)
		broadcaster.Subscribe(sw2)

		// Unsubscribe first subscriber
		unsub1()

		threadNotif := NewThreadNotification("thread_1", ThreadNotificationStarted, StateRunning)
		broadcaster.BroadcastThreadNotification(threadNotif)

		// sw1 should not receive notification
		if buf1.String() != "" {
			t.Error("Unsubscribed writer received notification")
		}

		// sw2 should receive notification
		if buf2.String() == "" {
			t.Error("Subscribed writer did not receive notification")
		}
	})

	t.Run("ConcurrentBroadcasts", func(t *testing.T) {
		broadcaster := NewNotificationBroadcaster()

		var buf bytes.Buffer
		sw := NewStreamWriter(&buf)

		broadcaster.Subscribe(sw)

		// Send multiple notifications concurrently
		done := make(chan bool, 10)
		for i := 0; i < 10; i++ {
			go func(idx int) {
				itemNotif := NewItemNotification(
					fmt.Sprintf("item_%d", idx),
					ItemNotificationCreated,
					"pending",
					nil,
				)
				broadcaster.BroadcastItemNotification(itemNotif)
				done <- true
			}(i)
		}

		// Wait for all goroutines
		for i := 0; i < 10; i++ {
			<-done
		}

		output := strings.TrimSpace(buf.String())
		lines := strings.Split(output, "\n")

		if len(lines) != 10 {
			t.Errorf("Expected 10 notifications, got %d", len(lines))
		}
	})
}

// TestStreamHandlerIntegration tests the full stream handler with notifications
func TestStreamHandlerIntegration(t *testing.T) {
	t.Run("HandleStreamWithNotifications", func(t *testing.T) {
		tmpDir := t.TempDir()
		rt, err := NewRuntime(tmpDir)
		if err != nil {
			t.Fatalf("NewRuntime() error = %v", err)
		}

		handler := NewStreamHandler(rt)

		// Create test request with streaming support
		reqBody := bytes.NewBufferString(``)
		req := httptest.NewRequest("POST", "/stream", reqBody)
		req.Header.Set("Accept", "application/x-ndjson")
		req.Header.Set("Content-Type", "application/x-ndjson")

		w := httptest.NewRecorder()

		// Handle stream
		ctx := context.Background()
		err = handler.HandleStream(ctx, w, req)

		// The request body is empty, so the stream should complete without error
		// Check that we got the initial thread notification
		if w.Code != http.StatusOK && w.Code != 0 {
			t.Errorf("Status code = %d, want 200 or 0 (streaming)", w.Code)
		}

		output := w.Body.String()
		if !strings.Contains(output, "thread/started") {
			t.Error("Expected thread/started notification in output")
		}
	})

	t.Run("HandleStreamWithJSONRPCRequest", func(t *testing.T) {
		tmpDir := t.TempDir()
		rt, err := NewRuntime(tmpDir)
		if err != nil {
			t.Fatalf("NewRuntime() error = %v", err)
		}

		handler := NewStreamHandler(rt)

		// Send a holon/status request
		reqBody := `{"jsonrpc":"2.0","id":1,"method":"holon/status","params":{}}`
		req := httptest.NewRequest("POST", "/stream", bytes.NewBufferString(reqBody))
		req.Header.Set("Accept", "application/x-ndjson")
		req.Header.Set("Content-Type", "application/x-ndjson")

		w := httptest.NewRecorder()

		ctx := context.Background()
		err = handler.HandleStream(ctx, w, req)

		if err != nil {
			t.Errorf("HandleStream() error = %v", err)
		}

		output := w.Body.String()
		lines := strings.Split(strings.TrimSpace(output), "\n")

		// Should have thread/started notification and status response
		if len(lines) < 1 {
			t.Error("Expected at least 1 line of output")
		}

		// Check that we got a valid JSON-RPC response
		foundResponse := false
		for _, line := range lines {
			var resp JSONRPCResponse
			if err := json.Unmarshal([]byte(line), &resp); err == nil {
				if resp.ID != nil && resp.Result != nil {
					foundResponse = true
					break
				}
			}
		}

		if !foundResponse {
			t.Error("Expected JSON-RPC response in output")
		}
	})
}

// TestNotificationEndToEnd tests the complete notification flow
func TestNotificationEndToEnd(t *testing.T) {
	t.Run("CompleteWorkflow", func(t *testing.T) {
		tmpDir := t.TempDir()
		rt, err := NewRuntime(tmpDir)
		if err != nil {
			t.Fatalf("NewRuntime() error = %v", err)
		}

		_ = NewStreamHandler(rt)
		broadcaster := NewNotificationBroadcaster()

		var buf bytes.Buffer
		sw := NewStreamWriter(&buf)
		broadcaster.Subscribe(sw)

		// Simulate a workflow: thread start -> turn start -> turn complete -> item created
		threadNotif := NewThreadNotification("thread_1", ThreadNotificationStarted, StateRunning)
		broadcaster.BroadcastThreadNotification(threadNotif)

		turnNotif := NewTurnNotification("turn_1", TurnNotificationStarted, StateActive)
		turnNotif.ThreadID = "thread_1"
		broadcaster.BroadcastTurnNotification(turnNotif)

		itemNotif := NewItemNotification("item_1", ItemNotificationCreated, "pending", map[string]interface{}{
			"text": "test item",
		})
		itemNotif.ThreadID = "thread_1"
		itemNotif.TurnID = "turn_1"
		broadcaster.BroadcastItemNotification(itemNotif)

		turnCompleteNotif := NewTurnNotification("turn_1", TurnNotificationCompleted, StateCompleted)
		turnCompleteNotif.ThreadID = "thread_1"
		turnCompleteNotif.CompletedAt = time.Now().Format(time.RFC3339)
		broadcaster.BroadcastTurnNotification(turnCompleteNotif)

		// Verify all notifications were sent
		output := strings.TrimSpace(buf.String())
		lines := strings.Split(output, "\n")

		expectedMethods := []string{
			"thread/started",
			"turn/started",
			"item/created",
			"turn/completed",
		}

		if len(lines) != len(expectedMethods) {
			t.Errorf("Expected %d notifications, got %d", len(expectedMethods), len(lines))
		}

		for i, expectedMethod := range expectedMethods {
			if i >= len(lines) {
				t.Errorf("Missing notification for method: %s", expectedMethod)
				continue
			}

			var notif Notification
			if err := json.Unmarshal([]byte(lines[i]), &notif); err != nil {
				t.Errorf("Line %d: failed to unmarshal: %v", i, err)
				continue
			}

			if notif.Method != expectedMethod {
				t.Errorf("Line %d: method = %s, want %s", i, notif.Method, expectedMethod)
			}
		}
	})
}

// TestNotificationConstants verifies notification constants are correct
func TestNotificationConstants(t *testing.T) {
	t.Run("ItemNotificationTypes", func(t *testing.T) {
		types := []string{
			ItemNotificationCreated,
			ItemNotificationUpdated,
			ItemNotificationDeleted,
		}
		expected := []string{"created", "updated", "deleted"}

		for i, typ := range types {
			if typ != expected[i] {
				t.Errorf("Item notification type %d = %s, want %s", i, typ, expected[i])
			}
		}
	})

	t.Run("TurnNotificationTypes", func(t *testing.T) {
		types := []string{
			TurnNotificationStarted,
			TurnNotificationCompleted,
			TurnNotificationInterrupted,
		}
		expected := []string{"started", "completed", "interrupted"}

		for i, typ := range types {
			if typ != expected[i] {
				t.Errorf("Turn notification type %d = %s, want %s", i, typ, expected[i])
			}
		}
	})

	t.Run("ThreadNotificationTypes", func(t *testing.T) {
		types := []string{
			ThreadNotificationStarted,
			ThreadNotificationResumed,
			ThreadNotificationPaused,
			ThreadNotificationClosed,
		}
		expected := []string{"started", "resumed", "paused", "closed"}

		for i, typ := range types {
			if typ != expected[i] {
				t.Errorf("Thread notification type %d = %s, want %s", i, typ, expected[i])
			}
		}
	})

	t.Run("States", func(t *testing.T) {
		states := []string{
			StateActive,
			StateCompleted,
			StateInterrupted,
			StateRunning,
			StatePaused,
			StateClosed,
		}
		expected := []string{"active", "completed", "interrupted", "running", "paused", "closed"}

		for i, state := range states {
			if state != expected[i] {
				t.Errorf("State %d = %s, want %s", i, state, expected[i])
			}
		}
	})
}

// TestStreamWriterClosed tests writing to a closed stream writer
func TestStreamWriterClosed(t *testing.T) {
	var buf bytes.Buffer
	sw := NewStreamWriter(&buf)

	// Close the writer
	sw.Close()

	itemNotif := NewItemNotification("item_1", ItemNotificationCreated, "pending", nil)
	err := sw.WriteItemNotification(itemNotif)

	if err == nil {
		t.Error("Expected error when writing to closed stream, got nil")
	}
}

func TestStreamWriterKeepAlive(t *testing.T) {
	var buf bytes.Buffer
	sw := NewStreamWriter(&buf)

	if err := sw.WriteKeepAlive(); err != nil {
		t.Fatalf("WriteKeepAlive() error = %v", err)
	}
	if got := buf.String(); got != "\n" {
		t.Fatalf("keep-alive output = %q, want %q", got, "\\n")
	}

	if err := sw.Close(); err != nil {
		t.Fatalf("Close() error = %v", err)
	}
	if err := sw.WriteKeepAlive(); err == nil {
		t.Fatal("expected error when writing keep-alive to closed stream")
	}
}

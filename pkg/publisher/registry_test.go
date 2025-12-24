package publisher

import (
	"testing"
)

// mockPublisher is a test implementation of Publisher
type mockPublisher struct {
	name string
}

func (m *mockPublisher) Publish(req PublishRequest) (PublishResult, error) {
	return PublishResult{
		Provider: m.name,
		Target:   req.Target,
		Success:  true,
	}, nil
}

func (m *mockPublisher) Name() string {
	return m.name
}

func (m *mockPublisher) Validate(req PublishRequest) error {
	return nil
}

func TestRegister(t *testing.T) {
	// Clean up registry before test
	Unregister("test1")
	Unregister("test2")

	t.Run("registers a publisher successfully", func(t *testing.T) {
		p := &mockPublisher{name: "test1"}
		err := Register(p)

		if err != nil {
			t.Fatalf("expected no error, got %v", err)
		}

		if !IsRegistered("test1") {
			t.Error("publisher was not registered")
		}
	})

	t.Run("returns error when registering nil publisher", func(t *testing.T) {
		err := Register(nil)

		if err == nil {
			t.Error("expected error for nil publisher, got nil")
		}
	})

	t.Run("returns error when publisher name is empty", func(t *testing.T) {
		p := &mockPublisher{name: ""}
		err := Register(p)

		if err == nil {
			t.Error("expected error for empty name, got nil")
		}
	})

	t.Run("returns error when duplicate name is registered", func(t *testing.T) {
		p1 := &mockPublisher{name: "test2"}
		p2 := &mockPublisher{name: "test2"}

		err := Register(p1)
		if err != nil {
			t.Fatalf("failed to register first publisher: %v", err)
		}

		err = Register(p2)
		if err == nil {
			t.Error("expected error for duplicate registration, got nil")
		}
	})

	// Clean up
	Unregister("test1")
	Unregister("test2")
}

func TestUnregister(t *testing.T) {
	// Clean up registry before test
	Unregister("test-unregister")

	t.Run("unregisters an existing publisher", func(t *testing.T) {
		p := &mockPublisher{name: "test-unregister"}
		_ = Register(p)

		err := Unregister("test-unregister")
		if err != nil {
			t.Fatalf("expected no error, got %v", err)
		}

		if IsRegistered("test-unregister") {
			t.Error("publisher was not unregistered")
		}
	})

	t.Run("returns error when unregistering non-existent publisher", func(t *testing.T) {
		err := Unregister("nonexistent")

		if err == nil {
			t.Error("expected error for non-existent publisher, got nil")
		}
	})
}

func TestGet(t *testing.T) {
	// Clean up registry before test
	Unregister("test-get")

	t.Run("returns registered publisher", func(t *testing.T) {
		p := &mockPublisher{name: "test-get"}
		_ = Register(p)

		retrieved := Get("test-get")
		if retrieved == nil {
			t.Error("expected publisher, got nil")
		}

		if retrieved.Name() != "test-get" {
			t.Errorf("expected name 'test-get', got '%s'", retrieved.Name())
		}
	})

	t.Run("returns nil for non-existent publisher", func(t *testing.T) {
		retrieved := Get("nonexistent")
		if retrieved != nil {
			t.Error("expected nil for non-existent publisher")
		}
	})

	// Clean up
	Unregister("test-get")
}

func TestList(t *testing.T) {
	// Clean up registry before test
	Unregister("test-list-1")
	Unregister("test-list-2")
	Unregister("test-list-3")

	t.Run("returns list when no publishers registered", func(t *testing.T) {
		// This test assumes no other publishers are registered
		// In real scenarios, there might be some publishers already
		names := List()
		// Just verify it returns a list (may or may not be empty)
		if names == nil {
			t.Error("expected list, got nil")
		}
	})

	t.Run("returns list of registered publishers", func(t *testing.T) {
		_ = Register(&mockPublisher{name: "test-list-1"})
		_ = Register(&mockPublisher{name: "test-list-2"})
		_ = Register(&mockPublisher{name: "test-list-3"})

		names := List()
		found := 0
		for _, name := range names {
			if name == "test-list-1" || name == "test-list-2" || name == "test-list-3" {
				found++
			}
		}

		if found != 3 {
			t.Errorf("expected to find 3 test publishers, found %d", found)
		}
	})

	// Clean up
	Unregister("test-list-1")
	Unregister("test-list-2")
	Unregister("test-list-3")
}

func TestIsRegistered(t *testing.T) {
	// Clean up registry before test
	Unregister("test-is-registered")

	t.Run("returns false for non-existent publisher", func(t *testing.T) {
		if IsRegistered("nonexistent") {
			t.Error("expected false for non-existent publisher")
		}
	})

	t.Run("returns true for registered publisher", func(t *testing.T) {
		p := &mockPublisher{name: "test-is-registered"}
		_ = Register(p)

		if !IsRegistered("test-is-registered") {
			t.Error("expected true for registered publisher")
		}
	})

	// Clean up
	Unregister("test-is-registered")
}

package workspace

import (
	"context"
	"testing"
)

// mockPreparer is a mock implementation of Preparer for testing
type mockPreparer struct {
	name string
}

func (m *mockPreparer) Prepare(ctx context.Context, req PrepareRequest) (PrepareResult, error) {
	return PrepareResult{}, nil
}

func (m *mockPreparer) Name() string {
	return m.name
}

func (m *mockPreparer) Validate(req PrepareRequest) error {
	return nil
}

func (m *mockPreparer) Cleanup(dest string) error {
	return nil
}

func TestRegister(t *testing.T) {
	t.Run("register valid preparer", func(t *testing.T) {
		// Create a mock preparer with a unique name
		preparer := &mockPreparer{name: "test-preparer-1"}

		// Register the preparer
		err := Register(preparer)
		if err != nil {
			t.Fatalf("Register() failed: %v", err)
		}

		// Verify it's registered
		if !IsRegistered("test-preparer-1") {
			t.Error("preparer was not registered")
		}

		// Clean up
		_ = Unregister("test-preparer-1")
	})

	t.Run("register nil preparer", func(t *testing.T) {
		err := Register(nil)
		if err == nil {
			t.Error("expected error when registering nil preparer")
		}
	})

	t.Run("register preparer with empty name", func(t *testing.T) {
		preparer := &mockPreparer{name: ""}

		err := Register(preparer)
		if err == nil {
			t.Error("expected error when registering preparer with empty name")
		}
	})

	t.Run("register duplicate name", func(t *testing.T) {
		preparer1 := &mockPreparer{name: "test-preparer-dup"}
		preparer2 := &mockPreparer{name: "test-preparer-dup"}

		// Register first preparer
		err := Register(preparer1)
		if err != nil {
			t.Fatalf("Register() failed: %v", err)
		}

		// Try to register second preparer with same name
		err = Register(preparer2)
		if err == nil {
			t.Error("expected error when registering duplicate name")
		}

		// Clean up
		_ = Unregister("test-preparer-dup")
	})
}

func TestUnregister(t *testing.T) {
	t.Run("unregister existing preparer", func(t *testing.T) {
		preparer := &mockPreparer{name: "test-preparer-2"}

		// Register the preparer
		_ = Register(preparer)

		// Unregister it
		err := Unregister("test-preparer-2")
		if err != nil {
			t.Fatalf("Unregister() failed: %v", err)
		}

		// Verify it's unregistered
		if IsRegistered("test-preparer-2") {
			t.Error("preparer was not unregistered")
		}
	})

	t.Run("unregister non-existent preparer", func(t *testing.T) {
		err := Unregister("non-existent-preparer")
		if err == nil {
			t.Error("expected error when unregistering non-existent preparer")
		}
	})
}

func TestGet(t *testing.T) {
	t.Run("get existing preparer", func(t *testing.T) {
		preparer := &mockPreparer{name: "test-preparer-3"}

		// Register the preparer
		_ = Register(preparer)

		// Get it
		got := Get("test-preparer-3")
		if got == nil {
			t.Error("Get() returned nil")
		}
		if got.Name() != "test-preparer-3" {
			t.Errorf("got name %s, want test-preparer-3", got.Name())
		}

		// Clean up
		_ = Unregister("test-preparer-3")
	})

	t.Run("get non-existent preparer", func(t *testing.T) {
		got := Get("non-existent-preparer")
		if got != nil {
			t.Error("Get() should return nil for non-existent preparer")
		}
	})
}

func TestList(t *testing.T) {
	// Clean up any existing preparers
	names := List()
	for _, name := range names {
		_ = Unregister(name)
	}

	// Register some preparers
	_ = Register(&mockPreparer{name: "test-preparer-a"})
	_ = Register(&mockPreparer{name: "test-preparer-b"})
	_ = Register(&mockPreparer{name: "test-preparer-c"})

	// List them
	got := List()
	if len(got) < 3 {
		t.Errorf("List() returned %d preparers, want at least 3", len(got))
	}

	// Check that our preparers are in the list
	hasA := false
	hasB := false
	hasC := false
	for _, name := range got {
		if name == "test-preparer-a" {
			hasA = true
		}
		if name == "test-preparer-b" {
			hasB = true
		}
		if name == "test-preparer-c" {
			hasC = true
		}
	}
	if !hasA || !hasB || !hasC {
		t.Error("List() did not contain all registered preparers")
	}

	// Clean up
	_ = Unregister("test-preparer-a")
	_ = Unregister("test-preparer-b")
	_ = Unregister("test-preparer-c")
}

func TestIsRegistered(t *testing.T) {
	preparer := &mockPreparer{name: "test-preparer-4"}

	// Not registered yet
	if IsRegistered("test-preparer-4") {
		t.Error("preparer should not be registered yet")
	}

	// Register it
	_ = Register(preparer)

	// Now it should be registered
	if !IsRegistered("test-preparer-4") {
		t.Error("preparer should be registered")
	}

	// Clean up
	_ = Unregister("test-preparer-4")

	// Not registered again
	if IsRegistered("test-preparer-4") {
		t.Error("preparer should not be registered after unregister")
	}
}

package registry

import (
	"context"
	"testing"

	"github.com/holon-run/holon/pkg/context/collector"
)

// mockCollector is a mock implementation of the collector.Collector interface for testing
type mockCollector struct {
	name string
}

func (m *mockCollector) Collect(ctx context.Context, req collector.CollectRequest) (collector.CollectResult, error) {
	return collector.CollectResult{
		Provider: m.name,
		Success:  true,
	}, nil
}

func (m *mockCollector) Name() string {
	return m.name
}

func (m *mockCollector) Validate(req collector.CollectRequest) error {
	return nil
}

func TestRegister(t *testing.T) {
	// Clean up registry before test
	Unregister("test1")
	Unregister("test2")
	Unregister("test3")

	tests := []struct {
		name        string
		collector   collector.Collector
		wantErr     bool
		errContains string
	}{
		{
			name:      "register valid collector",
			collector: &mockCollector{name: "test1"},
			wantErr:   false,
		},
		{
			name:      "register another valid collector",
			collector: &mockCollector{name: "test2"},
			wantErr:   false,
		},
		{
			name:        "register nil collector",
			collector:   nil,
			wantErr:     true,
			errContains: "cannot register nil collector",
		},
		{
			name: "register collector with empty name",
			collector: &mockCollector{
				name: "",
			},
			wantErr:     true,
			errContains: "collector name cannot be empty",
		},
		{
			name:        "register duplicate collector",
			collector:   &mockCollector{name: "test1"},
			wantErr:     true,
			errContains: "already registered",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := Register(tt.collector)
			if (err != nil) != tt.wantErr {
				t.Errorf("Register() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if err != nil && tt.errContains != "" {
				if !contains(err.Error(), tt.errContains) {
					t.Errorf("Register() error = %v, should contain %q", err, tt.errContains)
				}
			}
		})
	}

	// Clean up
	Unregister("test1")
	Unregister("test2")
}

func TestUnregister(t *testing.T) {
	// Setup
	c1 := &mockCollector{name: "test-unregister-1"}
	c2 := &mockCollector{name: "test-unregister-2"}
	Register(c1)
	Register(c2)

	tests := []struct {
		name        string
		collectorName string
		wantErr     bool
		errContains string
	}{
		{
			name:         "unregister existing collector",
			collectorName: "test-unregister-1",
			wantErr:      false,
		},
		{
			name:         "unregister non-existent collector",
			collectorName: "does-not-exist",
			wantErr:      true,
			errContains:  "not registered",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := Unregister(tt.collectorName)
			if (err != nil) != tt.wantErr {
				t.Errorf("Unregister() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if err != nil && tt.errContains != "" {
				if !contains(err.Error(), tt.errContains) {
					t.Errorf("Unregister() error = %v, should contain %q", err, tt.errContains)
				}
			}
		})
	}

	// Clean up
	Unregister("test-unregister-2")
}

func TestGet(t *testing.T) {
	// Setup
	c1 := &mockCollector{name: "test-get-1"}
	Register(c1)

	tests := []struct {
		name         string
		collectorName string
		wantNil      bool
	}{
		{
			name:         "get existing collector",
			collectorName: "test-get-1",
			wantNil:      false,
		},
		{
			name:         "get non-existent collector",
			collectorName: "does-not-exist",
			wantNil:      true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := Get(tt.collectorName)
			if (got == nil) != tt.wantNil {
				t.Errorf("Get() = %v, wantNil %v", got, tt.wantNil)
			}
			if got != nil && got.Name() != tt.collectorName {
				t.Errorf("Get() name = %v, want %v", got.Name(), tt.collectorName)
			}
		})
	}

	// Clean up
	Unregister("test-get-1")
}

func TestList(t *testing.T) {
	// Clean up
	Unregister("test-list-1")
	Unregister("test-list-2")
	Unregister("test-list-3")

	// Get initial list (may or may not have providers from other init functions)
	initialList := List()
	initialCount := len(initialList)

	// Register test collectors
	Register(&mockCollector{name: "test-list-1"})
	Register(&mockCollector{name: "test-list-2"})
	Register(&mockCollector{name: "test-list-3"})

	list := List()
	if len(list) < initialCount+3 {
		t.Errorf("List() should have at least %d collectors, got %d", initialCount+3, len(list))
	}

	// Check that our test collectors are in the list
	found := 0
	for _, name := range list {
		if name == "test-list-1" || name == "test-list-2" || name == "test-list-3" {
			found++
		}
	}
	if found != 3 {
		t.Errorf("List() should contain all 3 test collectors, found %d", found)
	}

	// Clean up
	Unregister("test-list-1")
	Unregister("test-list-2")
	Unregister("test-list-3")
}

func TestIsRegistered(t *testing.T) {
	// Clean up
	Unregister("test-registered-1")
	Unregister("test-registered-2")

	// Register test collector
	Register(&mockCollector{name: "test-registered-1"})

	tests := []struct {
		name         string
		collectorName string
		wantRegistered bool
	}{
		{
			name:         "check registered collector",
			collectorName: "test-registered-1",
			wantRegistered: true,
		},
		{
			name:         "check non-registered collector",
			collectorName: "test-registered-2",
			wantRegistered: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := IsRegistered(tt.collectorName)
			if got != tt.wantRegistered {
				t.Errorf("IsRegistered() = %v, want %v", got, tt.wantRegistered)
			}
		})
	}

	// Clean up
	Unregister("test-registered-1")
}

// contains is a helper function to check if a string contains a substring
func contains(s, substr string) bool {
	return len(s) >= len(substr) && findSubstring(s, substr)
}

func findSubstring(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}

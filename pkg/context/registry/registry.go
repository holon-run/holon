package registry

import (
	"fmt"
	"sync"

	"github.com/holon-run/holon/pkg/context/collector"
)

var (
	mu         sync.RWMutex
	collectors = make(map[string]collector.Collector)
)

// Register registers a collector with a given name.
// If a collector with the same name is already registered, it returns an error.
func Register(collector collector.Collector) error {
	if collector == nil {
		return fmt.Errorf("cannot register nil collector")
	}

	name := collector.Name()
	if name == "" {
		return fmt.Errorf("collector name cannot be empty")
	}

	mu.Lock()
	defer mu.Unlock()

	if _, exists := collectors[name]; exists {
		return fmt.Errorf("collector '%s' is already registered", name)
	}

	collectors[name] = collector
	return nil
}

// Unregister removes a collector from the registry.
func Unregister(name string) error {
	mu.Lock()
	defer mu.Unlock()

	if _, exists := collectors[name]; !exists {
		return fmt.Errorf("collector '%s' is not registered", name)
	}

	delete(collectors, name)
	return nil
}

// Get retrieves a collector by name.
// Returns nil if the collector is not found.
func Get(name string) collector.Collector {
	mu.RLock()
	defer mu.RUnlock()

	return collectors[name]
}

// List returns all registered collector names.
func List() []string {
	mu.RLock()
	defer mu.RUnlock()

	names := make([]string, 0, len(collectors))
	for name := range collectors {
		names = append(names, name)
	}
	return names
}

// IsRegistered checks if a collector with the given name is registered.
func IsRegistered(name string) bool {
	mu.RLock()
	defer mu.RUnlock()

	_, exists := collectors[name]
	return exists
}

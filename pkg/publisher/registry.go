package publisher

import (
	"fmt"
	"sync"
)

var (
	mu        sync.RWMutex
	publishers = make(map[string]Publisher)
)

// Register registers a publisher with a given name.
// If a publisher with the same name is already registered, it returns an error.
func Register(publisher Publisher) error {
	if publisher == nil {
		return fmt.Errorf("cannot register nil publisher")
	}

	name := publisher.Name()
	if name == "" {
		return fmt.Errorf("publisher name cannot be empty")
	}

	mu.Lock()
	defer mu.Unlock()

	if _, exists := publishers[name]; exists {
		return fmt.Errorf("publisher '%s' is already registered", name)
	}

	publishers[name] = publisher
	return nil
}

// Unregister removes a publisher from the registry.
func Unregister(name string) error {
	mu.Lock()
	defer mu.Unlock()

	if _, exists := publishers[name]; !exists {
		return fmt.Errorf("publisher '%s' is not registered", name)
	}

	delete(publishers, name)
	return nil
}

// Get retrieves a publisher by name.
// Returns nil if the publisher is not found.
func Get(name string) Publisher {
	mu.RLock()
	defer mu.RUnlock()

	return publishers[name]
}

// List returns all registered publisher names.
func List() []string {
	mu.RLock()
	defer mu.RUnlock()

	names := make([]string, 0, len(publishers))
	for name := range publishers {
		names = append(names, name)
	}
	return names
}

// IsRegistered checks if a publisher with the given name is registered.
func IsRegistered(name string) bool {
	mu.RLock()
	defer mu.RUnlock()

	_, exists := publishers[name]
	return exists
}

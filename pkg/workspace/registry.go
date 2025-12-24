package workspace

import (
	"fmt"
	"sync"
)

var (
	mu         sync.RWMutex
	preparers  = make(map[string]Preparer)
)

// init registers the built-in workspace preparers.
func init() {
	// Register built-in preparers
	if err := Register(NewGitClonePreparer()); err != nil {
		panic(fmt.Sprintf("failed to register git-clone preparer: %v", err))
	}
	if err := Register(NewSnapshotPreparer()); err != nil {
		panic(fmt.Sprintf("failed to register snapshot preparer: %v", err))
	}
	if err := Register(NewExistingPreparer()); err != nil {
		panic(fmt.Sprintf("failed to register existing preparer: %v", err))
	}
}

// Register registers a preparer with a given name.
// If a preparer with the same name is already registered, it returns an error.
func Register(preparer Preparer) error {
	if preparer == nil {
		return fmt.Errorf("cannot register nil preparer")
	}

	name := preparer.Name()
	if name == "" {
		return fmt.Errorf("preparer name cannot be empty")
	}

	mu.Lock()
	defer mu.Unlock()

	if _, exists := preparers[name]; exists {
		return fmt.Errorf("preparer '%s' is already registered", name)
	}

	preparers[name] = preparer
	return nil
}

// Unregister removes a preparer from the registry.
func Unregister(name string) error {
	mu.Lock()
	defer mu.Unlock()

	if _, exists := preparers[name]; !exists {
		return fmt.Errorf("preparer '%s' is not registered", name)
	}

	delete(preparers, name)
	return nil
}

// Get retrieves a preparer by name.
// Returns nil if the preparer is not found.
func Get(name string) Preparer {
	mu.RLock()
	defer mu.RUnlock()

	return preparers[name]
}

// List returns all registered preparer names.
func List() []string {
	mu.RLock()
	defer mu.RUnlock()

	names := make([]string, 0, len(preparers))
	for name := range preparers {
		names = append(names, name)
	}
	return names
}

// IsRegistered checks if a preparer with the given name is registered.
func IsRegistered(name string) bool {
	mu.RLock()
	defer mu.RUnlock()

	_, exists := preparers[name]
	return exists
}

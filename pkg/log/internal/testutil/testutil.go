// Package testutil provides testing utilities for the log package.
package testutil

import (
	"io"
)

// originalOutput stores the original stdout
var originalOutput io.Writer

// SetOutput sets the output for testing purposes
// This is a simplified version - in a real implementation you might want
// to use zap's WriteSyncer and test more thoroughly
func SetOutput(w io.Writer) {
	originalOutput = w
	// Note: This doesn't actually redirect zap's output since zap was already initialized
	// In production tests, you'd want to pass a WriteSyncer during Init()
	// This is a placeholder for more sophisticated test setup
}

// RestoreOutput restores the original output
func RestoreOutput() {
	// No-op for now since we're not actually redirecting output
	_ = originalOutput
}

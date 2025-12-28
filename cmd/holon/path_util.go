package main

import (
	"path/filepath"
)

// samePath returns true if two paths refer to the same location after cleaning.
func samePath(a, b string) bool {
	if a == "" || b == "" {
		return false
	}
	cleanA := filepath.Clean(a)
	cleanB := filepath.Clean(b)
	return cleanA == cleanB
}

package main

import (
	"path/filepath"
)

// cleanAbs returns the absolute path, resolving symlinks if possible.
// This helper ensures robust path comparison for samePath.
func cleanAbs(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	if resolved, err := filepath.EvalSymlinks(abs); err == nil {
		return resolved, nil
	}
	return abs, nil
}

// samePath returns true if two paths refer to the same location after
// resolving to absolute paths and following symlinks.
func samePath(a, b string) bool {
	if a == "" || b == "" {
		return false
	}
	absA, err := cleanAbs(a)
	if err != nil {
		return false
	}
	absB, err := cleanAbs(b)
	if err != nil {
		return false
	}
	return absA == absB
}

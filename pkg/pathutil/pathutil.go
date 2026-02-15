package pathutil

import (
	"path/filepath"
	"strings"
)

// PathOverlaps reports whether one path equals or contains the other.
func PathOverlaps(a, b string) bool {
	if a == b {
		return true
	}
	relAB, err := filepath.Rel(a, b)
	if err == nil && relAB != "." && relAB != ".." && !strings.HasPrefix(relAB, ".."+string(filepath.Separator)) {
		return true
	}
	relBA, err := filepath.Rel(b, a)
	if err == nil && relBA != "." && relBA != ".." && !strings.HasPrefix(relBA, ".."+string(filepath.Separator)) {
		return true
	}
	return false
}

// IsFilesystemRoot reports whether path points to filesystem root (POSIX or Windows volume root).
func IsFilesystemRoot(path string) bool {
	clean := filepath.Clean(path)
	if clean == string(filepath.Separator) {
		return true
	}
	volume := filepath.VolumeName(clean)
	return volume != "" && clean == volume+string(filepath.Separator)
}

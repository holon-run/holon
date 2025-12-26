// Package image provides version parsing and normalization for language version detection.
package image

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

// Version represents a parsed language version with major, minor, and patch components.
type Version struct {
	Major int
	Minor int
	Patch int
	Original string
}

// ParseVersion parses a version string and returns a Version struct.
// Supports various version formats:
// - "1.22" -> {Major: 1, Minor: 22, Patch: 0}
// - "1.22.0" -> {Major: 1, Minor: 22, Patch: 0}
// - "22" -> {Major: 22, Minor: 0, Patch: 0}
func ParseVersion(version string) (*Version, error) {
	version = strings.TrimSpace(version)

	// Strip common version range prefixes
	version = stripVersionRanges(version)
	if version == "" {
		return nil, fmt.Errorf("empty version after stripping ranges")
	}

	// Split by dots
	parts := strings.Split(version, ".")

	v := &Version{Original: version}

	switch len(parts) {
	case 1:
		// "22" or "v22"
		major, err := parseVersionNumber(parts[0])
		if err != nil {
			return nil, err
		}
		v.Major = major
		v.Minor = 0
		v.Patch = 0
	case 2:
		// "1.22" or "v1.22"
		major, err := parseVersionNumber(parts[0])
		if err != nil {
			return nil, err
		}
		minor, err := parseVersionNumber(parts[1])
		if err != nil {
			return nil, err
		}
		v.Major = major
		v.Minor = minor
		v.Patch = 0
	case 3:
		// "1.22.0" or "v1.22.0"
		major, err := parseVersionNumber(parts[0])
		if err != nil {
			return nil, err
		}
		minor, err := parseVersionNumber(parts[1])
		if err != nil {
			return nil, err
		}
		patch, err := parseVersionNumber(parts[2])
		if err != nil {
			return nil, err
		}
		v.Major = major
		v.Minor = minor
		v.Patch = patch
	default:
		return nil, fmt.Errorf("invalid version format: %s", version)
	}

	return v, nil
}

// parseVersionNumber parses a single version number component, stripping 'v' prefix if present.
func parseVersionNumber(s string) (int, error) {
	s = strings.TrimSpace(s)
	s = strings.TrimPrefix(s, "v")
	s = strings.TrimPrefix(s, "V")

	if s == "" {
		return 0, fmt.Errorf("empty version component")
	}

	num, err := strconv.Atoi(s)
	if err != nil {
		return 0, fmt.Errorf("invalid version number: %s", s)
	}

	if num < 0 {
		return 0, fmt.Errorf("version number cannot be negative: %s", s)
	}

	return num, nil
}

// stripVersionRanges removes version range operators and modifiers.
// Handles: ^, ~, >=, >, <=, <, =, ~>, x, *, any, latest
func stripVersionRanges(version string) string {
	version = strings.TrimSpace(version)

	// Remove range operators and modifiers
	prefixes := []string{"^", "~", ">=", ">", "<=", "<", "=", "~>", "v", "V"}
	for _, prefix := range prefixes {
		if strings.HasPrefix(version, prefix) {
			version = strings.TrimPrefix(version, prefix)
			version = strings.TrimSpace(version)
			break
		}
	}

	// Handle "x", "*", "any", "latest" as wildcards - return empty to indicate use default
	lower := strings.ToLower(version)
	if lower == "x" || lower == "*" || lower == "any" || lower == "latest" {
		return ""
	}

	// Remove trailing wildcards like "1.x" or "1.*"
	re := regexp.MustCompile(`\.(\*|x|X)$`)
	version = re.ReplaceAllString(version, "")

	return version
}

// String returns the version in "major.minor" format (patch is omitted for Docker images).
func (v *Version) String() string {
	if v.Minor == 0 {
		return fmt.Sprintf("%d", v.Major)
	}
	return fmt.Sprintf("%d.%d", v.Major, v.Minor)
}

// ImageString returns the version formatted for Docker image tags.
// For most languages, this is "major.minor", but some languages use just major (Node.js).
func (v *Version) ImageString(useMajorOnly bool) string {
	if useMajorOnly || v.Minor == 0 {
		return fmt.Sprintf("%d", v.Major)
	}
	return fmt.Sprintf("%d.%d", v.Major, v.Minor)
}

// IsZero returns true if the version is all zeros.
func (v *Version) IsZero() bool {
	return v.Major == 0 && v.Minor == 0 && v.Patch == 0
}

// Compare compares two versions.
// Returns: -1 if v < other, 0 if v == other, 1 if v > other
func (v *Version) Compare(other *Version) int {
	if v.Major != other.Major {
		if v.Major < other.Major {
			return -1
		}
		return 1
	}
	if v.Minor != other.Minor {
		if v.Minor < other.Minor {
			return -1
		}
		return 1
	}
	if v.Patch != other.Patch {
		if v.Patch < other.Patch {
			return -1
		}
		return 1
	}
	return 0
}

// NormalizeVersionRange attempts to normalize a version range to a specific version.
// For broad ranges (e.g., "^1.0.0", ">=2"), it returns a suggested LTS version.
// Returns empty string if the range is too broad and a default should be used.
func NormalizeVersionRange(versionRange string, ltsVersion string) string {
	versionRange = strings.TrimSpace(versionRange)
	if versionRange == "" {
		return ""
	}

	// Handle range operators first
	lower := strings.ToLower(versionRange)

	// Wildcard ranges: "1.x", "1.*", "x", "*", "any", "latest" -> use LTS
	if strings.Contains(lower, ".x") || strings.Contains(lower, ".*") ||
		lower == "x" || lower == "*" || lower == "any" || lower == "latest" {
		return ltsVersion
	}

	// Caret range (^1.2.3) - compatible with minor/patch updates
	if strings.HasPrefix(versionRange, "^") {
		rest := strings.TrimPrefix(versionRange, "^")
		if v, err := ParseVersion(rest); err == nil && !v.IsZero() {
			// Return the minor version (e.g., ^1.2 -> 1.2)
			return v.String()
		}
	}

	// Tilde range (~1.2.3) - compatible with patch updates only
	if strings.HasPrefix(versionRange, "~") {
		rest := strings.TrimPrefix(versionRange, "~")
		if v, err := ParseVersion(rest); err == nil && !v.IsZero() {
			return v.String()
		}
	}

	// Greater than or equal (>=1.2.3)
	if strings.HasPrefix(versionRange, ">=") {
		rest := strings.TrimPrefix(versionRange, ">=")
		if v, err := ParseVersion(rest); err == nil && !v.IsZero() {
			return v.String()
		}
	}

	// Parse as regular version (exact versions)
	if v, err := ParseVersion(versionRange); err == nil && !v.IsZero() {
		return v.String()
	}

	// Couldn't parse - return empty to signal use default
	return ""
}

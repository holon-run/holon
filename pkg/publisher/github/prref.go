package github

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

var (
	// PR ref patterns:
	// - owner/repo/pr/123
	// - owner/repo#123
	// - owner/repo/pull/123
	// - owner/repo 123 (if number provided separately)
	prRefPattern1 = regexp.MustCompile(`^([^/]+)/([^/]+)/pr/(\d+)$`)
	prRefPattern2 = regexp.MustCompile(`^([^/]+)/([^/]+)#(\d+)$`)
	prRefPattern3 = regexp.MustCompile(`^([^/]+)/([^/]+)/pull/(\d+)$`)
)

// ParsePRRef parses a PR reference string into its components.
// Supported formats:
//   - owner/repo/pr/123
//   - owner/repo#123
//   - owner/repo/pull/123
func ParsePRRef(target string) (*PRRef, error) {
	target = strings.TrimSpace(target)

	// Try pattern 1: owner/repo/pr/123
	if matches := prRefPattern1.FindStringSubmatch(target); matches != nil {
		prNum, _ := strconv.Atoi(matches[3])
		return &PRRef{
			Owner:    matches[1],
			Repo:     matches[2],
			PRNumber: prNum,
		}, nil
	}

	// Try pattern 2: owner/repo#123
	if matches := prRefPattern2.FindStringSubmatch(target); matches != nil {
		prNum, _ := strconv.Atoi(matches[3])
		return &PRRef{
			Owner:    matches[1],
			Repo:     matches[2],
			PRNumber: prNum,
		}, nil
	}

	// Try pattern 3: owner/repo/pull/123
	if matches := prRefPattern3.FindStringSubmatch(target); matches != nil {
		prNum, _ := strconv.Atoi(matches[3])
		return &PRRef{
			Owner:    matches[1],
			Repo:     matches[2],
			PRNumber: prNum,
		}, nil
	}

	return nil, fmt.Errorf("invalid PR reference format: %s (expected: owner/repo/pr/123, owner/repo#123, or owner/repo/pull/123)", target)
}

// String returns the string representation of the PR reference.
func (r PRRef) String() string {
	return fmt.Sprintf("%s/%s/pr/%d", r.Owner, r.Repo, r.PRNumber)
}

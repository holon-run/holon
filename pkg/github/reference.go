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
	// - owner/repo:base_branch (for PR publisher)
	prRefPattern1 = regexp.MustCompile(`^([^/]+)/([^/]+)/pr/(\d+)$`)
	prRefPattern2 = regexp.MustCompile(`^([^/]+)/([^/]+)#(\d+)$`)
	prRefPattern3 = regexp.MustCompile(`^([^/]+)/([^/]+)/pull/(\d+)$`)
	// For publisher with optional base branch
	prRefPattern4 = regexp.MustCompile(`^([^/]+)/([^/]+):([^/]+)$`)
)

// RefType represents the type of GitHub reference
type RefType int

const (
	// RefTypePR is a pull request reference
	RefTypePR RefType = iota
	// RefTypeIssue is an issue reference
	RefTypeIssue
	// RefTypeRepo is a repository reference
	RefTypeRepo
)

// Ref represents a parsed GitHub reference
type Ref struct {
	Owner      string
	Repo       string
	Number     int
	Kind       RefType
	BaseBranch string // For repo refs (e.g., owner/repo:main)
}

// ParseRef parses a GitHub reference string into its components.
// Supported formats:
//   - owner/repo/pr/123 (PR reference)
//   - owner/repo#123 (PR reference)
//   - owner/repo/pull/123 (PR reference)
//   - owner/repo:base_branch (Repository reference with base branch)
func ParseRef(target string) (*Ref, error) {
	target = strings.TrimSpace(target)

	// Try pattern 1: owner/repo/pr/123
	if matches := prRefPattern1.FindStringSubmatch(target); matches != nil {
		num, _ := strconv.Atoi(matches[3])
		return &Ref{
			Owner: matches[1],
			Repo:  matches[2],
			Number: num,
			Kind:  RefTypePR,
		}, nil
	}

	// Try pattern 2: owner/repo#123
	if matches := prRefPattern2.FindStringSubmatch(target); matches != nil {
		num, _ := strconv.Atoi(matches[3])
		return &Ref{
			Owner: matches[1],
			Repo:  matches[2],
			Number: num,
			Kind:  RefTypePR,
		}, nil
	}

	// Try pattern 3: owner/repo/pull/123
	if matches := prRefPattern3.FindStringSubmatch(target); matches != nil {
		num, _ := strconv.Atoi(matches[3])
		return &Ref{
			Owner: matches[1],
			Repo:  matches[2],
			Number: num,
			Kind:  RefTypePR,
		}, nil
	}

	// Try pattern 4: owner/repo:base_branch (for PR publisher)
	if matches := prRefPattern4.FindStringSubmatch(target); matches != nil {
		return &Ref{
			Owner:      matches[1],
			Repo:       matches[2],
			BaseBranch: matches[3],
			Kind:       RefTypeRepo,
		}, nil
	}

	return nil, fmt.Errorf("invalid GitHub reference format: %s (expected: owner/repo/pr/123, owner/repo#123, owner/repo/pull/123, or owner/repo:base_branch)", target)
}

// String returns the string representation of the reference
func (r *Ref) String() string {
	switch r.Kind {
	case RefTypePR:
		return fmt.Sprintf("%s/%s/pr/%d", r.Owner, r.Repo, r.Number)
	case RefTypeRepo:
		if r.BaseBranch != "" {
			return fmt.Sprintf("%s/%s:%s", r.Owner, r.Repo, r.BaseBranch)
		}
		return fmt.Sprintf("%s/%s", r.Owner, r.Repo)
	default:
		return fmt.Sprintf("%s/%s", r.Owner, r.Repo)
	}
}

// IsValidGitHubRef validates if a string is a valid GitHub reference format
func IsValidGitHubRef(ref string) bool {
	_, err := ParseRef(ref)
	return err == nil
}

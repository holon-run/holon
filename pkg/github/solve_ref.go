package github

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

// SolveRefType represents the type of GitHub reference for solving
type SolveRefType int

const (
	// SolveRefTypeIssue is an issue reference
	SolveRefTypeIssue SolveRefType = iota
	// SolveRefTypePR is a pull request reference
	SolveRefTypePR
)

// SolveRef represents a parsed GitHub reference for solving
type SolveRef struct {
	Owner  string
	Repo   string
	Number int
	Type   SolveRefType
}

var (
	// Full URL patterns
	issueURLPattern   = regexp.MustCompile(`^https://github\.com/([^/]+)/([^/]+)/issues/(\d+)$`)
	prURLPattern      = regexp.MustCompile(`^https://github\.com/([^/]+)/([^/]+)/pull/(\d+)$`)
	// Short patterns
	shortRefPattern   = regexp.MustCompile(`^([^/]+)/([^/]+)#(\d+)$`)
	// Numeric only (for #123 when repo is provided)
	numericPattern    = regexp.MustCompile(`^(\d+)$`)
)

// ParseSolveRef parses a GitHub reference string for the solve command.
// Supported formats:
//   - https://github.com/<owner>/<repo>/issues/<n> (issue URL)
//   - https://github.com/<owner>/<repo>/pull/<n> (PR URL)
//   - <owner>/<repo>#<n> (short form, type determined later via API)
//   - #<n> or <n> (numeric only, requires --repo flag)
//
// For ambiguous references (short forms), the Type field may be unknown
// and should be determined via GitHub API by checking if the number is a PR.
func ParseSolveRef(ref string, defaultRepo string) (*SolveRef, error) {
	ref = strings.TrimSpace(ref)

	// Try full issue URL
	if matches := issueURLPattern.FindStringSubmatch(ref); matches != nil {
		num, _ := strconv.Atoi(matches[3])
		return &SolveRef{
			Owner:  matches[1],
			Repo:   matches[2],
			Number: num,
			Type:   SolveRefTypeIssue,
		}, nil
	}

	// Try full PR URL
	if matches := prURLPattern.FindStringSubmatch(ref); matches != nil {
		num, _ := strconv.Atoi(matches[3])
		return &SolveRef{
			Owner:  matches[1],
			Repo:   matches[2],
			Number: num,
			Type:   SolveRefTypePR,
		}, nil
	}

	// Try short form: owner/repo#123
	if matches := shortRefPattern.FindStringSubmatch(ref); matches != nil {
		num, _ := strconv.Atoi(matches[3])
		// Type is ambiguous - will be determined via API
		// Default placeholder value; actual type determined via API in determineRefType
		return &SolveRef{
			Owner:  matches[1],
			Repo:   matches[2],
			Number: num,
			Type:   SolveRefTypePR, // Placeholder; type determined via API
		}, nil
	}

	// Try numeric only: #123 or 123 (requires defaultRepo)
	if defaultRepo == "" {
		return nil, fmt.Errorf("ambiguous reference '%s' requires --repo flag (e.g., --repo owner/repo)", ref)
	}

	refWithoutHash := strings.TrimPrefix(ref, "#")
	if matches := numericPattern.FindStringSubmatch(refWithoutHash); matches != nil {
		// Parse defaultRepo
		parts := strings.Split(defaultRepo, "/")
		if len(parts) != 2 {
			return nil, fmt.Errorf("invalid --repo format: %s (expected owner/repo)", defaultRepo)
		}
		num, _ := strconv.Atoi(matches[1])
		return &SolveRef{
			Owner:  parts[0],
			Repo:   parts[1],
			Number: num,
			Type:   SolveRefTypePR, // Placeholder; type determined via API
		}, nil
	}

	return nil, fmt.Errorf("invalid GitHub reference format: %s (supported: full URLs, owner/repo#123, or #123 with --repo)", ref)
}

// String returns the string representation of the solve reference
func (r *SolveRef) String() string {
	switch r.Type {
	case SolveRefTypeIssue:
		return fmt.Sprintf("%s/%s#%d (issue)", r.Owner, r.Repo, r.Number)
	case SolveRefTypePR:
		return fmt.Sprintf("%s/%s#%d (pr)", r.Owner, r.Repo, r.Number)
	default:
		return fmt.Sprintf("%s/%s#%d", r.Owner, r.Repo, r.Number)
	}
}

// URL returns the GitHub URL for this reference.
// Falls back to issues URL for unknown types (should not occur in normal usage).
func (r *SolveRef) URL() string {
	switch r.Type {
	case SolveRefTypeIssue:
		return fmt.Sprintf("https://github.com/%s/%s/issues/%d", r.Owner, r.Repo, r.Number)
	case SolveRefTypePR:
		return fmt.Sprintf("https://github.com/%s/%s/pull/%d", r.Owner, r.Repo, r.Number)
	default:
		// Fallback to issues URL for unknown types
		return fmt.Sprintf("https://github.com/%s/%s/issues/%d", r.Owner, r.Repo, r.Number)
	}
}

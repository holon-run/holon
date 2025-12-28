package githubpr

import (
	"fmt"
	"regexp"
	"strings"
	"time"
)

// PRRef formats:
// - "owner/repo" (base branch defaults to "main")
// - "owner/repo:base-branch"

var prRefRegex = regexp.MustCompile(`^([\w.-]+)/([\w.-]+)(?::([\w./-]+))?$`)

// ParsePRRef parses a PR reference string into a PRRef struct.
func ParsePRRef(target string) (*PRRef, error) {
	matches := prRefRegex.FindStringSubmatch(target)
	if matches == nil {
		return nil, fmt.Errorf("invalid target format: %s (expected owner/repo[:base_branch])", target)
	}

	ref := &PRRef{
		Owner:      matches[1],
		Repo:       matches[2],
		BaseBranch: "main", // Default to main
	}

	if matches[3] != "" {
		ref.BaseBranch = matches[3]
	}

	return ref, nil
}

// String returns the string representation of the PR reference.
func (r PRRef) String() string {
	if r.BaseBranch == "" || r.BaseBranch == "main" {
		return fmt.Sprintf("%s/%s", r.Owner, r.Repo)
	}
	return fmt.Sprintf("%s/%s:%s", r.Owner, r.Repo, r.BaseBranch)
}

// FullName returns the full repository name (owner/repo).
func (r PRRef) FullName() string {
	return fmt.Sprintf("%s/%s", r.Owner, r.Repo)
}

// CloneURL returns the HTTPS clone URL for the repository.
func (r PRRef) CloneURL() string {
	return fmt.Sprintf("https://github.com/%s/%s.git", r.Owner, r.Repo)
}

// GitCloneURLWithToken returns the HTTPS clone URL with an embedded token.
func (r PRRef) GitCloneURLWithToken(token string) string {
	return fmt.Sprintf("https://x-access-token:%s@github.com/%s/%s.git", token, r.Owner, r.Repo)
}

// ExtractBranchFromSummary extracts a branch name from summary.md content.
// Looks for patterns like "Branch: holon/fix-123" or returns a default.
func ExtractBranchFromSummary(summary string, issueID string) string {
	// Look for explicit branch marker
	lines := strings.Split(summary, "\n")
	for _, line := range lines {
		line = strings.TrimSpace(line)
		if strings.HasPrefix(line, "Branch:") || strings.HasPrefix(line, "branch:") {
			// Find the colon and extract everything after it
			if idx := strings.Index(line, ":"); idx != -1 {
				branch := strings.TrimSpace(line[idx+1:])
				if branch != "" {
					return branch
				}
			}
		}
	}

	// Generate default branch name with a timestamp suffix to avoid collisions
	suffix := time.Now().UTC().Format("20060102-150405")
	if issueID != "" {
		return fmt.Sprintf("holon/fix-%s-%s", issueID, suffix)
	}
	return fmt.Sprintf("holon/auto-fix-%s", suffix)
}

// ExtractTitleFromSummary extracts a PR title from summary.md content.
// Uses the first non-empty line (excluding markdown headers) as the title.
func ExtractTitleFromSummary(summary string) string {
	lines := strings.Split(summary, "\n")
	for _, line := range lines {
		line = strings.TrimSpace(line)
		// Skip empty lines and standalone headers
		if line == "" {
			continue
		}
		if strings.HasPrefix(line, "#") {
			// If it's a header, strip the # symbols
			title := strings.TrimSpace(strings.TrimLeft(line, "#"))
			if title != "" {
				return title
			}
			continue
		}
		// Found first non-empty, non-header line
		return line
	}
	return "Automated Holon Fix"
}

// FormatPRBody formats the PR body from summary content.
// Adds issue reference if provided.
func FormatPRBody(summary string, issueID string) string {
	body := summary

	// Add issue reference at the beginning if issue ID is provided
	if issueID != "" {
		body = fmt.Sprintf("Fixes #%s\n\n%s", issueID, body)
	}

	return body
}

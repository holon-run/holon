package githubpr

// PRRef represents a parsed GitHub repository reference.
// Supports formats: "owner/repo", "owner/repo:base_branch"
type PRRef struct {
	Owner       string
	Repo        string
	BaseBranch  string // Optional: defaults to "main"
}

// PRPublisherConfig contains configuration for PR publishing.
type PRPublisherConfig struct {
	// BranchName is the name of the branch to create/use for the PR
	BranchName string

	// Title is the PR title (if empty, derived from summary.md)
	Title string

	// CommitMessage is the commit message (if empty, derived from summary.md)
	CommitMessage string

	// IssueID is the optional issue number to reference in the PR
	IssueID string

	// AuthorName is the git author name for commits (if empty, uses "Holon Bot")
	AuthorName string

	// AuthorEmail is the git author email for commits (if empty, uses "bot@holon.run")
	AuthorEmail string

	// DryRun if true, validates without making changes
	DryRun bool
}

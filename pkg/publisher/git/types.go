package git

// GitPublisherConfig contains configuration for git publishing.
type GitPublisherConfig struct {
	// Branch is the branch name to create or checkout (optional)
	Branch string

	// Commit if true, commits changes after applying patch
	Commit bool

	// CommitMessage is the commit message (if empty, uses default)
	CommitMessage string

	// Push if true, pushes commits to remote
	Push bool

	// Remote is the remote name to push to (default: origin)
	Remote string

	// WorkspaceDir is the path to the git workspace
	WorkspaceDir string

	// AuthorName is the git author name for commits (if empty, uses "holonbot[bot]")
	AuthorName string

	// AuthorEmail is the git author email for commits (if empty, uses holonbot noreply address)
	AuthorEmail string
}

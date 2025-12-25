package githubpr

import (
	"testing"
)

func TestParsePRRef(t *testing.T) {
	tests := []struct {
		name     string
		target   string
		want     *PRRef
		wantErr  bool
	}{
		{
			name:   "simple owner/repo",
			target: "holon-run/holon",
			want: &PRRef{
				Owner:      "holon-run",
				Repo:       "holon",
				BaseBranch: "main",
			},
			wantErr: false,
		},
		{
			name:   "owner/repo with base branch",
			target: "holon-run/holon:develop",
			want: &PRRef{
				Owner:      "holon-run",
				Repo:       "holon",
				BaseBranch: "develop",
			},
			wantErr: false,
		},
		{
			name:    "invalid format - missing owner",
			target:  "holon",
			wantErr: true,
		},
		{
			name:    "invalid format - too many parts",
			target:  "a/b/c/d",
			wantErr: true,
		},
		{
			name:    "empty string",
			target:  "",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParsePRRef(tt.target)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParsePRRef() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if got == nil {
					t.Fatal("ParsePRRef() returned nil, expected non-nil")
				}
				if got.Owner != tt.want.Owner {
					t.Errorf("ParsePRRef() Owner = %v, want %v", got.Owner, tt.want.Owner)
				}
				if got.Repo != tt.want.Repo {
					t.Errorf("ParsePRRef() Repo = %v, want %v", got.Repo, tt.want.Repo)
				}
				if got.BaseBranch != tt.want.BaseBranch {
					t.Errorf("ParsePRRef() BaseBranch = %v, want %v", got.BaseBranch, tt.want.BaseBranch)
				}
			}
		})
	}
}

func TestPRRefString(t *testing.T) {
	tests := []struct {
		name string
		ref  PRRef
		want string
	}{
		{
			name: "default main branch",
			ref: PRRef{
				Owner:      "holon-run",
				Repo:       "holon",
				BaseBranch: "main",
			},
			want: "holon-run/holon",
		},
		{
			name: "custom base branch",
			ref: PRRef{
				Owner:      "holon-run",
				Repo:       "holon",
				BaseBranch: "develop",
			},
			want: "holon-run/holon:develop",
		},
		{
			name: "empty base branch (should default to main)",
			ref: PRRef{
				Owner:      "holon-run",
				Repo:       "holon",
				BaseBranch: "",
			},
			want: "holon-run/holon",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.ref.String(); got != tt.want {
				t.Errorf("PRRef.String() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestPRRefCloneURL(t *testing.T) {
	ref := PRRef{
		Owner:      "holon-run",
		Repo:       "holon",
		BaseBranch: "main",
	}

	want := "https://github.com/holon-run/holon.git"
	if got := ref.CloneURL(); got != want {
		t.Errorf("PRRef.CloneURL() = %v, want %v", got, want)
	}
}

func TestPRRefGitCloneURLWithToken(t *testing.T) {
	ref := PRRef{
		Owner:      "holon-run",
		Repo:       "holon",
		BaseBranch: "main",
	}

	want := "https://x-access-token:my-token@github.com/holon-run/holon.git"
	if got := ref.GitCloneURLWithToken("my-token"); got != want {
		t.Errorf("PRRef.GitCloneURLWithToken() = %v, want %v", got, want)
	}
}

func TestExtractTitleFromSummary(t *testing.T) {
	tests := []struct {
		name     string
		summary  string
		want     string
	}{
		{
			name:    "empty summary",
			summary: "",
			want:    "Automated Holon Fix",
		},
		{
			name:    "single line",
			summary: "Fix the bug in parser",
			want:    "Fix the bug in parser",
		},
		{
			name:    "with markdown header",
			summary: "# Fix Parser Bug\n\nFixed the parsing issue.",
			want:    "Fix Parser Bug",
		},
		{
			name:    "with multiple headers",
			summary: "## Summary\n\nFixed the bug.",
			want:    "Summary",
		},
		{
			name:    "multiline with first line as title",
			summary: "Add new feature\n\nDetails:\n- Feature 1\n- Feature 2",
			want:    "Add new feature",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := ExtractTitleFromSummary(tt.summary); got != tt.want {
				t.Errorf("ExtractTitleFromSummary() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestExtractBranchFromSummary(t *testing.T) {
	tests := []struct {
		name     string
		summary  string
		issueID  string
		want     string
	}{
		{
			name:    "no branch marker, with issue",
			summary: "Fix the bug\n\nDetails here.",
			issueID: "123",
			want:    "holon/fix-123",
		},
		{
			name:    "no branch marker, no issue",
			summary: "Fix the bug\n\nDetails here.",
			issueID: "",
			want:    "holon/auto-fix",
		},
		{
			name:    "with branch marker",
			summary: "Branch: custom/branch-123\n\nFix the bug.",
			issueID: "456",
			want:    "custom/branch-123",
		},
		{
			name:    "with lowercase branch marker",
			summary: "branch: feat/new-feature\n\nAdd feature.",
			issueID: "789",
			want:    "feat/new-feature",
		},
		{
			name:    "branch marker with spaces",
			summary: "Branch:  test/branch  \n\nFix.",
			issueID: "100",
			want:    "test/branch",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := ExtractBranchFromSummary(tt.summary, tt.issueID); got != tt.want {
				t.Errorf("ExtractBranchFromSummary() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestFormatPRBody(t *testing.T) {
	tests := []struct {
		name    string
		summary string
		issueID string
		want    string
	}{
		{
			name:    "without issue",
			summary: "Fixed the bug.\n\nChanged parser to handle edge case.",
			issueID: "",
			want:    "Fixed the bug.\n\nChanged parser to handle edge case.",
		},
		{
			name:    "with issue",
			summary: "Fixed the bug.\n\nChanged parser to handle edge case.",
			issueID: "123",
			want:    "Fixes #123\n\nFixed the bug.\n\nChanged parser to handle edge case.",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := FormatPRBody(tt.summary, tt.issueID); got != tt.want {
				t.Errorf("FormatPRBody() = %v, want %v", got, tt.want)
			}
		})
	}
}

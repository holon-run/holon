package github

import (
	"testing"
)

func TestParseSolveRef(t *testing.T) {
	tests := []struct {
		name        string
		ref         string
		defaultRepo string
		want        *SolveRef
		wantErr     bool
	}{
		{
			name: "full issue URL",
			ref:  "https://github.com/owner/repo/issues/123",
			want: &SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 123,
				Type:   SolveRefTypeIssue,
			},
			wantErr: false,
		},
		{
			name: "full PR URL",
			ref:  "https://github.com/owner/repo/pull/456",
			want: &SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 456,
				Type:   SolveRefTypePR,
			},
			wantErr: false,
		},
		{
			name: "short form owner/repo#123",
			ref:  "owner/repo#789",
			want: &SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 789,
				Type:   SolveRefTypePR, // Default to PR
			},
			wantErr: false,
		},
		{
			name:        "numeric 123 with default repo",
			ref:         "123",
			defaultRepo: "owner/repo",
			want: &SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 123,
				Type:   SolveRefTypePR,
			},
			wantErr: false,
		},
		{
			name:        "#123 with default repo",
			ref:         "#123",
			defaultRepo: "owner/repo",
			want: &SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 123,
				Type:   SolveRefTypePR,
			},
			wantErr: false,
		},
		{
			name:        "numeric without default repo returns error",
			ref:         "123",
			defaultRepo: "",
			wantErr:     true,
		},
		{
			name:    "invalid format",
			ref:     "invalid-format",
			wantErr: true,
		},
		{
			name:    "invalid URL",
			ref:     "https://example.com/not-github",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseSolveRef(tt.ref, tt.defaultRepo)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParseSolveRef() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if got.Owner != tt.want.Owner {
					t.Errorf("ParseSolveRef() Owner = %v, want %v", got.Owner, tt.want.Owner)
				}
				if got.Repo != tt.want.Repo {
					t.Errorf("ParseSolveRef() Repo = %v, want %v", got.Repo, tt.want.Repo)
				}
				if got.Number != tt.want.Number {
					t.Errorf("ParseSolveRef() Number = %v, want %v", got.Number, tt.want.Number)
				}
				if got.Type != tt.want.Type {
					t.Errorf("ParseSolveRef() Type = %v, want %v", got.Type, tt.want.Type)
				}
			}
		})
	}
}

func TestSolveRefString(t *testing.T) {
	tests := []struct {
		name string
		ref  SolveRef
		want string
	}{
		{
			name: "issue reference",
			ref: SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 123,
				Type:   SolveRefTypeIssue,
			},
			want: "owner/repo#123 (issue)",
		},
		{
			name: "PR reference",
			ref: SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 456,
				Type:   SolveRefTypePR,
			},
			want: "owner/repo#456 (pr)",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.ref.String(); got != tt.want {
				t.Errorf("SolveRef.String() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestSolveRefURL(t *testing.T) {
	tests := []struct {
		name string
		ref  SolveRef
		want string
	}{
		{
			name: "issue URL",
			ref: SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 123,
				Type:   SolveRefTypeIssue,
			},
			want: "https://github.com/owner/repo/issues/123",
		},
		{
			name: "PR URL",
			ref: SolveRef{
				Owner:  "owner",
				Repo:   "repo",
				Number: 456,
				Type:   SolveRefTypePR,
			},
			want: "https://github.com/owner/repo/pull/456",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.ref.URL(); got != tt.want {
				t.Errorf("SolveRef.URL() = %v, want %v", got, tt.want)
			}
		})
	}
}

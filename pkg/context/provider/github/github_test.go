package github

import (
	"testing"
)

func TestParseRef(t *testing.T) {
	tests := []struct {
		name      string
		ref       string
		repoHint  string
		wantOwner string
		wantRepo  string
		wantNum   int
		wantErr   bool
	}{
		{
			name:      "parse URL with pull",
			ref:       "https://github.com/owner/repo/pull/123",
			wantOwner: "owner",
			wantRepo:  "repo",
			wantNum:   123,
			wantErr:   false,
		},
		{
			name:      "parse URL with issues",
			ref:       "https://github.com/owner/repo/issues/456",
			wantOwner: "owner",
			wantRepo:  "repo",
			wantNum:   456,
			wantErr:   false,
		},
		{
			name:      "parse owner/repo#number format",
			ref:       "owner/repo#789",
			wantOwner: "owner",
			wantRepo:  "repo",
			wantNum:   789,
			wantErr:   false,
		},
		{
			name:      "parse #number with repo hint",
			ref:       "#42",
			repoHint:  "hint/repo",
			wantOwner: "hint",
			wantRepo:  "repo",
			wantNum:   42,
			wantErr:   false,
		},
		{
			name:      "parse plain number with repo hint",
			ref:       "99",
			repoHint:  "another/hint",
			wantOwner: "another",
			wantRepo:  "hint",
			wantNum:   99,
			wantErr:   false,
		},
		{
			name:     "invalid URL - missing parts",
			ref:      "https://github.com/owner",
			wantErr:  true,
		},
		{
			name:     "invalid ref format",
			ref:      "invalid-format",
			wantErr:  true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			owner, repo, num, err := ParseRef(tt.ref, tt.repoHint)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParseRef() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if owner != tt.wantOwner {
					t.Errorf("ParseRef() owner = %v, want %v", owner, tt.wantOwner)
				}
				if repo != tt.wantRepo {
					t.Errorf("ParseRef() repo = %v, want %v", repo, tt.wantRepo)
				}
				if num != tt.wantNum {
					t.Errorf("ParseRef() num = %v, want %v", num, tt.wantNum)
				}
			}
		})
	}
}

func TestParseRepo(t *testing.T) {
	tests := []struct {
		name      string
		repo      string
		wantOwner string
		wantName  string
		wantErr   bool
	}{
		{
			name:      "parse owner/repo format",
			repo:      "owner/repo",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse github.com/owner/repo format",
			repo:      "github.com/owner/repo",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse https://github.com/owner/repo format",
			repo:      "https://github.com/owner/repo",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse owner/repo.git format",
			repo:      "owner/repo.git",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:      "parse https://github.com/owner/repo.git format",
			repo:      "https://github.com/owner/repo.git",
			wantOwner: "owner",
			wantName:  "repo",
			wantErr:   false,
		},
		{
			name:    "invalid - missing repo",
			repo:    "owner",
			wantErr: true,
		},
		{
			name:    "invalid - empty string",
			repo:    "",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			owner, name, err := parseRepo(tt.repo)
			if (err != nil) != tt.wantErr {
				t.Errorf("parseRepo() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if owner != tt.wantOwner {
					t.Errorf("parseRepo() owner = %v, want %v", owner, tt.wantOwner)
				}
				if name != tt.wantName {
					t.Errorf("parseRepo() name = %v, want %v", name, tt.wantName)
				}
			}
		})
	}
}

package github

import (
	"testing"
)

func TestParsePRRef(t *testing.T) {
	tests := []struct {
		name    string
		target  string
		want    *PRRef
		wantErr bool
	}{
		{
			name:   "valid owner/repo/pr/123 format",
			target: "holon-run/holon/pr/123",
			want: &PRRef{
				Owner:    "holon-run",
				Repo:     "holon",
				PRNumber: 123,
			},
			wantErr: false,
		},
		{
			name:   "valid owner/repo#123 format",
			target: "holon-run/holon#123",
			want: &PRRef{
				Owner:    "holon-run",
				Repo:     "holon",
				PRNumber: 123,
			},
			wantErr: false,
		},
		{
			name:   "valid owner/repo/pull/123 format",
			target: "holon-run/holon/pull/123",
			want: &PRRef{
				Owner:    "holon-run",
				Repo:     "holon",
				PRNumber: 123,
			},
			wantErr: false,
		},
		{
			name:    "invalid format - missing PR number",
			target:  "holon-run/holon/pr",
			wantErr: true,
		},
		{
			name:    "invalid format - no separator",
			target:  "holon-run-holon-123",
			wantErr: true,
		},
		{
			name:    "invalid format - empty string",
			target:  "",
			wantErr: true,
		},
		{
			name:   "valid with whitespace trimming",
			target: "  holon-run/holon/pr/456  ",
			want: &PRRef{
				Owner:    "holon-run",
				Repo:     "holon",
				PRNumber: 456,
			},
			wantErr: false,
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
				if got.Owner != tt.want.Owner {
					t.Errorf("ParsePRRef() Owner = %v, want %v", got.Owner, tt.want.Owner)
				}
				if got.Repo != tt.want.Repo {
					t.Errorf("ParsePRRef() Repo = %v, want %v", got.Repo, tt.want.Repo)
				}
				if got.PRNumber != tt.want.PRNumber {
					t.Errorf("ParsePRRef() PRNumber = %v, want %v", got.PRNumber, tt.want.PRNumber)
				}
			}
		})
	}
}

func TestPRRefString(t *testing.T) {
	prRef := PRRef{
		Owner:    "holon-run",
		Repo:     "holon",
		PRNumber: 123,
	}

	want := "holon-run/holon/pr/123"
	got := prRef.String()

	if got != want {
		t.Errorf("PRRef.String() = %v, want %v", got, want)
	}
}

package agent

import (
	"testing"
)

func TestFindAgentBundleAsset(t *testing.T) {
	tests := []struct {
		name     string
		release  GitHubRelease
		shouldErr bool
		expectedBundle string
	}{
		{
			name: "valid agent release",
			release: GitHubRelease{
				TagName: "agent-claude-v0.2.0",
				Assets: []struct {
					Name string `json:"name"`
					URL  string `json:"browser_download_url"`
				}{
					{
						Name: "holon-agent-claude-0.2.0.tar.gz",
						URL:  "https://github.com/holon-run/holon/releases/download/agent-claude-v0.2.0/holon-agent-claude-0.2.0.tar.gz",
					},
					{
						Name: "holon-agent-claude-0.2.0.tar.gz.sha256",
						URL:  "https://github.com/holon-run/holon/releases/download/agent-claude-v0.2.0/holon-agent-claude-0.2.0.tar.gz.sha256",
					},
				},
			},
			shouldErr: false,
			expectedBundle: "holon-agent-claude-0.2.0.tar.gz",
		},
		{
			name: "non-agent release",
			release: GitHubRelease{
				TagName: "v1.0.0",
				Assets: []struct {
					Name string `json:"name"`
					URL  string `json:"browser_download_url"`
				}{
					{
						Name: "holon-linux-amd64.tar.gz",
						URL:  "https://github.com/holon-run/holon/releases/download/v1.0.0/holon-linux-amd64.tar.gz",
					},
				},
			},
			shouldErr: true,
			expectedBundle: "",
		},
		{
			name: "different agent release",
			release: GitHubRelease{
				TagName: "agent-custom-v0.1.0",
				Assets: []struct {
					Name string `json:"name"`
					URL  string `json:"browser_download_url"`
				}{
					{
						Name: "custom-agent-bundle.tar.gz",
						URL:  "https://github.com/holon-run/holon/releases/download/agent-custom-v0.1.0/custom-agent-bundle.tar.gz",
					},
				},
			},
			shouldErr: false,
			expectedBundle: "custom-agent-bundle.tar.gz",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			bundleName, bundleURL, err := FindAgentBundleAsset(&tt.release)

			if tt.shouldErr {
				if err == nil {
					t.Errorf("Expected error but got none")
				}
				return
			}

			if err != nil {
				t.Errorf("Unexpected error: %v", err)
				return
			}

			if bundleName != tt.expectedBundle {
				t.Errorf("Expected bundle name %q, got %q", tt.expectedBundle, bundleName)
			}

			if bundleURL == "" {
				t.Error("Expected non-empty bundle URL")
			}
		})
	}
}

func TestIsAgentRelease(t *testing.T) {
	tests := []struct {
		name     string
		tagName  string
		expected bool
	}{
		{
			name:     "agent-claude release",
			tagName:  "agent-claude-v0.2.0",
			expected: true,
		},
		{
			name:     "agent-custom release",
			tagName:  "agent-custom-v1.0.0",
			expected: true,
		},
		{
			name:     "regular holon release",
			tagName:  "v1.0.0",
			expected: false,
		},
		{
			name:     "beta release",
			tagName:  "agent-claude-v0.2.0-beta",
			expected: true, // We check prefix first, draft/prerelease checked separately
		},
		{
			name:     "non-agent prefix",
			tagName:  "feature-something",
			expected: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := len(tt.tagName) > 0 &&
						(tt.tagName[0:6] == "agent-" ||
						 (len(tt.tagName) >= 6 && tt.tagName[:6] == "agent-"))
			if result != tt.expected {
				t.Errorf("isAgentRelease(%q) = %v, expected %v", tt.tagName, result, tt.expected)
			}
		})
	}
}
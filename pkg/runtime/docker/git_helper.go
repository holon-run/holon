package docker

import (
	"os/exec"
	"strings"
)

// Deprecated: Use git.ResolveConfig() instead for centralized git config resolution.
// This function is kept for backward compatibility but should not be used in new code.
// The centralized resolver provides proper priority handling: host git > ProjectConfig > defaults.
func getGitConfig(key string) (string, error) {
	cmd := exec.Command("git", "config", "--get", key)
	out, err := cmd.Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}

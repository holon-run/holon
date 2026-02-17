package tools

import (
	"strings"
	"testing"
)

func TestRequiredCommandsListCopy(t *testing.T) {
	orig := RequiredCommandsList()
	if len(orig) == 0 {
		t.Fatal("required commands should not be empty")
	}
	orig[0] = "mutated"
	again := RequiredCommandsList()
	if again[0] == "mutated" {
		t.Fatal("RequiredCommandsList should return a copy")
	}
}

func TestBuildInstallScriptContainsVerification(t *testing.T) {
	script := BuildInstallScript()
	if !strings.Contains(script, "verify_required") {
		t.Fatal("script should verify required tools")
	}
	if !strings.Contains(script, "Missing required runtime tools:") {
		t.Fatal("script should fail-fast with missing tools message")
	}
	for _, cmd := range RequiredCommands {
		needle := "command -v " + cmd
		if !strings.Contains(script, needle) {
			t.Fatalf("script missing command check for %s", cmd)
		}
	}
}

func TestBuildInstallScriptIncludesPackageManagers(t *testing.T) {
	script := BuildInstallScript()
	for _, manager := range []string{"apt-get", "dnf", "yum"} {
		if !strings.Contains(script, manager) {
			t.Fatalf("script should include %s path", manager)
		}
	}
}

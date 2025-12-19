package integration

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/rogpeppe/go-internal/testscript"
)

func TestMain(m *testing.M) {
	// 1. Compile the binary
	_, filename, _, _ := runtime.Caller(0)
	projectRoot := filepath.Join(filepath.Dir(filename), "../..")
	binDir := filepath.Join(projectRoot, "bin")
	if err := os.MkdirAll(binDir, 0755); err != nil {
		fmt.Fprintf(os.Stderr, "failed to create bin dir: %v\n", err)
		os.Exit(1)
	}

	binPath := filepath.Join(binDir, "holon")
	if runtime.GOOS == "windows" {
		binPath += ".exe"
	}

	// Build the binary
	cmd := exec.Command("go", "build", "-o", binPath, "./cmd/holon")
	cmd.Dir = projectRoot
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "failed to build holon: %v\n", err)
		os.Exit(1)
	}

	// 2. Run the tests
	os.Exit(testscript.RunMain(m, map[string]func() int{
		"holon": func() int {
			// This is not used because we use the binary directly in PATH
			// However, testscript requires a main function map if we were testing internal commands.
			// Since we want to test the built binary, we just need to ensure it's in the PATH.
			return 0
		},
	}))
}

func TestScript(t *testing.T) {
	_, filename, _, _ := runtime.Caller(0)
	projectRoot := filepath.Join(filepath.Dir(filename), "../..")
	binDir := filepath.Join(projectRoot, "bin")

	testscript.Run(t, testscript.Params{
		Dir: "testdata",
		Setup: func(env *testscript.Env) error {
			// Add bin dir to PATH
			env.Vars = append(env.Vars, fmt.Sprintf("PATH=%s%c%s", binDir, filepath.ListSeparator, os.Getenv("PATH")))
			return nil
		},
	})
}

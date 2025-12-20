package integration

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/rogpeppe/go-internal/testscript"
)

var (
	repoRoot string
	holonBin string
)

func TestMain(m *testing.M) {
	var err error
	repoRoot, err = findRepoRoot()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}

	binDir, err := os.MkdirTemp("", "holon-bin-*")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}

	holonBin = filepath.Join(binDir, "holon")
	if runtime.GOOS == "windows" {
		holonBin += ".exe"
	}

	cmd := exec.Command("go", "build", "-o", holonBin, "./cmd/holon")
	cmd.Dir = repoRoot
	cmd.Env = append(os.Environ(), "CGO_ENABLED=0")
	if out, err := cmd.CombinedOutput(); err != nil {
		fmt.Fprintf(os.Stderr, "failed to build holon: %v\n%s\n", err, string(out))
		_ = os.RemoveAll(binDir)
		os.Exit(2)
	}

	exitCode := m.Run()
	_ = os.RemoveAll(binDir)
	os.Exit(exitCode)
}

func TestIntegration(t *testing.T) {
	testscript.Run(t, testscript.Params{
		Dir: filepath.Join(repoRoot, "tests", "integration", "testdata"),
		Setup: func(env *testscript.Env) error {
			home := filepath.Join(env.WorkDir, "home")
			tmp := filepath.Join(env.WorkDir, "tmp")
			if err := os.MkdirAll(home, 0o755); err != nil {
				return err
			}
			if err := os.MkdirAll(tmp, 0o755); err != nil {
				return err
			}

			env.Setenv("HOME", home)
			env.Setenv("TMPDIR", tmp)
			env.Setenv("TEMP", tmp)
			env.Setenv("TMP", tmp)

			pathVar := os.Getenv("PATH")
			env.Setenv("PATH", filepath.Dir(holonBin)+string(os.PathListSeparator)+pathVar)
			env.Setenv("HOLON_BIN", holonBin)
			return nil
		},
		Condition: func(cond string) (bool, error) {
			switch cond {
			case "docker":
				return dockerAvailable(), nil
			default:
				return false, fmt.Errorf("unknown condition: %q", cond)
			}
		},
	})
}

func dockerAvailable() bool {
	cmd := exec.Command("docker", "info")
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stdout
	if err := cmd.Run(); err != nil {
		return false
	}
	return true
}

func findRepoRoot() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}
	for {
		if _, err := os.Stat(filepath.Join(dir, "go.mod")); err == nil {
			return dir, nil
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
	return "", fmt.Errorf("unable to locate repo root (go.mod not found)")
}

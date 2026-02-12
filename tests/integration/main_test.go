package integration

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"testing"

	"github.com/rogpeppe/go-internal/testscript"
)

var (
	repoRoot        string
	holonBin        string
	testAgentBundle string
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

	testAgentBundle = resolveTestAgentBundle(repoRoot)

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

			// Set HOLON_REPO_ROOT to point to the repository root
			// This allows tests to access repo-built artifacts like agent bundles
			env.Setenv("HOLON_REPO_ROOT", repoRoot)
			if testAgentBundle != "" {
				env.Setenv("HOLON_TEST_AGENT_BUNDLE", testAgentBundle)
			}

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

func resolveTestAgentBundle(repoRoot string) string {
	agentDir := filepath.Join(repoRoot, "agents", "claude")
	cmd := exec.Command("npm", "run", "bundle")
	cmd.Dir = agentDir
	if out, err := cmd.CombinedOutput(); err != nil {
		fmt.Fprintf(os.Stderr, "warning: failed to build test agent bundle: %v\n%s\n", err, strings.TrimSpace(string(out)))
		return ""
	}

	bundleDir := filepath.Join(agentDir, "dist", "agent-bundles")
	entries, err := os.ReadDir(bundleDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "warning: failed to read test agent bundle dir: %v\n", err)
		return ""
	}
	var bundles []string
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".tar.gz") {
			continue
		}
		bundles = append(bundles, filepath.Join(bundleDir, entry.Name()))
	}
	if len(bundles) == 0 {
		fmt.Fprintln(os.Stderr, "warning: no test agent bundle found after npm run bundle")
		return ""
	}
	sort.Strings(bundles)
	return bundles[len(bundles)-1]
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

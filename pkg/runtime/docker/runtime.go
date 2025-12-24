package docker

import (
	"archive/tar"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"

	"github.com/docker/docker/api/types/container"
	"github.com/docker/docker/api/types/image"
	"github.com/docker/docker/client"
)

type Runtime struct {
	cli *client.Client
}

func NewRuntime() (*Runtime, error) {
	cli, err := client.NewClientWithOpts(client.FromEnv, client.WithAPIVersionNegotiation())
	if err != nil {
		return nil, err
	}
	return &Runtime{cli: cli}, nil
}

type ContainerConfig struct {
	BaseImage      string // e.g., golang:1.22 (The toolchain)
	AgentBundle    string // Required path to agent bundle archive (.tar.gz)
	Workspace      string
	SpecPath       string
	ContextPath    string // Optional: path to context files
	OutDir         string
	Env            map[string]string
	PromptPath     string   // Path to compiled system.md
	UserPromptPath string   // Path to compiled user.md
	Cmd            []string // Optional command override
}

func (r *Runtime) RunHolon(ctx context.Context, cfg *ContainerConfig) error {
	// 1. Snapshot Workspace (Isolation)
	snapshotDir, err := mkdirTempOutsideWorkspace(cfg.Workspace, "holon-workspace-*")
	if err != nil {
		return fmt.Errorf("failed to create snapshot dir: %w", err)
	}

	// Use git shared clone if workspace is a git repository, otherwise fall back to copy
	useSharedClone := isGitRepo(cfg.Workspace)
	if useSharedClone {
		fmt.Printf("Creating git shared clone at %s...\n", snapshotDir)
		if err := createSharedClone(cfg.Workspace, snapshotDir); err != nil {
			// If shared clone creation fails, log a warning and fall back to copy
			fmt.Printf("Warning: failed to create shared clone: %v. Falling back to copy...\n", err)
			useSharedClone = false
			if err := copyDir(cfg.Workspace, snapshotDir); err != nil {
				os.RemoveAll(snapshotDir)
				return fmt.Errorf("failed to snapshot workspace: %w", err)
			}
		}
	} else {
		fmt.Printf("Snapshotting workspace to %s...\n", snapshotDir)
		if err := copyDir(cfg.Workspace, snapshotDir); err != nil {
			os.RemoveAll(snapshotDir)
			return fmt.Errorf("failed to snapshot workspace: %w", err)
		}
	}

	// Set up cleanup function based on snapshot method
	cleanupSnapshot := func() error {
		// Both shared clone and regular snapshot can be cleaned up with simple removal
		return os.RemoveAll(snapshotDir)
	}
	defer func() {
		if err := cleanupSnapshot(); err != nil {
			fmt.Printf("Warning: failed to cleanup snapshot at %s: %v\n", snapshotDir, err)
		}
	}()

	// 2. Prepare Image (Build-on-Run composition)
	if cfg.AgentBundle == "" {
		return fmt.Errorf("agent bundle is required")
	}
	if cfg.BaseImage == "" {
		return fmt.Errorf("base image is required")
	}

	fmt.Printf("Composing execution image for %s + agent bundle %s...\n", cfg.BaseImage, cfg.AgentBundle)
	composedImage, err := r.buildComposedImageFromBundle(ctx, cfg.BaseImage, cfg.AgentBundle)
	if err != nil {
		return fmt.Errorf("failed to compose image: %w", err)
	}
	finalImage := composedImage

	// Pull final image if not present locally
	_, err = r.cli.ImageInspect(ctx, finalImage)
	if err != nil {
		fmt.Printf("Image %s not found locally, attempting to pull...\n", finalImage)
		reader, err := r.cli.ImagePull(ctx, finalImage, image.PullOptions{})
		if err != nil {
			fmt.Printf("Warning: failed to pull image %s: %v\n", finalImage, err)
		} else {
			defer reader.Close()
			io.Copy(io.Discard, reader)
		}
	} else {
		fmt.Printf("Image %s found locally.\n", finalImage)
	}

	// 3. Create Container
	// Inject host git identity
	gitName, err := getGitConfig("user.name")
	if err != nil {
		fmt.Printf("Warning: failed to get host git config 'user.name': %v\n", err)
	}
	gitEmail, err := getGitConfig("user.email")
	if err != nil {
		fmt.Printf("Warning: failed to get host git config 'user.email': %v\n", err)
	}

	if gitName != "" || gitEmail != "" {
		if cfg.Env == nil {
			cfg.Env = make(map[string]string)
		}
	}

	if gitName != "" {
		cfg.Env["GIT_AUTHOR_NAME"] = gitName
		cfg.Env["GIT_COMMITTER_NAME"] = gitName
	}
	if gitEmail != "" {
		cfg.Env["GIT_AUTHOR_EMAIL"] = gitEmail
		cfg.Env["GIT_COMMITTER_EMAIL"] = gitEmail
	}

	env := BuildContainerEnv(&EnvConfig{
		UserEnv: cfg.Env,
		HostUID: os.Getuid(),
		HostGID: os.Getgid(),
	})

	mountConfig := &MountConfig{
		SnapshotDir:    snapshotDir,
		SpecPath:       cfg.SpecPath,
		ContextPath:    cfg.ContextPath,
		OutDir:         cfg.OutDir,
		PromptPath:     cfg.PromptPath,
		UserPromptPath: cfg.UserPromptPath,
	}
	mounts := BuildContainerMounts(mountConfig)

	fmt.Printf("Creating container from image %s...\n", finalImage)
	resp, err := r.cli.ContainerCreate(ctx, &container.Config{
		Image:      finalImage,
		Cmd:        cfg.Cmd,
		Env:        env,
		WorkingDir: "/holon/workspace",
		Tty:        false,
	}, &container.HostConfig{
		Mounts:     mounts,
		AutoRemove: true,
	}, nil, nil, "")
	if err != nil {
		return fmt.Errorf("failed to create container: %w", err)
	}

	// 4. Start Container
	fmt.Printf("Starting container %s...\n", resp.ID[:12])
	if err := r.cli.ContainerStart(ctx, resp.ID, container.StartOptions{}); err != nil {
		return fmt.Errorf("failed to start container: %w", err)
	}

	// 4.5 Stream Logs
	fmt.Println("Streaming container logs...")
	out, err := r.cli.ContainerLogs(ctx, resp.ID, container.LogsOptions{
		ShowStdout: true,
		ShowStderr: true,
		Follow:     true,
	})
	if err == nil {
		defer out.Close()
		go io.Copy(os.Stdout, out)
	}

	// 5. Wait for completion
	fmt.Println("Waiting for container completion...")
	statusCh, errCh := r.cli.ContainerWait(ctx, resp.ID, container.WaitConditionNotRunning)
	select {
	case err := <-errCh:
		if err != nil {
			return fmt.Errorf("container wait error: %w", err)
		}
	case status := <-statusCh:
		if status.StatusCode != 0 {
			return fmt.Errorf("container failed with exit code %d", status.StatusCode)
		}
	}

	// 6. Artifact Validation (RFC-0002)
	// Read the spec to verify required artifacts, plus manifest.json
	// For now, validate basic manifest.json requirement
	if err := ValidateRequiredArtifacts(cfg.OutDir, nil); err != nil {
		return err
	}

	return nil
}

func (r *Runtime) buildComposedImageFromBundle(ctx context.Context, baseImage, bundlePath string) (string, error) {
	tmpDir, err := os.MkdirTemp("", "holon-build-*")
	if err != nil {
		return "", err
	}
	defer os.RemoveAll(tmpDir)

	bundleDigest, err := hashFile(bundlePath)
	if err != nil {
		return "", fmt.Errorf("failed to hash agent bundle: %w", err)
	}

	runtimeVersion, err := readBundleRuntimeVersion(bundlePath)
	if err != nil {
		return "", fmt.Errorf("failed to read bundle manifest: %w", err)
	}
	nodeMajor := nodeMajorVersion(runtimeVersion)

	bundleName := "agent-bundle.tar.gz"
	bundleDest := filepath.Join(tmpDir, bundleName)
	if err := copyFile(bundlePath, bundleDest); err != nil {
		return "", fmt.Errorf("failed to stage agent bundle: %w", err)
	}

	claudeCodeVersion := os.Getenv("HOLON_CLAUDE_CODE_VERSION")
	if claudeCodeVersion == "" {
		claudeCodeVersion = "2.0.74"
	}

	dockerfile := fmt.Sprintf(`
FROM %s
ARG NODE_MAJOR=%s
ARG CLAUDE_CODE_VERSION=%s
SHELL ["/bin/sh", "-c"]

RUN set -e; \
    if command -v apt-get >/dev/null 2>&1; then \
        apt-get update; \
        apt-get install -y --no-install-recommends curl ca-certificates git gnupg; \
        curl -fsSL https://deb.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        apt-get install -y --no-install-recommends nodejs; \
        rm -rf /var/lib/apt/lists/*; \
        if ! command -v gh >/dev/null 2>&1; then \
            curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg; \
            chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg; \
            echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | tee /etc/apt/sources.list.d/github-cli.list >/dev/null; \
            apt-get update; \
            apt-get install -y --no-install-recommends gh || true; \
            rm -rf /var/lib/apt/lists/*; \
        fi; \
    elif command -v dnf >/dev/null 2>&1; then \
        dnf install -y curl ca-certificates git; \
        curl -fsSL https://rpm.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        dnf install -y nodejs; \
    elif command -v yum >/dev/null 2>&1; then \
        yum install -y curl ca-certificates git; \
        curl -fsSL https://rpm.nodesource.com/setup_${NODE_MAJOR}.x | bash -; \
        yum install -y nodejs; \
    else \
        echo "Unsupported base image: no apt-get, dnf, or yum detected." >&2; \
        exit 1; \
    fi

RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}

COPY %s /holon/agent-bundle.tar.gz
RUN mkdir -p /holon/agent && tar -xzf /holon/agent-bundle.tar.gz -C /holon/agent

ENV IS_SANDBOX=1
WORKDIR /holon/workspace
ENTRYPOINT ["/holon/agent/bin/agent"]
`, baseImage, nodeMajor, claudeCodeVersion, bundleName)

	dfPath := filepath.Join(tmpDir, "Dockerfile")
	if err := os.WriteFile(dfPath, []byte(dockerfile), 0644); err != nil {
		return "", err
	}

	tag := composeImageTag(baseImage, bundleDigest)
	cmd := exec.Command("docker", "build", "-t", tag, tmpDir)
	if out, err := cmd.CombinedOutput(); err != nil {
		return "", fmt.Errorf("composition build failed: %v, output: %s", err, string(out))
	}

	return tag, nil
}

func composeImageTag(baseImage, bundleDigest string) string {
	hashInput := baseImage + ":" + bundleDigest
	hash := sha256.Sum256([]byte(hashInput))
	return fmt.Sprintf("holon-composed-%x", hash[:12])
}

type bundleManifest struct {
	Runtime struct {
		Type    string `json:"type"`
		Version string `json:"version"`
	} `json:"runtime"`
}

func readBundleRuntimeVersion(bundlePath string) (string, error) {
	file, err := os.Open(bundlePath)
	if err != nil {
		return "", err
	}
	defer file.Close()

	gz, err := gzip.NewReader(file)
	if err != nil {
		return "", err
	}
	defer gz.Close()

	tr := tar.NewReader(gz)
	for {
		header, err := tr.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return "", err
		}
		name := strings.TrimPrefix(header.Name, "./")
		if name != "manifest.json" {
			continue
		}
		payload, err := io.ReadAll(tr)
		if err != nil {
			return "", err
		}
		var manifest bundleManifest
		if err := json.Unmarshal(payload, &manifest); err != nil {
			return "", err
		}
		return manifest.Runtime.Version, nil
	}

	return "", fmt.Errorf("manifest.json not found in bundle")
}

func nodeMajorVersion(version string) string {
	if version == "" || version == "unknown" {
		return "20"
	}
	trimmed := strings.TrimPrefix(version, "v")
	parts := strings.Split(trimmed, ".")
	if len(parts) == 0 {
		return "20"
	}
	if _, err := strconv.Atoi(parts[0]); err != nil {
		return "20"
	}
	return parts[0]
}

func hashFile(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return "", err
	}
	return fmt.Sprintf("%x", hash.Sum(nil)), nil
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	defer out.Close()
	if _, err := io.Copy(out, in); err != nil {
		return err
	}
	return out.Sync()
}

// isGitRepo checks if the given directory is inside a git repository
func isGitRepo(dir string) bool {
	cmd := exec.Command("git", "-C", dir, "rev-parse", "--git-dir")
	if err := cmd.Run(); err != nil {
		return false
	}
	return true
}

// createSharedClone creates a git clone with shared object database
// This is preferred over worktree because it creates a complete .git directory
// that works correctly inside containers, while sharing objects to save space.
func createSharedClone(sourceRepo, clonePath string) error {
	fmt.Printf("  Creating shared clone of %s...\n", sourceRepo)

	cmd := exec.Command("git", "clone", "--shared", "--quiet", sourceRepo, clonePath)
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("failed to create shared clone: %v, output: %s", err, string(out))
	}

	// Fix alternates path to use relative path instead of absolute
	// This ensures the shared clone works correctly when mounted in a container
	alternatesFile := filepath.Join(clonePath, ".git", "objects", "info", "alternates")
	if content, err := os.ReadFile(alternatesFile); err == nil {
		alternatesPath := strings.TrimSpace(string(content))
		// Convert absolute path to relative path
		// Path is interpreted relative to .git/objects (not .git/objects/info where the file is)
		if filepath.IsAbs(alternatesPath) {
			// Resolve both paths through symlinks (important for macOS where /tmp -> /private/tmp)
			resolvedAlternatesPath, err := cleanAbs(alternatesPath)
			if err != nil {
				fmt.Printf("  Warning: failed to resolve alternates path: %v\n", err)
			} else {
				// Resolve clonePath to ensure both paths are in the same "world"
				resolvedClonePath, err := cleanAbs(clonePath)
				if err != nil {
					fmt.Printf("  Warning: failed to resolve clone path: %v\n", err)
				} else {
					// Calculate relative path from .git/objects
					objectsDir := filepath.Join(resolvedClonePath, ".git", "objects")
					relPath, err := filepath.Rel(objectsDir, resolvedAlternatesPath)
					if err == nil {
						// Only use relative path if it doesn't go to filesystem root
						// If the relative path starts with ".." and would reach root, use absolute path
						useRelative := true
						if strings.HasPrefix(relPath, "..") {
							// Count ".." components in relPath
							dotDotCount := strings.Count(relPath, ".."+string(filepath.Separator))
							if strings.HasSuffix(relPath, "..") {
								dotDotCount++
							}
							// Calculate path depth: number of non-empty components
							// For "/tmp/xxx/.git/objects", components are [tmp, xxx, .git, objects] = 4
							// Need 4 ".."s to reach root, so if dotDotCount >= 4, use absolute
							components := strings.Split(filepath.Clean(objectsDir), string(filepath.Separator))
							// Filter out empty components (can happen on some systems)
							nonEmptyCount := 0
							for _, c := range components {
								if c != "" {
									nonEmptyCount++
								}
							}
							if dotDotCount >= nonEmptyCount {
								useRelative = false
								fmt.Printf("  Info: keeping absolute alternates path (relative would go to root)\n")
							}
						}
						if useRelative {
							if err := os.WriteFile(alternatesFile, []byte(relPath+"\n"), 0644); err != nil {
								fmt.Printf("  Warning: failed to update alternates to relative path: %v\n", err)
							}
						}
					}
				}
			}
		}
	}

	// Verify clone was created successfully
	verifyCmd := exec.Command("git", "-C", clonePath, "status", "--short")
	if verifyOut, verifyErr := verifyCmd.CombinedOutput(); verifyErr != nil {
		return fmt.Errorf("clone created but git check failed: %v, output: %s", verifyErr, string(verifyOut))
	}
	fmt.Printf("  ✓ Shared clone created and verified\n")

	return nil
}

// removeWorktree removes a git worktree at the specified path
func removeWorktree(sourceRepo, worktreePath string) error {
	cmd := exec.Command("git", "-C", sourceRepo, "worktree", "remove", worktreePath)
	if err := cmd.Run(); err != nil {
		// Fallback to manual removal if worktree remove fails
		// This can happen if the worktree was already removed or is in a bad state
		return os.RemoveAll(worktreePath)
	}
	return nil
}

// copyDir is a helper to snapshot the workspace (fallback for non-git repos)
func copyDir(src string, dst string) error {
	// Using cp -a for recursive copy on Darwin/Linux
	cmd := exec.Command("cp", "-a", src+"/.", dst+"/")
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("cp failed: %v, output: %s", err, string(out))
	}
	return nil
}

func mkdirTempOutsideWorkspace(workspace, pattern string) (string, error) {
	absWorkspace, err := cleanAbs(workspace)
	if err != nil {
		return "", err
	}

	var baseCandidates []string
	if v := strings.TrimSpace(os.Getenv("HOLON_SNAPSHOT_BASE")); v != "" {
		baseCandidates = append(baseCandidates, v)
	}
	baseCandidates = append(baseCandidates, os.TempDir())

	if cacheDir, err := os.UserCacheDir(); err == nil && cacheDir != "" {
		baseCandidates = append(baseCandidates, filepath.Join(cacheDir, "holon"))
	}

	// Parent directory is a good, usually writable, fallback.
	baseCandidates = append(baseCandidates, filepath.Dir(absWorkspace))

	if runtime.GOOS != "windows" {
		baseCandidates = append(baseCandidates, "/tmp")
	}

	var lastErr error
	for _, base := range baseCandidates {
		if strings.TrimSpace(base) == "" {
			continue
		}
		absBase, err := cleanAbs(base)
		if err != nil {
			lastErr = err
			continue
		}
		if isSubpath(absBase, absWorkspace) {
			continue
		}
		if err := os.MkdirAll(absBase, 0o755); err != nil {
			lastErr = err
			continue
		}
		dir, err := os.MkdirTemp(absBase, pattern)
		if err != nil {
			lastErr = err
			continue
		}
		return dir, nil
	}

	if lastErr != nil {
		return "", lastErr
	}
	return "", fmt.Errorf("unable to create temp dir outside workspace %q", absWorkspace)
}

func cleanAbs(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	if resolved, err := filepath.EvalSymlinks(abs); err == nil {
		return resolved, nil
	}
	return abs, nil
}

func isSubpath(candidate, parent string) bool {
	rel, err := filepath.Rel(parent, candidate)
	if err != nil {
		return false
	}
	rel = filepath.Clean(rel)
	if rel == "." {
		return true
	}
	return !strings.HasPrefix(rel, ".."+string(filepath.Separator)) && rel != ".."
}

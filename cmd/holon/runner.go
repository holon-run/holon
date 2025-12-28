package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"

	"github.com/holon-run/holon/pkg/agent/resolver"
	v1 "github.com/holon-run/holon/pkg/api/v1"
	gh "github.com/holon-run/holon/pkg/github"
	holonlog "github.com/holon-run/holon/pkg/log"
	"github.com/holon-run/holon/pkg/prompt"
	"github.com/holon-run/holon/pkg/runtime/docker"
	"gopkg.in/yaml.v3"
)

// Runtime interface defines the contract for running holon containers
// This allows for easy mocking in tests
type Runtime interface {
	RunHolon(ctx context.Context, cfg *docker.ContainerConfig) (string, error)
}

// RunnerConfig holds the configuration for the Run function
type RunnerConfig struct {
	SpecPath             string
	GoalStr              string
	TaskName             string
	BaseImage            string
	AgentBundle          string
	WorkspacePath        string
	ContextPath          string
	InputPath            string // Optional: path to input directory (if empty, creates temp dir)
	OutDir               string
	OutDirIsTemp         bool // true if output dir is a temporary directory (vs user-provided)
	RoleName             string
	EnvVarsList          []string
	LogLevel             string
	Mode                 string
	Cleanup              string // Cleanup mode: "auto" (default), "none", "all"
	AgentConfigMode      string // Agent config mount mode: "auto", "yes", "no"
	GitAuthorName        string // Optional: git author name override
	GitAuthorEmail       string // Optional: git author email override
	WorkspaceIsTemporary bool   // true if workspace is a temporary directory (vs user-provided)
}

// Runner encapsulates the dependencies and state needed to run a holon
type Runner struct {
	runtime  Runtime
	resolver *resolver.Registry
}

// NewRunner creates a new Runner with the given runtime
func NewRunner(rt Runtime) *Runner {
	// Initialize cache directory from environment or use default
	cacheDir := os.Getenv("HOLON_CACHE_DIR")
	return &Runner{
		runtime:  rt,
		resolver: resolver.NewRegistry(cacheDir),
	}
}

// Run executes a holon with the given configuration
// This is the main logic extracted from the cobra Command
func (r *Runner) Run(ctx context.Context, cfg RunnerConfig) error {
	// Validation deferred to allow goal extraction from Spec
	if cfg.SpecPath == "" && cfg.GoalStr == "" {
		return fmt.Errorf("either --spec or --goal is required")
	}

	// Determine cleanup mode
	cleanupMode := cfg.Cleanup
	if cleanupMode == "" {
		cleanupMode = "auto" // Default to auto cleanup
	}

	// Validate cleanup mode
	switch cleanupMode {
	case "auto", "none", "all":
		// Valid cleanup modes
	default:
		return fmt.Errorf("invalid cleanup mode %q; must be one of: auto, none, all", cleanupMode)
	}

	// Create or use input directory
	inputDir := cfg.InputPath
	inputIsTemp := false
	if inputDir == "" {
		// Create temporary input directory
		td, err := os.MkdirTemp("", "holon-input-*")
		if err != nil {
			return fmt.Errorf("failed to create temp input dir: %w", err)
		}
		inputDir = td
		inputIsTemp = true
	}

	// Cleanup input directory based on mode and whether it's temp
	// For temp input: clean on "auto" or "all"
	// For user input: clean only on "all"
	if (inputIsTemp && (cleanupMode == "auto" || cleanupMode == "all")) ||
		(!inputIsTemp && cleanupMode == "all") {
		defer os.RemoveAll(inputDir)
	}

	// Cleanup output directory only when cleanup mode is "all"
	// This applies to both temp and user-provided output directories
	if cleanupMode == "all" {
		defer os.RemoveAll(cfg.OutDir)
	}

	// Create input subdirectories
	inputPromptsDir := filepath.Join(inputDir, "prompts")
	if err := os.MkdirAll(inputPromptsDir, 0755); err != nil {
		return fmt.Errorf("failed to create input/prompts dir: %w", err)
	}

	// Copy or create context directory in input
	var absContext string
	if cfg.ContextPath != "" {
		var err error
		absContext, err = filepath.Abs(cfg.ContextPath)
		if err != nil {
			return fmt.Errorf("failed to resolve context path: %w", err)
		}
		// Copy context to input directory
		inputContextDir := filepath.Join(inputDir, "context")
		if samePath(absContext, inputContextDir) {
			holonlog.Debug("context path already in input dir; skipping copy", "path", inputContextDir)
		} else {
			if err := copyDir(absContext, inputContextDir); err != nil {
				return fmt.Errorf("failed to copy context to input dir: %w", err)
			}
		}
		absContext = inputContextDir
	}

	var tempSpecDir string
	var cleanupNeeded bool

	if cfg.GoalStr != "" {
		if cfg.TaskName == "" {
			cfg.TaskName = fmt.Sprintf("adhoc-%d", os.Getpid())
		}
		// Create a temporary spec file
		td, err := os.MkdirTemp("", "holon-spec-*")
		if err != nil {
			return fmt.Errorf("failed to create temp dir for spec: %w", err)
		}
		tempSpecDir = td
		cleanupNeeded = true

		cfg.SpecPath = filepath.Join(tempSpecDir, "spec.yaml")
		specContent := fmt.Sprintf(`version: "v1"
kind: Holon
metadata:
  name: %q
goal:
  description: %q
output:
  artifacts:
    - path: "manifest.json"
      required: true
    - path: "diff.patch"
      required: true
    - path: "summary.md"
      required: true
`, cfg.TaskName, cfg.GoalStr)

		if err := os.WriteFile(cfg.SpecPath, []byte(specContent), 0644); err != nil {
			if cleanupNeeded {
				os.RemoveAll(tempSpecDir)
			}
			return fmt.Errorf("failed to write temporary spec: %w", err)
		}
	}

	// Ensure cleanup happens even if we fail later
	if cleanupNeeded {
		defer os.RemoveAll(tempSpecDir)
	}

	absWorkspace, err := filepath.Abs(cfg.WorkspacePath)
	if err != nil {
		return fmt.Errorf("failed to resolve workspace path: %w", err)
	}
	absSpec, err := filepath.Abs(cfg.SpecPath)
	if err != nil {
		return fmt.Errorf("failed to resolve spec path: %w", err)
	}
	absOut, err := filepath.Abs(cfg.OutDir)
	if err != nil {
		return fmt.Errorf("failed to resolve output path: %w", err)
	}

	// Ensure out dir exists
	if err := os.MkdirAll(absOut, 0755); err != nil {
		return fmt.Errorf("failed to create output directory: %w", err)
	}

	// Copy spec to input directory
	inputSpecPath := filepath.Join(inputDir, "spec.yaml")
	if err := copyFile(absSpec, inputSpecPath); err != nil {
		return fmt.Errorf("failed to copy spec to input dir: %w", err)
	}

	// Collect Env Vars with proper precedence
	envVars, err := r.collectEnvVars(cfg, absSpec)
	if err != nil {
		return err
	}

	// Apply git config overrides from project config
	// These override host git config for container operations
	if cfg.GitAuthorName != "" {
		envVars["GIT_AUTHOR_NAME"] = cfg.GitAuthorName
		envVars["GIT_COMMITTER_NAME"] = cfg.GitAuthorName
		holonlog.Info("git config", "author_name", cfg.GitAuthorName, "source", "config")
	}
	if cfg.GitAuthorEmail != "" {
		envVars["GIT_AUTHOR_EMAIL"] = cfg.GitAuthorEmail
		envVars["GIT_COMMITTER_EMAIL"] = cfg.GitAuthorEmail
		holonlog.Info("git config", "author_email", cfg.GitAuthorEmail, "source", "config")
	}

	// Populate Goal from Spec if not provided via flag
	if cfg.GoalStr == "" && cfg.SpecPath != "" {
		goal, err := r.extractGoalFromSpec(absSpec)
		if err != nil {
			// Non-fatal error, just warn
			holonlog.Warn("failed to extract goal from spec", "error", err)
		} else {
			cfg.GoalStr = goal
		}
	}

	// Validation: must have goal by now
	if cfg.GoalStr == "" {
		return fmt.Errorf("goal description is missing in spec or flags")
	}

	// Add log_level to environment variables
	if cfg.LogLevel != "" {
		envVars["LOG_LEVEL"] = cfg.LogLevel
	} else {
		envVars["LOG_LEVEL"] = "progress" // Default to progress mode
	}

	// Add mode to environment variables
	if cfg.Mode != "" {
		envVars["HOLON_MODE"] = cfg.Mode
	} else {
		envVars["HOLON_MODE"] = "solve" // Default to solve mode
	}

	// Compile prompts
	sysPrompt, userPrompt, promptTempDir, err := r.compilePrompts(cfg, absContext, envVars)
	if err != nil {
		return err
	}
	defer os.RemoveAll(promptTempDir)

	// Copy prompts to input directory
	if err := os.WriteFile(filepath.Join(inputPromptsDir, "system.md"), []byte(sysPrompt), 0644); err != nil {
		return fmt.Errorf("failed to write system prompt to input dir: %w", err)
	}
	if err := os.WriteFile(filepath.Join(inputPromptsDir, "user.md"), []byte(userPrompt), 0644); err != nil {
		return fmt.Errorf("failed to write user prompt to input dir: %w", err)
	}

	// Write debug prompts to output directory
	if err := r.writeDebugPrompts(absOut, sysPrompt, userPrompt); err != nil {
		holonlog.Warn("failed to write debug prompts", "error", err)
	}

	// Resolve input directory to absolute path
	absInputDir, err := filepath.Abs(inputDir)
	if err != nil {
		return fmt.Errorf("failed to resolve input path: %w", err)
	}

	agentBundlePath, err := r.resolveAgentBundle(ctx, cfg, absWorkspace)
	if err != nil {
		return err
	}

	containerCfg := &docker.ContainerConfig{
		BaseImage:            cfg.BaseImage,
		AgentBundle:          agentBundlePath,
		Workspace:            absWorkspace,
		InputPath:            absInputDir,
		OutDir:               absOut,
		Env:                  envVars,
		AgentConfigMode:      cfg.AgentConfigMode,
		WorkspaceIsTemporary: cfg.WorkspaceIsTemporary,
	}

	holonlog.Progress("running holon", "spec", cfg.SpecPath, "base_image", cfg.BaseImage, "agent", containerCfg.AgentBundle)
	snapshotDir, err := r.runtime.RunHolon(ctx, containerCfg)
	if err != nil {
		return fmt.Errorf("execution failed: %w", err)
	}
	holonlog.Progress("holon execution completed")

	// Set HOLON_WORKSPACE to point to the actual workspace that was modified
	// This is critical for post-execution operations like publish
	if snapshotDir != "" {
		if err := os.Setenv("HOLON_WORKSPACE", snapshotDir); err != nil {
			return fmt.Errorf("failed to set HOLON_WORKSPACE environment variable: %w", err)
		}
		// Export snapshotDir via environment variable for caller to clean up
		if err := os.Setenv("HOLON_SNAPSHOT_DIR", snapshotDir); err != nil {
			holonlog.Warn("failed to set HOLON_SNAPSHOT_DIR", "error", err)
		}
	}

	// NOTE: Output directory cleanup is intentionally not performed here.
	// It is handled at a higher level (e.g., after results have been published)
	// to avoid deleting outputs before they are consumed.

	return nil
}

func (r *Runner) resolveAgentBundle(ctx context.Context, cfg RunnerConfig, workspace string) (string, error) {
	agentRef := strings.TrimSpace(cfg.AgentBundle)
	if agentRef == "" {
		agentRef = strings.TrimSpace(os.Getenv("HOLON_AGENT"))
	}
	if agentRef == "" {
		agentRef = strings.TrimSpace(os.Getenv("HOLON_AGENT_BUNDLE"))
	}

	// If we have an explicit reference, try to resolve it using the resolver system
	if agentRef != "" {
		resolvedPath, err := r.resolver.Resolve(ctx, agentRef)
		if err != nil {
			return "", fmt.Errorf("failed to resolve agent bundle '%s': %w", agentRef, err)
		}
		return resolvedPath, nil
	}

	// Try builtin agent (auto-install) first
	holonlog.Info("no agent specified, trying builtin default agent")
	resolvedPath, err := r.resolver.Resolve(ctx, "") // Empty string triggers builtin resolver
	if err == nil {
		holonlog.Info("successfully resolved builtin agent", "path", resolvedPath)
		return resolvedPath, nil
	}

	// If builtin agent failed (e.g., auto-install disabled), fall back to local build system
	holonlog.Debug("builtin agent not available", "error", err)
	holonlog.Info("falling back to local build system")

	scriptPath := filepath.Join(workspace, "agents", "claude", "scripts", "build-bundle.sh")
	if _, err := os.Stat(scriptPath); err != nil {
		return "", fmt.Errorf("agent bundle not found; set --agent/HOLON_AGENT (legacy: --agent-bundle/HOLON_AGENT_BUNDLE) or enable auto-install")
	}

	bundleDir := filepath.Join(workspace, "agents", "claude", "dist", "agent-bundles")
	bundlePath, err := findLatestBundle(bundleDir)
	if err != nil {
		return "", err
	}
	if bundlePath == "" {
		holonlog.Info("agent bundle not found, building local bundle")
		if err := buildAgentBundle(scriptPath, workspace); err != nil {
			return "", err
		}
		bundlePath, err = findLatestBundle(bundleDir)
		if err != nil {
			return "", err
		}
		if bundlePath == "" {
			return "", fmt.Errorf("agent bundle build completed but no bundle found in %s", bundleDir)
		}
	}

	return bundlePath, nil
}

func findLatestBundle(dir string) (string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil
		}
		return "", err
	}

	type bundleInfo struct {
		path    string
		modTime int64
	}

	var bundles []bundleInfo
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".tar.gz") {
			continue
		}
		info, err := entry.Info()
		if err != nil {
			return "", err
		}
		bundles = append(bundles, bundleInfo{
			path:    filepath.Join(dir, entry.Name()),
			modTime: info.ModTime().UnixNano(),
		})
	}

	if len(bundles) == 0 {
		return "", nil
	}

	sort.Slice(bundles, func(i, j int) bool {
		return bundles[i].modTime > bundles[j].modTime
	})

	return bundles[0].path, nil
}

func buildAgentBundle(scriptPath, workspace string) error {
	cmd := exec.Command("bash", scriptPath)
	cmd.Dir = workspace
	cmd.Env = os.Environ()
	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("failed to build agent bundle: %v, output: %s", err, string(output))
	}
	return nil
}

// collectEnvVars collects environment variables with proper precedence:
// spec context.env < auto injection < CLI --env flags
func (r *Runner) collectEnvVars(cfg RunnerConfig, absSpec string) (map[string]string, error) {
	envVars := make(map[string]string)

	// 0. Parse spec file to extract context.env (lowest priority)
	if cfg.SpecPath != "" {
		specData, err := os.ReadFile(absSpec)
		if err != nil {
			return nil, fmt.Errorf("failed to read spec file: %w", err)
		}

		var spec v1.HolonSpec
		if err := yaml.Unmarshal(specData, &spec); err != nil {
			return nil, fmt.Errorf("failed to parse spec file: %w", err)
		}

		// Add context.env variables (lowest priority)
		for k, v := range spec.Context.Env {
			envVars[k] = v
		}
	}

	// 1. Automatic Secret Injection (v0.1: Anthropic Key & URL)
	anthropicKey := os.Getenv("ANTHROPIC_API_KEY")
	if anthropicKey == "" {
		anthropicKey = os.Getenv("ANTHROPIC_AUTH_TOKEN")
	}
	if anthropicKey != "" {
		envVars["ANTHROPIC_API_KEY"] = anthropicKey
		envVars["ANTHROPIC_AUTH_TOKEN"] = anthropicKey
	}

	// Support both ANTHROPIC_BASE_URL (new) and ANTHROPIC_API_URL (alias for convenience)
	anthropicURL := os.Getenv("ANTHROPIC_BASE_URL")
	if anthropicURL == "" {
		anthropicURL = os.Getenv("ANTHROPIC_API_URL")
	}
	if anthropicURL != "" {
		envVars["ANTHROPIC_BASE_URL"] = anthropicURL
		envVars["ANTHROPIC_API_URL"] = anthropicURL
	}

	// 1.5. Automatic GitHub Secret Injection
	var githubToken string
	if token := os.Getenv("GITHUB_TOKEN"); token != "" {
		githubToken = token
		envVars["GITHUB_TOKEN"] = token
		envVars["GH_TOKEN"] = token
	} else if token := os.Getenv("GH_TOKEN"); token != "" {
		githubToken = token
		envVars["GITHUB_TOKEN"] = token
		envVars["GH_TOKEN"] = token
	}

	// 1.55. Resolve GitHub actor identity if token is available
	// This allows the agent to know its own identity and avoid self-replies
	if githubToken != "" {
		actorInfo := r.resolveGitHubActorIdentity(context.Background(), githubToken)
		if actorInfo != nil {
			envVars["HOLON_ACTOR_LOGIN"] = actorInfo.Login
			envVars["HOLON_ACTOR_TYPE"] = actorInfo.Type
			if actorInfo.Source != "" {
				envVars["HOLON_ACTOR_SOURCE"] = actorInfo.Source
			}
			if actorInfo.AppSlug != "" {
				envVars["HOLON_ACTOR_APP_SLUG"] = actorInfo.AppSlug
			}
			// Log identity resolution (without exposing sensitive data)
			holonlog.Info("github actor identity resolved", "login", actorInfo.Login, "type", actorInfo.Type)
		} else {
			// Identity lookup failed - non-critical, log and continue
			holonlog.Info("github actor identity lookup failed, continuing without identity")
		}
	}

	// 1.6. Automatic Holon Configuration Injection (for testing and agent behavior)
	// HOLON_CLAUDE_DRIVER: Select driver implementation (mock, real SDK, etc.)
	if driver := os.Getenv("HOLON_CLAUDE_DRIVER"); driver != "" {
		envVars["HOLON_CLAUDE_DRIVER"] = driver
	}
	// HOLON_CLAUDE_MOCK_FIXTURE: Path to mock driver fixture file
	if fixture := os.Getenv("HOLON_CLAUDE_MOCK_FIXTURE"); fixture != "" {
		envVars["HOLON_CLAUDE_MOCK_FIXTURE"] = fixture
	}
	// HOLON_MODE: Execution mode (solve, pr-fix, etc.)
	if mode := os.Getenv("HOLON_MODE"); mode != "" {
		envVars["HOLON_MODE"] = mode
	}

	// 2. Custom Env Vars from CLI (--env K=V) - highest priority
	for _, pair := range cfg.EnvVarsList {
		parts := strings.SplitN(pair, "=", 2)
		if len(parts) == 2 {
			envVars[parts[0]] = parts[1]
		}
	}

	return envVars, nil
}

// resolveGitHubActorIdentity resolves the GitHub actor identity from a token
// Returns nil if identity lookup fails (non-critical operation)
func (r *Runner) resolveGitHubActorIdentity(ctx context.Context, token string) *gh.ActorInfo {
	client := gh.NewClient(token)
	actorInfo, err := client.GetCurrentUser(ctx)
	if err != nil {
		// Non-critical: log and return nil
		// The error may be due to network issues, invalid token, or rate limiting
		// We don't want to block execution for this
		holonlog.Warn("failed to resolve github actor identity", "error", err)
		return nil
	}
	return actorInfo
}

// extractGoalFromSpec extracts the goal description from a spec file
func (r *Runner) extractGoalFromSpec(absSpec string) (string, error) {
	specData, err := os.ReadFile(absSpec)
	if err != nil {
		return "", fmt.Errorf("failed to read spec: %w", err)
	}

	var spec v1.HolonSpec
	if err := yaml.Unmarshal(specData, &spec); err != nil {
		return "", fmt.Errorf("failed to parse spec: %w", err)
	}

	return spec.Goal.Description, nil
}

// compilePrompts compiles system and user prompts
func (r *Runner) compilePrompts(cfg RunnerConfig, absContext string, envVars map[string]string) (sysPrompt, userPrompt string, promptTempDir string, err error) {
	compiler := prompt.NewCompiler("")

	// Extract context files for template
	contextFiles := []string{}
	if cfg.ContextPath != "" {
		files, err := os.ReadDir(absContext)
		if err != nil {
			holonlog.Warn("failed to read context directory", "error", err)
		} else {
			for _, f := range files {
				contextFiles = append(contextFiles, f.Name())
			}
		}
	}

	sysPrompt, err = compiler.CompileSystemPrompt(prompt.Config{
		Mode:         cfg.Mode,
		Role:         cfg.RoleName,
		Language:     "en", // TODO: Detect or flag
		WorkingDir:   "/holon/workspace",
		ContextFiles: contextFiles,
		// Pass GitHub actor identity from environment variables
		ActorLogin:   envVars["HOLON_ACTOR_LOGIN"],
		ActorType:    envVars["HOLON_ACTOR_TYPE"],
		ActorSource:  envVars["HOLON_ACTOR_SOURCE"],
		ActorAppSlug: envVars["HOLON_ACTOR_APP_SLUG"],
	})
	if err != nil {
		return "", "", "", fmt.Errorf("failed to compile system prompt: %w", err)
	}

	// Create temp directory for prompts
	promptTempDir, err = os.MkdirTemp("", "holon-prompt-*")
	if err != nil {
		return "", "", "", fmt.Errorf("failed to create temporary prompt dir: %w", err)
	}

	// Write System Prompt
	sysPromptPath := filepath.Join(promptTempDir, "system.md")
	if err := os.WriteFile(sysPromptPath, []byte(sysPrompt), 0644); err != nil {
		os.RemoveAll(promptTempDir)
		return "", "", "", fmt.Errorf("failed to write system prompt: %w", err)
	}

	// Compile User Prompt
	var contextFileNames []string
	if cfg.ContextPath != "" {
		files, err := os.ReadDir(absContext)
		if err != nil {
			holonlog.Warn("failed to read context directory for user prompt", "error", err)
		} else {
			for _, f := range files {
				if !f.IsDir() {
					contextFileNames = append(contextFileNames, f.Name())
				}
			}
		}
	}

	userPrompt, err = compiler.CompileUserPrompt(cfg.GoalStr, contextFileNames)
	if err != nil {
		os.RemoveAll(promptTempDir)
		return "", "", "", fmt.Errorf("failed to compile user prompt: %w", err)
	}

	// Write User Prompt
	userPromptPath := filepath.Join(promptTempDir, "user.md")
	if err := os.WriteFile(userPromptPath, []byte(userPrompt), 0644); err != nil {
		os.RemoveAll(promptTempDir)
		return "", "", "", fmt.Errorf("failed to write user prompt: %w", err)
	}

	return sysPrompt, userPrompt, promptTempDir, nil
}

// writeDebugPrompts writes the compiled prompts to the output directory for debugging
func (r *Runner) writeDebugPrompts(absOut, sysPrompt, userPrompt string) error {
	if err := os.WriteFile(filepath.Join(absOut, "prompt.compiled.system.md"), []byte(sysPrompt), 0644); err != nil {
		return fmt.Errorf("failed to write debug system prompt: %w", err)
	}
	if err := os.WriteFile(filepath.Join(absOut, "prompt.compiled.user.md"), []byte(userPrompt), 0644); err != nil {
		return fmt.Errorf("failed to write debug user prompt: %w", err)
	}
	return nil
}

// copyFile copies a file from src to dst, preserving permissions
func copyFile(src, dst string) error {
	// Get source file info to preserve permissions
	info, err := os.Stat(src)
	if err != nil {
		return fmt.Errorf("failed to stat source file: %w", err)
	}

	in, err := os.Open(src)
	if err != nil {
		return fmt.Errorf("failed to open source file: %w", err)
	}
	defer in.Close()

	out, err := os.Create(dst)
	if err != nil {
		return fmt.Errorf("failed to create destination file: %w", err)
	}
	defer out.Close()

	if _, err := io.Copy(out, in); err != nil {
		return fmt.Errorf("failed to copy file contents: %w", err)
	}

	// Sync to ensure data is written to disk
	if err := out.Sync(); err != nil {
		return fmt.Errorf("failed to sync file: %w", err)
	}

	// Preserve file permissions
	if err := os.Chmod(dst, info.Mode()); err != nil {
		return fmt.Errorf("failed to set file permissions: %w", err)
	}

	return nil
}

// copyDir recursively copies a directory tree from src to dst
// Handles regular files, directories, and symbolic links
// Returns early if src and dst are the same path
func copyDir(src, dst string) error {
	// Guard against self-copy: if src and dst are the same, skip copy
	// This prevents truncation bugs when copying a directory onto itself
	if samePath(src, dst) {
		return nil
	}

	entries, err := os.ReadDir(src)
	if err != nil {
		return fmt.Errorf("failed to read source directory: %w", err)
	}

	if err := os.MkdirAll(dst, 0755); err != nil {
		return fmt.Errorf("failed to create destination directory: %w", err)
	}

	for _, entry := range entries {
		srcPath := filepath.Join(src, entry.Name())
		dstPath := filepath.Join(dst, entry.Name())

		// Check if it's a symbolic link
		if entry.Type()&os.ModeSymlink != 0 {
			// Read the symlink target
			target, err := os.Readlink(srcPath)
			if err != nil {
				return fmt.Errorf("failed to read symlink %s: %w", srcPath, err)
			}
			// Create the same symlink at destination
			if err := os.Symlink(target, dstPath); err != nil {
				return fmt.Errorf("failed to create symlink %s: %w", dstPath, err)
			}
			continue
		}

		if entry.IsDir() {
			if err := copyDir(srcPath, dstPath); err != nil {
				return err
			}
		} else {
			if err := copyFile(srcPath, dstPath); err != nil {
				return err
			}
		}
	}

	return nil
}

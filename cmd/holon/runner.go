package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	v1 "github.com/jolestar/holon/pkg/api/v1"
	"github.com/jolestar/holon/pkg/prompt"
	"github.com/jolestar/holon/pkg/runtime/docker"
	"gopkg.in/yaml.v3"
)

// Runtime interface defines the contract for running holon containers
// This allows for easy mocking in tests
type Runtime interface {
	RunHolon(ctx context.Context, cfg *docker.ContainerConfig) error
}

// RunnerConfig holds the configuration for the Run function
type RunnerConfig struct {
	SpecPath      string
	GoalStr       string
	TaskName      string
	BaseImage     string
	AdapterImage  string
	WorkspacePath string
	ContextPath   string
	OutDir        string
	RoleName      string
	EnvVarsList   []string
	LogLevel      string
}

// Runner encapsulates the dependencies and state needed to run a holon
type Runner struct {
	runtime Runtime
}

// NewRunner creates a new Runner with the given runtime
func NewRunner(rt Runtime) *Runner {
	return &Runner{
		runtime: rt,
	}
}

// Run executes a holon with the given configuration
// This is the main logic extracted from the cobra Command
func (r *Runner) Run(ctx context.Context, cfg RunnerConfig) error {
	// Validation deferred to allow goal extraction from Spec
	if cfg.SpecPath == "" && cfg.GoalStr == "" {
		return fmt.Errorf("either --spec or --goal is required")
	}

	var tempDir string
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
		tempDir = td
		cleanupNeeded = true

		cfg.SpecPath = filepath.Join(tempDir, "spec.yaml")
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
				os.RemoveAll(tempDir)
			}
			return fmt.Errorf("failed to write temporary spec: %w", err)
		}
	}

	// Ensure cleanup happens even if we fail later
	if cleanupNeeded {
		defer os.RemoveAll(tempDir)
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
	var absContext string
	if cfg.ContextPath != "" {
		absContext, err = filepath.Abs(cfg.ContextPath)
		if err != nil {
			return fmt.Errorf("failed to resolve context path: %w", err)
		}
	}

	// Ensure out dir exists
	if err := os.MkdirAll(absOut, 0755); err != nil {
		return fmt.Errorf("failed to create output directory: %w", err)
	}

	// Collect Env Vars with proper precedence
	envVars, err := r.collectEnvVars(cfg, absSpec)
	if err != nil {
		return err
	}

	// Populate Goal from Spec if not provided via flag
	if cfg.GoalStr == "" && cfg.SpecPath != "" {
		goal, err := r.extractGoalFromSpec(absSpec)
		if err != nil {
			// Non-fatal error, just warn
			fmt.Printf("Warning: Failed to extract goal from spec: %v\n", err)
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

	// Compile prompts
	sysPrompt, userPrompt, promptTempDir, err := r.compilePrompts(cfg, absContext)
	if err != nil {
		return err
	}
	defer os.RemoveAll(promptTempDir)
	absPromptTempDir, err := filepath.Abs(promptTempDir)
	if err != nil {
		return fmt.Errorf("failed to resolve prompt temp dir: %w", err)
	}

	// Write debug prompts to output directory
	if err := r.writeDebugPrompts(absOut, sysPrompt, userPrompt); err != nil {
		fmt.Printf("Warning: Failed to write debug prompts: %v\n", err)
	}

	adapterImage := cfg.AdapterImage
	if adapterImage == "" {
		adapterImage = "holon-adapter-claude"
	}

	containerCfg := &docker.ContainerConfig{
		BaseImage:      cfg.BaseImage,
		AdapterImage:   adapterImage,
		Workspace:      absWorkspace,
		SpecPath:       absSpec,
		ContextPath:    absContext,
		PromptPath:     filepath.Join(absPromptTempDir, "system.md"),
		UserPromptPath: filepath.Join(absPromptTempDir, "user.md"),
		OutDir:         absOut,
		Env:            envVars,
	}

	fmt.Printf("Running Holon: %s with base image %s (adapter: %s)\n", cfg.SpecPath, cfg.BaseImage, containerCfg.AdapterImage)
	if err := r.runtime.RunHolon(ctx, containerCfg); err != nil {
		return fmt.Errorf("execution failed: %w", err)
	}
	fmt.Println("Holon execution completed.")

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
	if token := os.Getenv("GITHUB_TOKEN"); token != "" {
		envVars["GITHUB_TOKEN"] = token
		envVars["GH_TOKEN"] = token
	} else if token := os.Getenv("GH_TOKEN"); token != "" {
		envVars["GITHUB_TOKEN"] = token
		envVars["GH_TOKEN"] = token
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
func (r *Runner) compilePrompts(cfg RunnerConfig, absContext string) (sysPrompt, userPrompt string, promptTempDir string, err error) {
	compiler := prompt.NewCompiler("")

	// Extract context files for template
	contextFiles := []string{}
	if cfg.ContextPath != "" {
		files, err := os.ReadDir(absContext)
		if err != nil {
			fmt.Printf("Warning: Failed to read context directory: %v\n", err)
		} else {
			for _, f := range files {
				contextFiles = append(contextFiles, f.Name())
			}
		}
	}

	sysPrompt, err = compiler.CompileSystemPrompt(prompt.Config{
		Role:         cfg.RoleName,
		Language:     "en", // TODO: Detect or flag
		WorkingDir:   "/holon/workspace",
		ContextFiles: contextFiles,
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
			fmt.Printf("Warning: Failed to read context directory for user prompt: %v\n", err)
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

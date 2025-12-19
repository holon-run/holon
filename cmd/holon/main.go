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
	"github.com/spf13/cobra"
	"gopkg.in/yaml.v3"
)

var specPath string
var goalStr string
var taskName string
var adapterImage string
var workspacePath string
var contextPath string
var outDir string
var roleName string
var envVarsList []string
var logLevel string

var runCmd = &cobra.Command{
	Use:   "run",
	Short: "Run a Holon execution unit",
	Run: func(cmd *cobra.Command, args []string) {
		// Validation deferred to allow goal extraction from Spec
		if specPath == "" && goalStr == "" {
			fmt.Println("Error: either --spec or --goal is required")
			os.Exit(1)
		}

		if goalStr != "" {
			if taskName == "" {
				taskName = fmt.Sprintf("adhoc-%d", os.Getpid())
			}
			// Create a temporary spec file
			tempDir, err := os.MkdirTemp("", "holon-spec-*")
			if err != nil {
				fmt.Printf("Failed to create temp dir for spec: %v\n", err)
				os.Exit(1)
			}
			specPath = filepath.Join(tempDir, "spec.yaml")
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
`, taskName, goalStr)
			if err := os.WriteFile(specPath, []byte(specContent), 0644); err != nil {
				fmt.Printf("Failed to write temporary spec: %v\n", err)
				os.Exit(1)
			}
			defer os.RemoveAll(tempDir)
		}

		ctx := context.Background()
		rt, err := docker.NewRuntime()
		if err != nil {
			fmt.Printf("Failed to initialize runtime: %v\n", err)
			os.Exit(1)
		}

		absWorkspace, err := filepath.Abs(workspacePath)
		if err != nil {
			fmt.Printf("Failed to resolve workspace path: %v\n", err)
			os.Exit(1)
		}
		absSpec, err := filepath.Abs(specPath)
		if err != nil {
			fmt.Printf("Failed to resolve spec path: %v\n", err)
			os.Exit(1)
		}
		absOut, err := filepath.Abs(outDir)
		if err != nil {
			fmt.Printf("Failed to resolve output path: %v\n", err)
			os.Exit(1)
		}
		var absContext string
		if contextPath != "" {
			absContext, err = filepath.Abs(contextPath)
			if err != nil {
				fmt.Printf("Failed to resolve context path: %v\n", err)
				os.Exit(1)
			}
		}

		// Ensure out dir exists
		os.MkdirAll(absOut, 0755)

		// Collect Env Vars
		envVars := make(map[string]string)

		// 0. Parse spec file to extract context.env
		if specPath != "" {
			specData, err := os.ReadFile(absSpec)
			if err != nil {
				fmt.Printf("Failed to read spec file: %v\n", err)
				os.Exit(1)
			}

			var spec v1.HolonSpec
			if err := yaml.Unmarshal(specData, &spec); err != nil {
				fmt.Printf("Failed to parse spec file: %v\n", err)
				os.Exit(1)
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

		// 1.6 Populate Goal from Spec if not provided via flag
		if goalStr == "" && specPath != "" {
			// We already unmarshaled context.env, let's fully unmarshal or re-read
			specData, err := os.ReadFile(absSpec)
			if err != nil {
				fmt.Printf("Warning: Failed to read spec for goal extraction: %v\n", err)
			} else {
				var spec v1.HolonSpec
				if err := yaml.Unmarshal(specData, &spec); err != nil {
					fmt.Printf("Warning: Failed to parse spec for goal extraction: %v\n", err)
				} else {
					goalStr = spec.Goal.Description
				}
			}
		}

		// Validation: must have goal by now
		if goalStr == "" {
			fmt.Println("Error: goal description is missing in spec or flags")
			os.Exit(1)
		}

		// 2. Custom Env Vars from CLI (--env K=V) - highest priority
		for _, pair := range envVarsList {
			parts := strings.SplitN(pair, "=", 2)
			if len(parts) == 2 {
				envVars[parts[0]] = parts[1]
			}
		}

		// Add log_level to environment variables
		if logLevel != "" {
			envVars["LOG_LEVEL"] = logLevel
		} else {
			envVars["LOG_LEVEL"] = "progress" // Default to progress mode
		}

		// X. Compile System Prompt
		compiler := prompt.NewCompiler("")
		// NOTE: We do NOT inject project context (CLAUDE.md) into system prompt here.
		// It should be handled by the Agent itself (e.g. Claude Code reads it from workspace),
		// or by the Adapter if explicitly requested.
		// Mixing it into the compiled system prompt causes duplication.

		// Extract context files for template
		contextFiles := []string{}
		if contextPath != "" {
			files, err := os.ReadDir(absContext)
			if err != nil {
				fmt.Printf("Warning: Failed to read context directory: %v\n", err)
			} else {
				for _, f := range files {
					contextFiles = append(contextFiles, f.Name())
				}
			}
		}

		sysPrompt, err := compiler.CompileSystemPrompt(prompt.Config{
			Role:         roleName,
			Language:     "en", // TODO: Detect or flag
			WorkingDir:   "/holon/workspace",
			ContextFiles: contextFiles,
		})
		if err != nil {
			fmt.Printf("Failed to compile system prompt: %v\n", err)
			os.Exit(1)
		}
		// Project Context append removed

		// Write to temp file
		promptTempDir, err := os.MkdirTemp("", "holon-prompt-*")
		if err != nil {
			fmt.Printf("Failed to create temporary prompt dir: %v\n", err)
			os.Exit(1)
		}
		defer os.RemoveAll(promptTempDir)

		// Write System Prompt
		sysPromptPath := filepath.Join(promptTempDir, "system.md")
		if err := os.WriteFile(sysPromptPath, []byte(sysPrompt), 0644); err != nil {
			fmt.Printf("Failed to write system prompt: %v\n", err)
			os.Exit(1)
		}

		// X+1. Compile User Prompt
		// Only collect filenames for the user prompt to keep it concise
		var contextFileNames []string
		if contextPath != "" {
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

		userPrompt, err := compiler.CompileUserPrompt(goalStr, contextFileNames)
		if err != nil {
			fmt.Printf("Failed to compile user prompt: %v\n", err)
			os.Exit(1)
		}

		// Write User Prompt
		userPromptPath := filepath.Join(promptTempDir, "user.md")
		if err := os.WriteFile(userPromptPath, []byte(userPrompt), 0644); err != nil {
			fmt.Printf("Failed to write user prompt: %v\n", err)
			os.Exit(1)
		}

		// Debug Outputs (as requested in Issue #40)
		if err := os.WriteFile(filepath.Join(absOut, "prompt.compiled.system.md"), []byte(sysPrompt), 0644); err != nil {
			fmt.Printf("Warning: Failed to write debug system prompt: %v\n", err)
		}
		if err := os.WriteFile(filepath.Join(absOut, "prompt.compiled.user.md"), []byte(userPrompt), 0644); err != nil {
			fmt.Printf("Warning: Failed to write debug user prompt: %v\n", err)
		}

		cfg := &docker.ContainerConfig{
			BaseImage:      adapterImage,
			AdapterImage:   "holon-adapter-claude",
			Workspace:      absWorkspace,
			SpecPath:       absSpec,
			ContextPath:    absContext,
			PromptPath:     sysPromptPath,
			UserPromptPath: userPromptPath,
			OutDir:         absOut,
			Env:            envVars,
		}

		fmt.Printf("Running Holon: %s with base image %s\n", specPath, adapterImage)
		if err := rt.RunHolon(ctx, cfg); err != nil {
			fmt.Printf("Execution failed: %v\n", err)
			os.Exit(1)
		}
		fmt.Println("Holon execution completed.")
	},
}

var rootCmd = &cobra.Command{
	Use:   "holon",
	Short: "Holon is a standardized execution unit for AI-driven software engineering.",
}

func init() {
	runCmd.Flags().StringVarP(&specPath, "spec", "s", "", "Path to holon spec file")
	runCmd.Flags().StringVarP(&goalStr, "goal", "g", "", "Goal description (alternative to --spec)")
	runCmd.Flags().StringVarP(&taskName, "name", "n", "", "Task name (optional, defaults to auto-generated)")
	runCmd.Flags().StringVarP(&adapterImage, "image", "i", "golang:1.22", "Docker image for execution (Base toolchain)")
	runCmd.Flags().StringVarP(&workspacePath, "workspace", "w", ".", "Path to workspace")
	runCmd.Flags().StringVarP(&contextPath, "context", "c", "", "Path to context directory")
	runCmd.Flags().StringVarP(&outDir, "out", "o", "./holon-output", "Path to output directory")
	runCmd.Flags().StringVarP(&roleName, "role", "r", "", "Role to assume (e.g. coder, architect)")
	runCmd.Flags().StringSliceVarP(&envVarsList, "env", "e", []string{}, "Environment variables to pass to the container (K=V)")
	runCmd.Flags().StringVar(&logLevel, "log-level", "progress", "Log level: debug, info, progress, minimal")
	rootCmd.AddCommand(runCmd)
}

func main() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

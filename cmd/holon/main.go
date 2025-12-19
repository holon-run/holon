package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/jolestar/holon/pkg/api/v1"
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
var envVarsList []string

var runCmd = &cobra.Command{
	Use:   "run",
	Short: "Run a Holon execution unit",
	Run: func(cmd *cobra.Command, args []string) {
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

		absWorkspace, _ := filepath.Abs(workspacePath)
		absSpec, _ := filepath.Abs(specPath)
		absOut, _ := filepath.Abs(outDir)
		var absContext string
		if contextPath != "" {
			absContext, _ = filepath.Abs(contextPath)
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

		// 2. Custom Env Vars from CLI (--env K=V) - highest priority
		for _, pair := range envVarsList {
			parts := strings.SplitN(pair, "=", 2)
			if len(parts) == 2 {
				envVars[parts[0]] = parts[1]
			}
		}

		cfg := &docker.ContainerConfig{
			BaseImage:    adapterImage,
			AdapterImage: "holon-adapter-claude",
			Workspace:    absWorkspace,
			SpecPath:     absSpec,
			ContextPath:  absContext,
			OutDir:       absOut,
			Env:          envVars,
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
	runCmd.Flags().StringVarP(&outDir, "out", "o", "./holon-out", "Path to output directory")
	runCmd.Flags().StringSliceVarP(&envVarsList, "env", "e", []string{}, "Environment variables to pass to the container (K=V)")
	rootCmd.AddCommand(runCmd)
}

func main() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

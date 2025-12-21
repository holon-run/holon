package main

import (
	"context"
	"fmt"
	"os"

	"github.com/jolestar/holon/pkg/runtime/docker"
	"github.com/spf13/cobra"
)

var specPath string
var goalStr string
var taskName string
var baseImage string
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
	RunE: func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()
		rt, err := docker.NewRuntime()
		if err != nil {
			return fmt.Errorf("failed to initialize runtime: %w", err)
		}

		runner := NewRunner(rt)
		return runner.Run(ctx, RunnerConfig{
			SpecPath:      specPath,
			GoalStr:       goalStr,
			TaskName:      taskName,
			BaseImage:     baseImage,
			AdapterImage:  adapterImage,
			WorkspacePath: workspacePath,
			ContextPath:   contextPath,
			OutDir:        outDir,
			RoleName:      roleName,
			EnvVarsList:   envVarsList,
			LogLevel:      logLevel,
		})
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
	runCmd.Flags().StringVarP(&baseImage, "image", "i", "golang:1.22", "Docker image for execution (Base toolchain)")
	runCmd.Flags().StringVar(&adapterImage, "adapter-image", "holon-adapter-claude", "Docker image containing the Holon adapter (e.g. holon-adapter-claude)")
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

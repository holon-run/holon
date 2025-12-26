package main

import (
	"context"
	"fmt"
	"os"

	"github.com/holon-run/holon/pkg/config"
	_ "github.com/holon-run/holon/pkg/publisher/github"  // Register GitHub publisher
	_ "github.com/holon-run/holon/pkg/publisher/githubpr" // Register GitHub PR publisher
	_ "github.com/holon-run/holon/pkg/publisher/git"      // Register git publisher
	"github.com/holon-run/holon/pkg/runtime/docker"
	"github.com/spf13/cobra"
)

var specPath string
var goalStr string
var taskName string
var baseImage string
var agentPath string
var agentBundlePath string
var workspacePath string
var contextPath string
var inputPath string
var outDir string
var cleanupMode string
var roleName string
var envVarsList []string
var logLevel string
var mode string
var agentConfigMode string

// resolvedConfig holds the resolved configuration values
type resolvedConfig struct {
	baseImage string
	agent     string
	logLevel  string
}

// resolveWithProjectConfig resolves configuration values with precedence:
// CLI flags > project config > hardcoded defaults
func resolveWithProjectConfig(cmd *cobra.Command, cfg *config.ProjectConfig) resolvedConfig {
	resolved := resolvedConfig{}

	// Resolve base image: CLI > config > default
	// Only use CLI value if flag was explicitly changed
	cliImage := baseImage
	if !cmd.Flags().Changed("image") {
		cliImage = ""
	}
	image, source := cfg.ResolveBaseImage(cliImage, "golang:1.22")
	resolved.baseImage = image
	logConfigResolution("base_image", image, source)

	// Resolve agent: CLI > config > empty (will be handled by agent resolver)
	// Only use CLI value if flag was explicitly changed
	cliAgent := agentPath
	if !cmd.Flags().Changed("agent") && !cmd.Flags().Changed("agent-bundle") {
		cliAgent = ""
	}
	agent, source := cfg.ResolveAgent(cliAgent)
	resolved.agent = agent
	if agent != "" {
		logConfigResolution("agent", agent, source)
	}

	// Resolve log level: CLI > config > default
	// Only use CLI value if flag was explicitly changed
	cliLogLevel := logLevel
	if !cmd.Flags().Changed("log-level") {
		cliLogLevel = ""
	}
	level, source := cfg.ResolveLogLevel(cliLogLevel, "progress")
	resolved.logLevel = level
	logConfigResolution("log_level", level, source)

	return resolved
}

// logConfigResolution logs the resolved configuration value and its source
func logConfigResolution(key, value, source string) {
	fmt.Printf("Config: %s = %q (source: %s)\n", key, value, source)
}

var runCmd = &cobra.Command{
	Use:   "run",
	Short: "Run a Holon agent execution",
	RunE: func(cmd *cobra.Command, args []string) error {
		ctx := context.Background()
		rt, err := docker.NewRuntime()
		if err != nil {
			return fmt.Errorf("failed to initialize runtime: %w", err)
		}

		// Load project config
		projectCfg, err := config.LoadFromCurrentDir()
		if err != nil {
			return fmt.Errorf("failed to load project config: %w", err)
		}

		// Resolve agent bundle
		if agentPath == "" {
			agentPath = agentBundlePath
		}

		// Apply config with precedence: CLI flags > project config > defaults
		resolved := resolveWithProjectConfig(cmd, projectCfg)

		runner := NewRunner(rt)
		return runner.Run(ctx, RunnerConfig{
			SpecPath:        specPath,
			GoalStr:         goalStr,
			TaskName:        taskName,
			BaseImage:       resolved.baseImage,
			AgentBundle:     resolved.agent,
			WorkspacePath:   workspacePath,
			ContextPath:     contextPath,
			InputPath:       inputPath,
			OutDir:          outDir,
			RoleName:        roleName,
			EnvVarsList:     envVarsList,
			LogLevel:        resolved.logLevel,
			Mode:            mode,
			Cleanup:         cleanupMode,
			AgentConfigMode: agentConfigMode,
			GitAuthorName:   projectCfg.GetGitAuthorName(),
			GitAuthorEmail:  projectCfg.GetGitAuthorEmail(),
		})
	},
}

var rootCmd = &cobra.Command{
	Use:   "holon",
	Short: "Holon is a standardized runner for AI-driven software engineering.",
}

func init() {
	runCmd.Flags().StringVarP(&specPath, "spec", "s", "", "Path to holon spec file")
	runCmd.Flags().StringVarP(&goalStr, "goal", "g", "", "Goal description (alternative to --spec)")
	runCmd.Flags().StringVarP(&taskName, "name", "n", "", "Task name (optional, defaults to auto-generated)")
	runCmd.Flags().StringVarP(&baseImage, "image", "i", "golang:1.22", "Docker image for execution (Base toolchain)")
	runCmd.Flags().StringVar(&agentPath, "agent", "", "Agent bundle reference (path to .tar.gz, URL, or alias)")
	runCmd.Flags().StringVar(&agentBundlePath, "agent-bundle", "", "Deprecated: use --agent")
	_ = runCmd.Flags().MarkDeprecated("agent-bundle", "use --agent instead")
	runCmd.Flags().StringVarP(&workspacePath, "workspace", "w", ".", "Path to workspace")
	runCmd.Flags().StringVarP(&contextPath, "context", "c", "", "Path to context directory")
	runCmd.Flags().StringVar(&inputPath, "input", "", "Path to input directory (default: creates temp dir, auto-cleaned)")
	runCmd.Flags().StringVarP(&outDir, "out", "o", "./holon-output", "Path to output directory")
	runCmd.Flags().StringVar(&cleanupMode, "cleanup", "auto", "Cleanup mode: auto (clean temp input), none (keep all), all (clean input+output)")
	runCmd.Flags().StringVarP(&roleName, "role", "r", "", "Role to assume (e.g. developer, reviewer)")
	runCmd.Flags().StringVar(&mode, "mode", "solve", "Execution mode: solve, pr-fix, plan, review")
	runCmd.Flags().StringSliceVarP(&envVarsList, "env", "e", []string{}, "Environment variables to pass to the container (K=V)")
	runCmd.Flags().StringVar(&logLevel, "log-level", "progress", "Log level: debug, info, progress, minimal")
	runCmd.Flags().StringVar(&agentConfigMode, "agent-config-mode", "auto", "Agent config mount mode: auto (mount if ~/.claude exists), yes (always mount, warn if missing), no (never mount)")
	rootCmd.AddCommand(runCmd)
	rootCmd.AddCommand(agentCmd)
	rootCmd.AddCommand(contextCmd)
	rootCmd.AddCommand(publishCmd)
}

func main() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

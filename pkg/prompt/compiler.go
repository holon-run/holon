package prompt

import (
	"bytes"
	"embed"
	"fmt"
	"io/fs"
	"text/template"

	"gopkg.in/yaml.v3"
)

//go:embed all:assets/*
var promptAssets embed.FS

// Config represents the prompt configuration
type Config struct {
	Role         string
	Language     string
	WorkingDir   string
	ContextFiles []string
}

// Manifest represents the structure of manifest.yaml
type Manifest struct {
	Version  string `yaml:"version"`
	Defaults struct {
		Role     string `yaml:"role"`
		Contract string `yaml:"contract"`
	} `yaml:"defaults"`
}

// Compiler handles the assembly of prompts
type Compiler struct {
	assets fs.FS
}

// NewCompiler creates a new prompt compiler
func NewCompiler(assetsPath string) *Compiler {
	// For runtime, we can pass a specific FS or use the embedded one.
	// If assetsPath is empty, use embedded assets.
	// However, to support 'misc/prompts' which is outside pkg/prompt,
	// we should rely on the caller passing the FS or we embed 'misc/prompts' via a slightly different strategy or copy it.
	// But 'embed' cannot reach outside module easily without `//go:embed` directive in the right place.
	// For now, let's assume we copy 'misc/prompts' to 'pkg/prompt/assets' during build or we just put the code that does embedding in the root or pass the FS.

	// Simplification: We will assume assets are embedded in THIS package for now,
	// or we use os.DirFS if running locally and the path exists.

	// Fix: fs.Sub to strip 'assets' prefix so ReadFile("manifest.yaml") works
	sub, err := fs.Sub(promptAssets, "assets")
	if err != nil {
		// Should not happen with embedded assets unless structure is wrong
		panic(fmt.Errorf("failed to subtree assets: %w", err))
	}

	return &Compiler{
		assets: sub,
	}
}

// Global variable to allow setting assets from outside (e.g. tests or custom locations)
// Not thread safe, but acceptable for CLI entry.
// A better pattern would be NewCompiler accepting options.

// NewCompilerFromFS creates a compiler from a given FS (useful for testing or external loading)
func NewCompilerFromFS(assets fs.FS) *Compiler {
	return &Compiler{assets: assets}
}

func (c *Compiler) CompileSystemPrompt(cfg Config) (string, error) {
	// 1. Load Manifest
	manifestData, err := fs.ReadFile(c.assets, "manifest.yaml")
	if err != nil {
		return "", fmt.Errorf("failed to read manifest: %w", err)
	}
	var manifest Manifest
	if err := yaml.Unmarshal(manifestData, &manifest); err != nil {
		return "", fmt.Errorf("failed to parse manifest: %w", err)
	}

	// 2. Resolve Role
	role := cfg.Role
	if role == "" {
		role = manifest.Defaults.Role
	}
	if role == "" {
		role = "coder" // Fallback
	}

	// 3. Load Contract
	contractName := manifest.Defaults.Contract
	if contractName == "" {
		contractName = "v1"
	}
	contractData, err := fs.ReadFile(c.assets, fmt.Sprintf("contract/%s.md", contractName))
	if err != nil {
		return "", fmt.Errorf("failed to read contract %s: %w", contractName, err)
	}

	// 4. Load Role
	roleData, err := fs.ReadFile(c.assets, fmt.Sprintf("roles/%s.md", role))
	if err != nil {
		return "", fmt.Errorf("failed to read role %s: %w", role, err)
	}

	// 5. Combine
	// We combine them with a newline separator
	fullTemplate := string(contractData) + "\n\n" + string(roleData)

	// 6. Template Execution
	tmpl, err := template.New("system").Parse(fullTemplate)
	if err != nil {
		return "", fmt.Errorf("failed to parse template: %w", err)
	}

	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, cfg); err != nil {
		return "", fmt.Errorf("failed to execute template: %w", err)
	}

	return buf.String(), nil
}

// CompileUserPrompt assembles the user's task prompt (Goal + Context Filenames)
func (c *Compiler) CompileUserPrompt(goal string, contextFiles []string) (string, error) {
	var sb bytes.Buffer

	// 1. Goal
	sb.WriteString("### TASK GOAL\n")
	sb.WriteString(goal)
	sb.WriteString("\n")

	// 2. Context Files (Filenames only to keep prompt size manageable)
	if len(contextFiles) > 0 {
		sb.WriteString("\n\n### ADDITIONAL CONTEXT FILES\n")
		sb.WriteString("The following files provide additional context and are available at /holon/input/context/:\n")
		for _, name := range contextFiles {
			sb.WriteString(fmt.Sprintf("- %s\n", name))
		}
	}

	return sb.String(), nil
}

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
	Mode         string
	Role         string
	Language     string
	WorkingDir   string
	ContextFiles []string
}

// Manifest represents the structure of manifest.yaml
type Manifest struct {
	Version  string `yaml:"version"`
	Defaults struct {
		Mode string `yaml:"mode"`
		Role string `yaml:"role"`
		// Contract is kept for backward compatibility with existing manifests and
		// external tools. It is intentionally not used by the current compiler.
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

	// 2. Resolve Mode (with defaults)
	mode := cfg.Mode
	if mode == "" {
		mode = manifest.Defaults.Mode
	}
	if mode == "" {
		mode = "execute" // Fallback
	}

	// 3. Resolve Role (with defaults and alias support)
	role := cfg.Role
	if role == "" {
		role = manifest.Defaults.Role
	}
	if role == "" {
		role = "developer" // Fallback
	}
	// Support "coder" as alias for "developer" (backward compatibility)
	if role == "coder" {
		role = "developer"
	}

	// 4. Load Common Contract (required base layer)
	commonData, err := fs.ReadFile(c.assets, "contracts/common.md")
	if err != nil {
		return "", fmt.Errorf("failed to read common contract: %w", err)
	}

	// 5. Load Base Role (required)
	// Note: We always load the base role first, then layer mode-specific overlays on top
	rolePath := fmt.Sprintf("roles/%s.md", role)
	roleData, err := fs.ReadFile(c.assets, rolePath)
	if err != nil {
		return "", fmt.Errorf("failed to read base role %s (path: %s): %w", role, rolePath, err)
	}

	// 6. Load Mode Contract (optional)
	// Mode contracts are optional - if missing, skip silently
	var modeData []byte
	modeContractPath := fmt.Sprintf("modes/%s/contract.md", mode)
	modeData, err = fs.ReadFile(c.assets, modeContractPath)
	if err != nil {
		// Mode contract is optional - continue without it
		modeData = nil
	}

	// 7. Load Mode Overlay (optional)
	// Note: The spec mentions modes/<mode>/overlay.md as optional layer 4
	var modeOverlayData []byte
	modeOverlayPath := fmt.Sprintf("modes/%s/overlay.md", mode)
	modeOverlayData, err = fs.ReadFile(c.assets, modeOverlayPath)
	if err != nil {
		// Mode overlay is optional - continue without it
		modeOverlayData = nil
	}

	// 8. Load Mode-Specific Role Overlay (optional)
	// Note: This overlays the base role for the specific mode
	var roleOverlayData []byte
	roleOverlayPath := fmt.Sprintf("modes/%s/overlays/%s.md", mode, role)
	roleOverlayData, err = fs.ReadFile(c.assets, roleOverlayPath)
	if err != nil {
		// Role overlay is optional - continue without it
		roleOverlayData = nil
	}

	// 9. Combine layers in order per #148 spec:
	//    common.md + roles/<role>.md + modes/<mode>/contract.md + modes/<mode>/overlay.md + modes/<mode>/overlays/<role>.md
	var fullTemplate string

	// Layer 1: Common Contract (required)
	fullTemplate = string(commonData)

	// Layer 2: Base Role (required)
	fullTemplate += "\n\n" + string(roleData)

	// Layer 3: Mode Contract (optional)
	if modeData != nil {
		fullTemplate += "\n\n" + string(modeData)
	}

	// Layer 4: Mode Overlay (optional)
	if modeOverlayData != nil {
		fullTemplate += "\n\n" + string(modeOverlayData)
	}

	// Layer 5: Mode-Specific Role Overlay (optional)
	if roleOverlayData != nil {
		fullTemplate += "\n\n" + string(roleOverlayData)
	}

	// 10. Template Execution
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

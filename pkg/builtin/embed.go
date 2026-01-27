// Package builtin provides embedded builtin skills for Holon.
// Builtin skills are embedded in the binary using Go's embed package and can be
// loaded by reference (in this repo, builtin skill refs are flat names like
// "github-solve").
//
//go:generate sh -c "rm -rf skills && cp -r ../../skills . && git rev-parse HEAD > skills/.git-commit"
package builtin

import (
	"embed"
	"errors"
	"io/fs"
	"path/filepath"
	"strings"
)

//go:embed skills/*
var builtinSkills embed.FS

// FS returns the embedded skills filesystem, rooted at the skills directory.
// The returned FS provides access to all builtin skills.
func FS() fs.FS {
	// Create a subfilesystem rooted at the skills directory
	sub, err := fs.Sub(builtinSkills, "skills")
	if err != nil {
		// This should never happen if the embed path is correct
		// Return a nil filesystem so callers can detect unavailability
		return nil
	}
	return sub
}

// Has checks if a builtin skill exists at the given reference.
// The reference is the skill directory path within the embedded skills FS
// (in this repo, typically a flat name like "github-solve").
// Returns true if the skill directory exists and contains SKILL.md.
func Has(ref string) bool {
	f := FS()
	if f == nil {
		return false
	}

	// Check if SKILL.md exists in the skill directory
	skillManifestPath := filepath.Join(ref, "SKILL.md")
	_, err := fs.Stat(f, skillManifestPath)
	return err == nil
}

// Load reads the SKILL.md file for a builtin skill.
// The reference is the skill directory path within the embedded skills FS
// (in this repo, typically a flat name like "github-solve").
// Returns the contents of SKILL.md or an error if the skill doesn't exist.
func Load(ref string) ([]byte, error) {
	f := FS()
	if f == nil {
		return nil, errors.New("builtin skills filesystem not available")
	}

	skillManifestPath := filepath.Join(ref, "SKILL.md")
	return fs.ReadFile(f, skillManifestPath)
}

// LoadDir reads the entire skill directory for a builtin skill.
// Returns a map of filename to content for all files in the skill directory.
func LoadDir(ref string) (map[string][]byte, error) {
	f := FS()
	if f == nil {
		return nil, errors.New("builtin skills filesystem not available")
	}

	files := make(map[string][]byte)

	// Walk the skill directory and read all files
	err := fs.WalkDir(f, ref, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		// Skip directories
		if d.IsDir() {
			return nil
		}

		// Read the file
		content, err := fs.ReadFile(f, path)
		if err != nil {
			return err
		}

		// Store with relative path from skill root
		relPath := path
		if ref != "." {
			// Ensure path is long enough to avoid slicing panic
			if len(path) > len(ref)+1 {
				relPath = path[len(ref)+1:]
			} else {
				relPath = filepath.Base(path)
			}
		}
		files[relPath] = content

		return nil
	})

	if err != nil {
		return nil, err
	}

	return files, nil
}

// List returns all builtin skill references.
// A skill reference is returned if the directory contains a SKILL.md file.
func List() ([]string, error) {
	f := FS()
	if f == nil {
		return nil, errors.New("builtin skills filesystem not available")
	}

	var skills []string

	// Walk the entire filesystem
	err := fs.WalkDir(f, ".", func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		// Check if this is a SKILL.md file
		if !d.IsDir() && d.Name() == "SKILL.md" {
			// Get the parent directory as the skill reference
			skillRef := filepath.Dir(path)
			if skillRef == "." {
				skillRef = ""
			}
			skills = append(skills, skillRef)
		}

		return nil
	})

	if err != nil {
		return nil, err
	}

	return skills, nil
}

// GitCommit returns the git commit SHA at which the builtin skills were generated.
// Returns empty string if the commit information is not available.
func GitCommit() string {
	f := FS()
	if f == nil {
		return ""
	}

	content, err := fs.ReadFile(f, ".git-commit")
	if err != nil {
		return ""
	}

	return strings.TrimSpace(string(content))
}

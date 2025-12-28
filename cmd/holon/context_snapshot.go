package main

import (
	"fmt"
	"os"
	"path/filepath"
)

// printContextSnapshot logs the sizes of known context files for debugging.
func printContextSnapshot(contextDir string) {
	files := []string{
		filepath.Join(contextDir, "github", "pr.json"),
		filepath.Join(contextDir, "github", "review_threads.json"),
		filepath.Join(contextDir, "github", "pr.diff"),
		filepath.Join(contextDir, "github", "review.md"),
		filepath.Join(contextDir, "pr-fix.schema.json"),
	}
	fmt.Println("Context snapshot:")
	for _, f := range files {
		info, err := os.Stat(f)
		if err != nil {
			fmt.Printf("  - %s: error: %v\n", f, err)
			continue
		}
		fmt.Printf("  - %s: %d bytes\n", f, info.Size())
	}
}

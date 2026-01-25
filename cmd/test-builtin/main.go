package main

import (
"fmt"
"github.com/holon-run/holon/pkg/skills"
)

func main() {
	// Create resolver with a temporary workspace
	resolver := skills.NewResolver("/tmp/test-workspace")
	
	// Try to resolve "github/solve"
	fmt.Println("Resolving 'github/solve'...")
	resolved, err := resolver.Resolve([]string{"github/solve"}, nil, nil)
	if err != nil {
		fmt.Printf("Error: %v\n", err)
	} else {
		fmt.Printf("Success! Resolved %d skills:\n", len(resolved))
		for _, skill := range resolved {
			fmt.Printf("  - Name: %s, Path: %s, Builtin: %v\n", skill.Name, skill.Path, skill.Builtin)
		}
	}
}

// Command to generate the built-in catalog from a source list
// Usage: go run generate_builtin_catalog.go <source-list.json> > builtin_catalog.json
package main

import (
	"encoding/json"
	"fmt"
	"os"
)

// SourceList represents a list of skill sources
type SourceList struct {
	Name        string        `json:"name"`
	Description string        `json:"description"`
	Sources     []SkillSource `json:"sources"`
}

// SkillSource represents a skill source entry
type SkillSource struct {
	Name        string `json:"name"`
	URL         string `json:"url"`
	Description string `json:"description"`
	SHA256      string `json:"sha256,omitempty"`
	Version     string `json:"version,omitempty"`
}

// Catalog represents the generated catalog
type Catalog struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	Skills      []CatalogEntry `json:"skills"`
}

// CatalogEntry represents a single skill in the catalog
type CatalogEntry struct {
	Name        string `json:"name"`
	URL         string `json:"url"`
	Description string `json:"description"`
	SHA256      string `json:"sha256,omitempty"`
	Version     string `json:"version,omitempty"`
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "Usage: go run generate_builtin_catalog.go <source-list.json>")
		os.Exit(1)
	}

	sourceFile := os.Args[1]
	data, err := os.ReadFile(sourceFile)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error reading source file: %v\n", err)
		os.Exit(1)
	}

	var sourceList SourceList
	if err := json.Unmarshal(data, &sourceList); err != nil {
		fmt.Fprintf(os.Stderr, "Error parsing source list: %v\n", err)
		os.Exit(1)
	}

	// Convert sources to catalog entries
	catalog := Catalog{
		Name:        sourceList.Name,
		Description: sourceList.Description,
		Skills:      make([]CatalogEntry, len(sourceList.Sources)),
	}

	for i, source := range sourceList.Sources {
		catalog.Skills[i] = CatalogEntry{
			Name:        source.Name,
			URL:         source.URL,
			Description: source.Description,
			SHA256:      source.SHA256,
			Version:     source.Version,
		}
	}

	// Output catalog as JSON
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(catalog); err != nil {
		fmt.Fprintf(os.Stderr, "Error encoding catalog: %v\n", err)
		os.Exit(1)
	}
}

package workspace

import (
	"context"
	"fmt"
	"os"

	"github.com/holon-run/holon/pkg/git"
)

// ExistingPreparer uses an existing directory as the workspace without modification
// This is useful when the user wants to use an existing checkout
type ExistingPreparer struct {
	name string
}

// NewExistingPreparer creates a new existing preparer
func NewExistingPreparer() *ExistingPreparer {
	return &ExistingPreparer{
		name: "existing",
	}
}

// Name returns the strategy name
func (p *ExistingPreparer) Name() string {
	return p.name
}

// Validate checks if the request is valid for this preparer
func (p *ExistingPreparer) Validate(req PrepareRequest) error {
	if req.Source == "" {
		return fmt.Errorf("source cannot be empty")
	}
	if req.Dest == "" {
		return fmt.Errorf("dest cannot be empty")
	}
	// Check that dest exists (it should already contain the workspace content)
	if _, err := os.Stat(req.Dest); os.IsNotExist(err) {
		return fmt.Errorf("dest does not exist: %s", req.Dest)
	}
	return nil
}

// Prepare uses an existing directory as the workspace.
// This validates that dest contains a usable git repository or directory tree,
// and typically does not modify content.
// If a Ref is explicitly requested, a git checkout will be performed which modifies
// the working directory content. The Source parameter is used for metadata and origin tracking.
// This strategy is useful for using already-prepared workspaces (e.g., GitHub Actions checkout).
// Note: The workspace manifest is written to the output directory by the runtime, not here.
func (p *ExistingPreparer) Prepare(ctx context.Context, req PrepareRequest) (PrepareResult, error) {
	if err := p.Validate(req); err != nil {
		return PrepareResult{}, fmt.Errorf("validation failed: %w", err)
	}

	result := NewPrepareResult(p.Name())
	result.Source = req.Source
	result.Ref = req.Ref

	// For existing strategy, dest should already contain the workspace content
	actualDest := req.Dest

	// Get git information if available
	client := git.NewClient(actualDest)
	if client.IsRepo(ctx) {
		// Get HEAD SHA
		if headSHA, err := client.GetHeadSHA(ctx); err == nil {
			result.HeadSHA = headSHA
		}

		// Check if shallow
		if isShallow, err := client.IsShallowClone(ctx); err == nil {
			result.IsShallow = isShallow
			result.HasHistory = !isShallow
		}

		// Handle ref checkout if requested
		if req.Ref != "" {
			if err := client.Checkout(ctx, req.Ref); err != nil {
				result.Notes = append(result.Notes, fmt.Sprintf("Warning: failed to checkout ref '%s': %v", req.Ref, err))
			} else {
				// Update HEAD SHA after checkout
				if headSHA, err := client.GetHeadSHA(ctx); err == nil {
					result.HeadSHA = headSHA
				}
			}
		}
	} else {
		result.HasHistory = false
		result.IsShallow = false
	}

	return result, nil
}

// Cleanup for existing strategy is a no-op (we don't own the directory)
func (p *ExistingPreparer) Cleanup(dest string) error {
	// Don't clean up directories we don't own
	return nil
}

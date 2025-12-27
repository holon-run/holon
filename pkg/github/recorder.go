package github

import (
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"testing"

	vcr "gopkg.in/dnaeon/go-vcr.v2/recorder"
	"gopkg.in/dnaeon/go-vcr.v2/cassette"
)

// recorderMode determines whether we're recording or replaying
type recorderMode int

const (
	// modeReplay uses existing fixtures only
	modeReplay recorderMode = iota
	// modeRecord records new fixtures (overwrites existing)
	modeRecord
)

// getRecorderMode determines the recorder mode from environment
func getRecorderMode() recorderMode {
	// Check HOLON_VCR_MODE environment variable
	// If set to "record", record new fixtures
	// Otherwise (default), replay existing fixtures
	if os.Getenv("HOLON_VCR_MODE") == "record" {
		return modeRecord
	}
	return modeReplay
}

// NewRecorder creates a new VCR recorder for testing GitHub API interactions.
//
// The recorder will:
// - In replay mode (default): Use recorded fixtures from testdata/fixtures/
// - In record mode (HOLON_VCR_MODE=record): Record new API interactions to fixtures
//
// Usage:
//
//	rec, err := NewRecorder(t, "fixture_name")
//	if err != nil {
//	    t.Fatal(err)
//	}
//	defer rec.Stop()
//
//	client := NewClient("test-token")
//	client.httpClient = rec.HTTPClient()
//
// IMPORTANT: When recording new fixtures, you must set a real GitHub token:
//
//	HOLON_VCR_MODE=record GITHUB_TOKEN=your_token go test ./pkg/github/...
func NewRecorder(t *testing.T, name string) (*Recorder, error) {
	t.Helper()

	mode := getRecorderMode()

	// Determine fixture path
	// Note: go-vcr automatically adds ".yaml" extension, so don't include it here
	fixturePath := filepath.Join("testdata", "fixtures", name)

	// Determine VCR mode
	var vcrMode vcr.Mode
	if mode == modeReplay {
		vcrMode = vcr.ModeReplaying
	} else {
		vcrMode = vcr.ModeRecording
	}

	// Create recorder
	r, err := vcr.NewAsMode(fixturePath, vcrMode, nil)
	if err != nil {
		// Wrap cassette not found error as os.ErrNotExist for easier error checking
		if errors.Is(err, cassette.ErrCassetteNotFound) {
			return nil, fmt.Errorf("cassette %q not found: %w", fixturePath, os.ErrNotExist)
		}
		return nil, fmt.Errorf("failed to create recorder: %w", err)
	}

	// Filter sensitive headers from saved recordings
	r.AddSaveFilter(func(i *cassette.Interaction) error {
		// Remove authorization header from recorded cassettes
		delete(i.Request.Headers, "Authorization")
		return nil
	})

	return &Recorder{recorder: r, mode: mode}, nil
}

// Recorder wraps go-vcr recorder with Holon-specific functionality
type Recorder struct {
	recorder *vcr.Recorder
	mode     recorderMode
}

// Stop stops the recorder
func (r *Recorder) Stop() error {
	if r.recorder != nil {
		if err := r.recorder.Stop(); err != nil {
			return fmt.Errorf("failed to stop recorder: %w", err)
		}
	}
	return nil
}

// Mode returns the current recorder mode
func (r *Recorder) Mode() recorderMode {
	return r.mode
}

// IsRecording returns true if we're in record mode
func (r *Recorder) IsRecording() bool {
	return r.mode == modeRecord
}

// HTTPClient returns an HTTP client configured to use the recorder
func (r *Recorder) HTTPClient() *http.Client {
	return &http.Client{
		Transport: r.recorder,
	}
}

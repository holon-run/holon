package prompt

import (
	"embed"
	"fmt"
	"io/fs"
	"sync"
)

//go:embed all:assets/*
var promptAssets embed.FS

var assetsOnce sync.Once
var assetsFS fs.FS
var assetsErr error

// AssetsFS returns the embedded prompt assets filesystem rooted at assets/.
func AssetsFS() (fs.FS, error) {
	assetsOnce.Do(func() {
		sub, err := fs.Sub(promptAssets, "assets")
		if err != nil {
			assetsErr = fmt.Errorf("failed to subtree assets: %w", err)
			return
		}
		assetsFS = sub
	})

	return assetsFS, assetsErr
}

// ReadAsset reads an embedded prompt asset by path (relative to assets/).
func ReadAsset(path string) ([]byte, error) {
	assets, err := AssetsFS()
	if err != nil {
		return nil, err
	}

	data, err := fs.ReadFile(assets, path)
	if err != nil {
		return nil, fmt.Errorf("failed to read asset %s: %w", path, err)
	}

	return data, nil
}

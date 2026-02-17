package agenthome

import (
	"embed"
	"fmt"
	"io/fs"
	"sync"
)

//go:embed all:assets/*
var embeddedAssets embed.FS

var assetsOnce sync.Once
var assetsFS fs.FS
var assetsErr error

// AssetsFS returns the embedded agenthome assets filesystem rooted at assets/.
func AssetsFS() (fs.FS, error) {
	assetsOnce.Do(func() {
		sub, err := fs.Sub(embeddedAssets, "assets")
		if err != nil {
			assetsErr = fmt.Errorf("failed to subtree assets: %w", err)
			return
		}
		assetsFS = sub
	})
	return assetsFS, assetsErr
}

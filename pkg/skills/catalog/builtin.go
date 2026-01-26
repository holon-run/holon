package catalog

import (
	_ "embed"
)

// BuiltinCatalogJSON is the embedded built-in catalog
//
//go:embed builtin_catalog.json
var BuiltinCatalogJSON []byte

// BuiltinCatalog returns the built-in catalog adapter
func BuiltinCatalog() (*BuiltinCatalogAdapter, error) {
	return NewBuiltinCatalogAdapter(BuiltinCatalogJSON)
}

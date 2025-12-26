# Holon Homebrew Tap

This is the official Homebrew tap for [Holon](https://github.com/holon-run/holon), providing prebuilt binaries for macOS and Linux.

## Installation

### Install Holon

```bash
brew install holon-run/tap/holon
```

This will install the `holon` binary from the latest GitHub release.

### Upgrade Holon

```bash
brew update && brew upgrade holon-run/tap/holon
```

### Uninstall Holon

```bash
brew uninstall holon-run/tap/holon
```

## Formula

The formula (`Formula/holon.rb`) is automatically updated during the Holon release process. Each release updates the URLs and SHA256 checksums for the binaries.

## Supported Platforms

- macOS (Intel): `darwin-amd64`
- macOS (Apple Silicon): `darwin-arm64`
- Linux (x86_64): `linux-amd64`

## Development

### Testing Formula Changes Locally

If you want to test changes to the formula locally:

```bash
brew install --build-from-source ./Formula/holon.rb
```

### Updating the Formula

The formula is automatically updated by the release workflow. Manual updates should not be necessary.

## License

See [Holon LICENSE](https://github.com/holon-run/holon/blob/main/LICENSE)

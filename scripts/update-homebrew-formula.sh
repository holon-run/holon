#!/bin/bash
set -euo pipefail

# Update Homebrew formula with new release assets
# This script is called during the GitHub release workflow

VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "Usage: $0 <version>"
  echo "Example: $0 v0.1.0"
  exit 1
fi

# Strip 'v' prefix if present
VERSION="${VERSION#v}"

echo "Updating Homebrew formula for version ${VERSION}..."

# Verify that all required binary files exist
for file in "bin/holon-darwin-amd64.tar.gz" "bin/holon-darwin-arm64.tar.gz" "bin/holon-linux-amd64.tar.gz"; do
  if [ ! -f "$file" ]; then
    echo "Error: Required file not found: $file"
    exit 1
  fi
done

# Calculate checksums for each binary
INTEL_MAC_SHA256=$(sha256sum "bin/holon-darwin-amd64.tar.gz" | awk '{print $1}')
ARM_MAC_SHA256=$(sha256sum "bin/holon-darwin-arm64.tar.gz" | awk '{print $1}')
LINUX_SHA256=$(sha256sum "bin/holon-linux-amd64.tar.gz" | awk '{print $1}')

echo "Checksums:"
echo "  macOS (Intel):  ${INTEL_MAC_SHA256}"
echo "  macOS (ARM):    ${ARM_MAC_SHA256}"
echo "  Linux (amd64):  ${LINUX_SHA256}"

# Ensure the Homebrew tap formula directory exists
mkdir -p homebrew-tap/Formula

# Create the updated formula
cat > homebrew-tap/Formula/holon.rb << EOF
# typed: strict
# frozen_string_literal: true

class Holon < Formula
  desc "Standardized runner for AI-driven software engineering"
  homepage "https://github.com/holon-run/holon"
  license "MIT"

  version "${VERSION}"

  # Auto-update configuration
  livecheck do
    url "https://github.com/holon-run/holon/releases/latest"
    strategy :github_latest
  end

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/holon-run/holon/releases/download/v${VERSION}/holon-darwin-amd64.tar.gz"
      sha256 "${INTEL_MAC_SHA256}"

      def install
        bin.install "holon"
      end
    else
      url "https://github.com/holon-run/holon/releases/download/v${VERSION}/holon-darwin-arm64.tar.gz"
      sha256 "${ARM_MAC_SHA256}"

      def install
        bin.install "holon"
      end
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/holon-run/holon/releases/download/v${VERSION}/holon-linux-amd64.tar.gz"
      sha256 "${LINUX_SHA256}"

      def install
        bin.install "holon"
      end
    end
  end

  test do
    version_output = shell_output("\#{bin}/holon version")
    assert_match "holon version", version_output
    assert_match "commit:", version_output
    assert_match "built at:", version_output
  end
end
EOF

echo "Homebrew formula updated successfully!"
echo "Formula location: homebrew-tap/Formula/holon.rb"

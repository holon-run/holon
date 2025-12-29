# typed: strict
# frozen_string_literal: true

class Holon < Formula
  desc "Standardized runner for AI-driven software engineering"
  homepage "https://github.com/holon-run/holon"
  license "MIT"

  version "0.2.0"

  # Auto-update configuration
  livecheck do
    url "https://github.com/holon-run/holon/releases/latest"
    strategy :github_latest
  end

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/holon-run/holon/releases/download/v0.2.0/holon-darwin-amd64.tar.gz"
      sha256 "b25399bd8eeed8ea1d40742419482df0ca635aa4f80701a6bcd3ed945acc61bc"

      def install
        bin.install "holon"
      end
    else
      url "https://github.com/holon-run/holon/releases/download/v0.2.0/holon-darwin-arm64.tar.gz"
      sha256 "5bc5c501b629f54f88c25f4af1056bef30e569ae9ca0ad8b46abc1895a5733d1"

      def install
        bin.install "holon"
      end
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/holon-run/holon/releases/download/v0.2.0/holon-linux-amd64.tar.gz"
      sha256 "12524c933363a7178c2caa56b34bf717516e5b897f9c6e9d60241c8d9d4b87f0"

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

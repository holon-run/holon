# typed: strict
# frozen_string_literal: true

class Holon < Formula
  desc "Standardized runner for AI-driven software engineering"
  homepage "https://github.com/holon-run/holon"
  license "MIT"

  version "0.1.0"

  # Auto-update configuration
  livecheck do
    url "https://github.com/holon-run/holon/releases/latest"
    strategy :github_latest
  end

  on_macos do
    if Hardware::CPU.intel?
      url "https://github.com/holon-run/holon/releases/download/v0.1.0/holon-darwin-amd64.tar.gz"
      sha256 "7e47f504d07d80d6cd121b9c3e9dec47a240412c29476d8983c0270ce54fb863"

      def install
        bin.install "holon-darwin-amd64" => "holon"
      end
    else
      url "https://github.com/holon-run/holon/releases/download/v0.1.0/holon-darwin-arm64.tar.gz"
      sha256 "dc09104ef2eac1574179bfa8381213c621862aedcd9cffcbd777160b3121e0f8"

      def install
        bin.install "holon-darwin-arm64" => "holon"
      end
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/holon-run/holon/releases/download/v0.1.0/holon-linux-amd64.tar.gz"
      sha256 "da9929d8d99e362ec6631e0c5e295897944b389c7e4748c1119582cb50602bf6"

      def install
        bin.install "holon-linux-amd64" => "holon"
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

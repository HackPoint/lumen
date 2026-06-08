class Lumen < Formula
  desc "Terminal dashboard for Claude Code — live context fill, cost, and optimizer savings"
  homepage "https://github.com/speedata/lumen"
  version "0.1.0"

  on_macos do
    on_arm do
      url "https://github.com/speedata/lumen/releases/download/v#{version}/lumen-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_SHA256_AARCH64"
    end
  end

  def install
    bin.install "lumen"
  end

  test do
    # --version exits 0 and prints the version string
    assert_match version.to_s, shell_output("#{bin}/lumen --version")
  end
end

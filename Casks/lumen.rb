cask "lumen" do
  version "1.1.0"
  sha256 "2eb328f4936b532f756cfc60db18fb782dab7ed124716595bc3bf2b7887d6818"

  url "https://github.com/HackPoint/lumen/releases/download/v#{version}/Lumen_#{version}_aarch64.dmg"
  name "Lumen"
  desc "macOS menu-bar app for Claude Code — live context gauge, cost, and optimizer"
  homepage "https://github.com/HackPoint/lumen"

  depends_on macos: ">= :ventura"
  depends_on arch: :arm64

  app "Lumen.app"

  # Homebrew clears quarantine for cask-installed apps automatically.
  # This explicit postflight is belt-and-suspenders for the un-notarized build.
  postflight do
    system_command "/usr/bin/xattr",
      args: ["-dr", "com.apple.quarantine", "#{appdir}/Lumen.app"],
      sudo: false
  end

  zap trash: [
    "~/Library/Application Support/io.speedata.lumen",
    "~/Library/Caches/io.speedata.lumen",
    "~/Library/Logs/io.speedata.lumen",
    "~/Library/WebKit/io.speedata.lumen",
  ]
end

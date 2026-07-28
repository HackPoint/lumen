cask "lumen" do
  version "1.1.2"
  sha256 "2eb328f4936b532f756cfc60db18fb782dab7ed124716595bc3bf2b7887d6818"

  url "https://github.com/HackPoint/lumen/releases/download/v#{version}/Lumen_#{version}_aarch64.dmg"
  name "Lumen"
  desc "macOS menu-bar app for Claude Code — live context gauge, cost, and optimizer"
  homepage "https://github.com/HackPoint/lumen"

  # `macos: :ventura` already means "Ventura or newer" for a cask. The string
  # comparison form Homebrew deprecated (">= :ventura") warned on every
  # `brew info`/`install` and is slated to become an error.
  depends_on macos: :ventura
  depends_on arch: :arm64

  app "Lumen.app"

  # Homebrew clears quarantine for cask-installed apps automatically.
  # This explicit postflight is belt-and-suspenders for the un-notarized build.
  postflight do
    system_command "/usr/bin/xattr",
      args: ["-dr", "com.apple.quarantine", "#{appdir}/Lumen.app"],
      sudo: false

    # Launch straight after install so the menu-bar icon is simply there. Lumen
    # is a tray app with both windows hidden at startup, so this shows an icon
    # rather than stealing focus with a window. On first run it registers a login
    # item, so this is the only time the launch needs prompting.
    #
    # `-g` keeps the app in the background: without it, `open` activates Lumen and
    # pulls focus out of the terminal the user is still watching brew run in.
    system_command "/usr/bin/open",
      args: ["-g", "-a", "#{appdir}/Lumen.app"],
      sudo: false
  end

  # Quitting first: the app was launched above and by its login item, so an
  # uninstall that left it running would keep a tray icon for a deleted app.
  uninstall quit:       "io.speedata.lumen",
            launchctl:  "io.speedata.lumen"

  zap trash: [
    "~/Library/Application Support/io.speedata.lumen",
    "~/Library/Caches/io.speedata.lumen",
    "~/Library/Logs/io.speedata.lumen",
    "~/Library/WebKit/io.speedata.lumen",
    # The login item the app registers on first run.
    "~/Library/LaunchAgents/io.speedata.lumen.plist",
  ]
end

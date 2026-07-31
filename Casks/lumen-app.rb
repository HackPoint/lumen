# Token is "lumen-app", not "lumen": homebrew/cask already ships an unrelated
# `lumen` (anishathalye/lumen, a screen-brightness tool). With the same token, an
# unqualified `brew install --cask lumen` installed THAT app instead of this one,
# and `brew upgrade --cask lumen` offered to replace this one with it. Pairs with
# the `lumen-cli` formula; the installed app and the `lumen` command are unchanged.
cask "lumen-app" do
  version "1.5.0"
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
  # `launchctl` takes the agent's Label, not the bundle id. The autostart plugin writes
  # ~/Library/LaunchAgents/Lumen.plist with Label "Lumen", so unloading
  # "io.speedata.lumen" matched nothing and every uninstall left the login item behind,
  # still trying to launch a deleted app at each boot. Reported in issue #5.
  uninstall quit:       "io.speedata.lumen",
            launchctl:  "Lumen"

  zap trash: [
    "~/Library/Application Support/io.speedata.lumen",
    "~/Library/Caches/io.speedata.lumen",
    "~/Library/Logs/io.speedata.lumen",
    "~/Library/WebKit/io.speedata.lumen",
    # The login item the app registers on first run.
    # The file the plugin actually writes. The bundle-id name below is kept for
    # installs made before the agent was renamed, since zap should clean those too.
    "~/Library/LaunchAgents/Lumen.plist",
    "~/Library/LaunchAgents/io.speedata.lumen.plist",
  ]
end

#!/usr/bin/env bash
# Launch or quit the installed Lumen the way a user's session does, on the runner the upgrade
# workflow is on. usage: upgrade-app.sh launch|quit
set -u
case "$1:$RUNNER_OS" in
  launch:macOS)
    open -g -a /Applications/Lumen.app ;;
  launch:Windows)
    powershell -NoProfile -Command "Start-Process -FilePath (Join-Path \$env:LOCALAPPDATA 'Lumen\\Lumen.exe')" ;;
  launch:Linux)
    # The deb's own binary under /usr/bin, not one of the sidecars beside it.
    app=$(dpkg -L lumen | grep -E '^/usr/bin/' | grep -v -E 'lumen-(daemon|mcp|tok|cli)$' | head -1)
    echo "launching $app"
    nohup xvfb-run -a dbus-launch "$app" >> "$RUNNER_TEMP/app.out" 2>&1 & ;;
  quit:macOS)
    osascript -e 'tell application id "io.hackpoint.lumen" to quit' || true ;;
  quit:Windows)
    powershell -NoProfile -Command "Get-Process | Where-Object { \$_.Name -match '^(Lumen|lumen-daemon)$' } | Stop-Process -Force" || true ;;
  quit:Linux)
    pkill -f '^/usr/bin/Lumen' || true
    pkill -f 'lumen-daemon' || true ;;
  *)
    echo "usage: upgrade-app.sh launch|quit (RUNNER_OS=$RUNNER_OS)" >&2; exit 2 ;;
esac
# A quit lets the daemon and the app's last writes finish before the next step reads anything.
[ "$1" = quit ] && sleep 5
exit 0

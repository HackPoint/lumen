#!/usr/bin/env bash
# fix-quarantine.sh — remove the macOS quarantine attribute from Lumen.app
#
# macOS blocks unsigned apps with "Lumen is damaged and can't be opened."
# This is NOT actual damage — it's a Gatekeeper check for un-notarized apps.
# Running this once clears the flag; Lumen then opens normally.
#
# Usage:
#   bash scripts/fix-quarantine.sh
#   or one-liner (reads the script before running):
#   curl -fsSL https://raw.githubusercontent.com/HackPoint/lumen/main/scripts/fix-quarantine.sh | bash

set -euo pipefail

APP="/Applications/Lumen.app"

if [ ! -d "$APP" ]; then
  echo "Error: $APP not found. Drag Lumen.app to /Applications first." >&2
  exit 1
fi

echo "Removing quarantine attribute from $APP ..."
xattr -dr com.apple.quarantine "$APP"
echo "Done. Open Lumen normally — the 'damaged' dialog will not reappear."

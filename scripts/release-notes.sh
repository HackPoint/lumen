#!/usr/bin/env bash
# release-notes.sh <version> [changelog]
#
# Prints the CHANGELOG block for <version>: the text under "## [<version>]" up to the next
# "## [" line, without that header and without the blank lines around it. It is the only
# source of a release's notes. A version with no block, or a block with nothing in it, is an
# error and nothing is printed.
#
# Exit status: 0 notes printed, 1 no usable block (or the changelog cannot be read),
# 2 bad arguments.
set -u

if [ $# -lt 1 ] || [ $# -gt 2 ]; then
  echo "usage: release-notes.sh <version> [changelog]" >&2
  exit 2
fi
version=$1
changelog=${2:-$(dirname "$0")/../CHANGELOG.md}

case $version in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "release-notes: \"$version\" is not a version (expected N.N.N or N.N.N-suffix)" >&2
    exit 2
    ;;
esac

if [ ! -r "$changelog" ]; then
  echo "release-notes: cannot read $changelog" >&2
  exit 1
fi

notes=$(awk -v v="$version" '
  index($0, "## [" v "]") == 1 { found = 1; next }
  found && index($0, "## [") == 1 { exit }
  found {
    buf[++n] = $0
    if ($0 ~ /[^ \t\r]/) { if (!first) first = n; last = n }
  }
  END {
    if (!found) exit 3
    if (!last) exit 4
    for (i = first; i <= last; i++) print buf[i]
  }
' "$changelog")
status=$?

case $status in
  0) printf '%s\n' "$notes" ;;
  3)
    echo "release-notes: no \"## [$version]\" block in $changelog" >&2
    exit 1
    ;;
  4)
    echo "release-notes: the \"## [$version]\" block in $changelog is empty" >&2
    exit 1
    ;;
  *)
    echo "release-notes: reading $changelog failed (awk exit $status)" >&2
    exit 1
    ;;
esac

#!/bin/sh
# probe-hook-env.sh — what a Claude Code hook finds on this machine, unedited.
#
# Claude Code runs a hook's command with /bin/sh -c on macOS and Linux, and with Git
# Bash's bash -c on Windows. Through 1.5.1 Lumen's hooks were bash scripts that ran
# python3, stat, wc, mktemp and date -u; from 1.6.0 they run `lumen-mcp hook`, and the
# shell left around it needs bash, cat and date. This runs each of those in the form
# the scripts used and prints the command, everything it wrote and its exit status, so
# a per-platform table is built from what each platform answered.
#
# POSIX sh, so that on Linux it runs in the shell hooks get:
#   /bin/sh scripts/probe-hook-env.sh
# On Windows, in Git Bash, as Claude Code's hooks are:
#   bash scripts/probe-hook-env.sh
#
# It writes only inside one directory of its own under the temp dir, removed at exit.

# Each probe is a string printed as written and then eval'd: the single quotes are meant,
# and $F is used only inside them.
# shellcheck disable=SC2016,SC2034
T="${TMPDIR:-/tmp}/lumen-probe.$$"
mkdir "$T" || exit 1
trap 'rm -rf "$T"' EXIT
# mktemp is among the things probed, and where it puts a file is part of its answer.
# Each one it makes is removed straight after; this keeps those that honour TMPDIR here.
export TMPDIR="$T"
printf 'one\ntwo\nthree\n' >"$T/f"
F="$T/f"

# The command, its stdout and stderr as they came, and its status.
probe() {
    printf '$ %s\n' "$1"
    out=$(eval "$1" 2>&1)
    rc=$?
    [ -n "$out" ] && printf '%s\n' "$out"
    printf '[exit %s]\n\n' "$rc"
}

have() { command -v "$1" >/dev/null 2>&1; }

section() { printf '## %s\n\n' "$1"; }

section "Platform"
probe 'uname -a'
if have sw_vers; then probe 'sw_vers'; fi
if [ -r /etc/os-release ]; then probe "grep -E '^(NAME|VERSION)=' /etc/os-release"; fi
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) WINDOWS=1 ;;
    *) WINDOWS=0 ;;
esac
if [ "$WINDOWS" = 1 ]; then probe 'cmd //c ver'; fi

section "The shell that runs a hook command"
if [ "$WINDOWS" = 1 ]; then
    # Claude Code looks for Git Bash: CLAUDE_CODE_GIT_BASH_PATH, then Git's default
    # install directories, then next to the git on PATH. It never runs `bash` by name,
    # which would find WSL's bash.exe in System32 first on a machine that has WSL.
    probe 'printf "%s\n" "${CLAUDE_CODE_GIT_BASH_PATH:-<unset>}"'
    probe 'ls -l "/c/Program Files/Git/bin/bash.exe"'
    probe 'cygpath -w "$(command -v git)"'
    probe 'cmd //c where bash'
else
    probe 'ls -l /bin/sh'
    if [ -e /private/var/select/sh ]; then probe 'ls -l /private/var/select/sh'; fi
    probe '/bin/sh -c '\''echo "BASH_VERSION=${BASH_VERSION:-<unset>}"'\'''
fi
probe 'command -v bash'
probe 'bash --version | head -n 1'
probe 'command -v env'

section "What the 1.6.0 shims run besides lumen-mcp"
probe 'command -v cat'
probe 'command -v date'
probe 'date -u +"%Y-%m-%dT%H:%M:%SZ"'
probe 'command -v lumen-mcp'

section "What the 1.5.1 scripts ran: Python"
for p in python3 python py; do
    probe "command -v $p"
done
probe 'python3 --version'
probe 'python3 -c "import sys; print(sys.version_info[0])"'
probe 'python --version'
probe 'python -c "import sys; print(sys.version_info[0])"'
probe 'py -3 --version'
if [ "$WINDOWS" = 1 ]; then
    # The App Execution Aliases: a python3 here with no Python installed is the
    # Microsoft Store stub, which `command -v` finds and which cannot run a script.
    probe 'ls -l "$LOCALAPPDATA/Microsoft/WindowsApps" | grep -i python'
fi

section "What the 1.5.1 scripts ran: the rest, in the forms they used"
probe 'command -v stat'
probe 'stat -c %Y "$F"'
probe 'stat -f %m "$F"'
probe 'stat --version | head -n 1'
probe 'command -v wc'
probe 'printf "[%s]\n" "$(wc -l < "$F")"'
probe "wc -c < \"\$F\" | awk '{print int(\$1/4)}'"
probe 'command -v mktemp'
probe 'm=$(mktemp "${TMPDIR:-/tmp}/lumen_bash_out.XXXXXX") && echo "$m" && rm -f "$m"'
probe 'm=$(mktemp -t lumen_bash_out) && echo "$m" && rm -f "$m"'
probe 'date --version | head -n 1'
probe "printf 'Foo.RS' | tr '[:upper:]' '[:lower:]'"
probe "printf 'a\tb\tc' | cut -f2"
probe 'grep -Fxq -- "two" "$F"'

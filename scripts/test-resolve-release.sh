#!/usr/bin/env bash
# Tests for the release workflow's `resolve` job, the step that turns a tag into the one commit
# every build job checks out. The step's script is read out of .github/workflows/release.yml
# and run in an empty workspace against a throwaway git remote on which main has moved past
# every tag, so what is tested is the workflow itself and not a copy of it.
# Run: bash scripts/test-resolve-release.sh
set -u

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

failed=0
pass() { printf 'ok   - %s\n' "$1"; }
fail() {
  printf 'FAIL - %s\n' "$1"
  [ -n "${2:-}" ] && printf '         %s\n' "$2"
  failed=$((failed + 1))
}

# Nothing from the developer's git or GitHub environment may reach the steps or the fixtures.
for v in GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES \
  GIT_COMMON_DIR GIT_NAMESPACE GIT_CONFIG GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM GIT_CONFIG_COUNT \
  GIT_CONFIG_PARAMETERS GIT_ASKPASS GIT_SSH_COMMAND GH_TOKEN GITHUB_TOKEN LUMEN_RELEASE_REMOTE; do
  unset "$v"
done
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config" GIT_CONFIG_NOSYSTEM=1
mkdir -p "$HOME"

must() { "$@" || { printf 'setup failed: %s\n' "$*" >&2; exit 2; }; }
g() { git -c user.name=fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false -c tag.gpgSign=false "$@"; }

# ── the remote: tags on one commit, main one commit further on ───────────────

SERVER="$WORK/server"
REMOTE="$SERVER/example/lumen.git"
SRC="$WORK/src"
must mkdir -p "$SERVER/example"
must g init -q "$SRC"
must g -C "$SRC" symbolic-ref HEAD refs/heads/main
printf 'the release\n' >"$SRC/file"
must g -C "$SRC" add file
must g -C "$SRC" commit -q -m release
RELEASE_SHA=$(g -C "$SRC" rev-parse HEAD)
TREE_SHA=$(g -C "$SRC" rev-parse 'HEAD^{tree}')
BLOB_SHA=$(g -C "$SRC" rev-parse HEAD:file)
must g -C "$SRC" tag -a v1.0.0 -m v1.0.0
must g -C "$SRC" tag v1.1.0
must g -C "$SRC" tag -a v1.2.0-rc.1 -m rc
must g -C "$SRC" tag v1.3.0+build.5
must g -C "$SRC" tag -a v1.4.0 -m nested v1.0.0 2>/dev/null
must g -C "$SRC" tag -a v2.0.0 -m tree "$TREE_SHA"
must g -C "$SRC" tag v2.1.0 "$TREE_SHA"
must g -C "$SRC" tag v2.2.0 "$BLOB_SHA"
must g -C "$SRC" tag refs/tags/v7.7.7
printf 'main moved on\n' >"$SRC/file"
must g -C "$SRC" commit -q -a -m "after the release"
MAIN_SHA=$(g -C "$SRC" rev-parse HEAD)
must g -C "$SRC" branch v3.0.0
must g init -q --bare "$REMOTE"
must g -C "$SRC" push -q "$REMOTE" main v3.0.0 --tags

TAG_OBJECT=$(g -C "$SRC" rev-parse v1.0.0)
if [ "$RELEASE_SHA" = "$MAIN_SHA" ] || [ "$TAG_OBJECT" = "$RELEASE_SHA" ]; then
  echo "setup failed: the fixture needs main past the tags and an annotated tag object" >&2
  exit 2
fi

# ── reading a step out of the workflow, and running it as the runner would ──

workflow="$ROOT/.github/workflows/release.yml"
wf="$WORK/release.yml"
tr -d '\r' <"$workflow" >"$wf"

cat >"$WORK/step.awk" <<'EOF'
function leave() {
  if (me) {
    if (mode == "env") printf "%s", envbuf
    else printf "%s", runbuf
    found = 1
  }
  me = 0; envbuf = ""; runbuf = ""; section = ""
}
function key(l) {
  if (l == "        id: " id) { me = 1; section = ""; return }
  if (l ~ /^        env:[ \t]*$/) { section = "env"; return }
  if (l ~ /^        run:[ \t]*[|]-?[ \t]*$/) { section = "run"; return }
  if (l ~ /^        [A-Za-z_-]+:/) { section = ""; return }
  if (section == "env" && l ~ /^          [^ ]/) { sub(/^          /, "", l); envbuf = envbuf l "\n"; return }
  if (section == "run") {
    if (l ~ /^[ \t]*$/) { runbuf = runbuf "\n"; return }
    if (l ~ /^          /) { sub(/^          /, "", l); runbuf = runbuf l "\n"; return }
    section = ""
  }
}
$0 == "  " job ":" { leave(); on = 1; next }
on && /^  [A-Za-z0-9_-]+:[ \t]*$/ { leave(); on = 0; next }
on && /^      - / { leave(); key("        " substr($0, 9)); next }
on { key($0) }
END { leave(); if (!found) exit 3 }
EOF

# The runner pastes each ${{ expression }} into the text before the shell sees it. An
# expression with no CTX_ value is an error, so a new one cannot slip past unmodelled.
cat >"$WORK/subst.awk" <<'EOF'
function ev(x,   n) {
  sub(/^[ \t]+/, "", x); sub(/[ \t]+$/, "", x)
  n = x; gsub(/[^A-Za-z0-9]/, "_", n); n = "CTX_" toupper(n)
  if (n in ENVIRON) return ENVIRON[n]
  unmodelled = unmodelled " " x
  return ""
}
{
  out = ""; rest = $0
  while ((i = index(rest, "${{")) > 0) {
    out = out substr(rest, 1, i - 1)
    rest = substr(rest, i + 3)
    j = index(rest, "}}")
    if (j == 0) { unmodelled = unmodelled " (unterminated)"; break }
    out = out ev(substr(rest, 1, j - 1))
    rest = substr(rest, j + 2)
  }
  print out rest
}
END { if (unmodelled != "") { print "expressions the harness does not model:" unmodelled > "/dev/stderr"; exit 4 } }
EOF

step_part() { awk -v job="$1" -v id="$2" -v mode="$3" -f "$WORK/step.awk" "$wf"; }

reset_ctx() {
  EVENT=workflow_dispatch REF_NAME=main INPUT_TAG="" TOKEN=""
  SERVER_URL="file://$SERVER" STEP_REMOTE=$REMOTE STEP_PATH=$PATH
  NEEDS=()
}

subst() {
  env "CTX_GITHUB_EVENT_NAME=$EVENT" "CTX_GITHUB_SHA=$MAIN_SHA" "CTX_GITHUB_REF_NAME=$REF_NAME" \
    "CTX_GITHUB_REPOSITORY=example/lumen" "CTX_GITHUB_SERVER_URL=$SERVER_URL" \
    "CTX_INPUTS_TAG=$INPUT_TAG" "CTX_SECRETS_GITHUB_TOKEN=$TOKEN" \
    ${NEEDS[@]+"${NEEDS[@]}"} awk -f "$WORK/subst.awk"
}

case_n=0
status=0
case_dir=""

# run_step JOB ID: the step as the runner runs it (`bash -e`), in an empty workspace, with the
# context from reset_ctx and the globals set after it. Leaves status and the files in $case_dir.
run_step() {
  local job=$1 id=$2 line name val
  case_n=$((case_n + 1))
  case_dir="$WORK/case-$case_n"
  mkdir -p "$case_dir/ws" "$case_dir/home" "$case_dir/tmp"
  : >"$case_dir/output"
  : >"$case_dir/summary"
  : >"$case_dir/stdout"
  : >"$case_dir/stderr"

  if ! step_part "$job" "$id" run >"$case_dir/raw.sh" 2>>"$case_dir/stderr" || [ ! -s "$case_dir/raw.sh" ]; then
    echo "release.yml has no step with id $id and a run script in the $job job" >>"$case_dir/stderr"
    status=97
    return
  fi
  if ! subst <"$case_dir/raw.sh" >"$case_dir/step.sh" 2>>"$case_dir/stderr"; then
    status=96
    return
  fi

  local envs=(
    "PATH=$STEP_PATH" "HOME=$case_dir/home" "XDG_CONFIG_HOME=$case_dir/home/.config"
    "TMPDIR=$case_dir/tmp" "RUNNER_TEMP=$case_dir/tmp" "GIT_CONFIG_NOSYSTEM=1"
    "GITHUB_EVENT_NAME=$EVENT" "GITHUB_REF_NAME=$REF_NAME" "GITHUB_SHA=$MAIN_SHA"
    "GITHUB_REPOSITORY=example/lumen" "GITHUB_SERVER_URL=$SERVER_URL"
    "GITHUB_OUTPUT=$case_dir/output" "GITHUB_STEP_SUMMARY=$case_dir/summary"
    "GITHUB_WORKSPACE=$case_dir/ws"
  )
  [ -n "$STEP_REMOTE" ] && envs+=("LUMEN_RELEASE_REMOTE=$STEP_REMOTE")

  step_part "$job" "$id" env >"$case_dir/step.env" 2>>"$case_dir/stderr"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    name=${line%%:*}
    val=${line#*: }
    case $val in
      \"*\" | \'*\') val=${val#?}; val=${val%?} ;;
    esac
    val=$(printf '%s' "$val" | subst 2>>"$case_dir/stderr"; printf x)
    val=${val%x}
    val=${val%?}
    envs+=("$name=$val")
  done <"$case_dir/step.env"

  (cd "$case_dir/ws" && env "${envs[@]}" bash --noprofile --norc -e "$case_dir/step.sh") \
    >"$case_dir/stdout" 2>>"$case_dir/stderr"
  status=$?
}

value_of() { awk -F= -v k="$1" '$1 == k { sub(/^[^=]*=/, ""); print; exit }' "$case_dir/output"; }
said() { cat "$case_dir/stdout" "$case_dir/stderr"; }
brief() { said | head -c 240 | tr '\n' ' '; }

resolved() {
  local name=$1 tag=$2 version=$3 sha=$4 why="" got lines
  [ "$status" -eq 0 ] || why="$why exit $status;"
  got=$(value_of build_ref)
  if [ "$got" != "$sha" ]; then
    why="$why build_ref is ${got:-empty}"
    [ "$got" = "$MAIN_SHA" ] && why="$why (main's HEAD, not the tag's commit)"
    why="$why;"
  fi
  [ "$(value_of tag)" = "$tag" ] || why="$why tag is $(value_of tag);"
  [ "$(value_of version)" = "$version" ] || why="$why version is $(value_of version);"
  lines=$(wc -l <"$case_dir/output" | tr -d ' ')
  [ "$lines" -eq 3 ] || why="$why $lines output lines;"
  [ ! -e "$case_dir/ws/pwned" ] || why="$why an injected command ran;"
  if [ -z "$why" ]; then
    pass "$name"
  else
    fail "$name" "$why $(brief)"
  fi
}

refused() {
  local name=$1 fragment=$2 why=""
  if [ "$status" -eq 0 ]; then
    why="$why exit 0, accepted (build_ref ${got:-$(value_of build_ref)});"
  fi
  [ ! -s "$case_dir/output" ] || why="$why wrote outputs: $(tr '\n' ' ' <"$case_dir/output");"
  if ! said | grep -qF -- "$fragment"; then
    why="$why no \"$fragment\" in the message;"
  fi
  [ ! -e "$case_dir/ws/pwned" ] || why="$why an injected command ran;"
  if [ -z "$why" ]; then
    pass "$name"
  else
    fail "$name" "$why $(brief)"
  fi
}

# ── what a dispatch builds ────────────────────────────────────────────────────

reset_ctx
INPUT_TAG=v1.0.0
run_step resolve r
resolved "a dispatch of an annotated tag builds the commit it points at, not main's HEAD" v1.0.0 1.0.0 "$RELEASE_SHA"

reset_ctx
INPUT_TAG=v1.1.0
run_step resolve r
resolved "a dispatch of a lightweight tag builds the commit it points at" v1.1.0 1.1.0 "$RELEASE_SHA"

reset_ctx
INPUT_TAG=v1.2.0-rc.1
run_step resolve r
resolved "a prerelease tag resolves, and keeps its suffix in the version" v1.2.0-rc.1 1.2.0-rc.1 "$RELEASE_SHA"

reset_ctx
INPUT_TAG=v1.3.0+build.5
run_step resolve r
resolved "a tag with build metadata resolves" v1.3.0+build.5 1.3.0+build.5 "$RELEASE_SHA"

reset_ctx
INPUT_TAG=v1.4.0
run_step resolve r
resolved "a tag of a tag is peeled down to the commit" v1.4.0 1.4.0 "$RELEASE_SHA"

reset_ctx
INPUT_TAG=v1.0.0
STEP_REMOTE=""
run_step resolve r
resolved "with no override the remote is the repository on the server the run is on" v1.0.0 1.0.0 "$RELEASE_SHA"

reset_ctx
EVENT=push
REF_NAME=v1.0.0
run_step resolve r
resolved "a tag push builds the tag's commit, as a SHA and not the tag's name" v1.0.0 1.0.0 "$RELEASE_SHA"

reset_ctx
EVENT=push
REF_NAME=v1.1.0
run_step resolve r
resolved "a push of a lightweight tag builds the tag's commit" v1.1.0 1.1.0 "$RELEASE_SHA"

reset_ctx
INPUT_TAG=v1.0.0
run_step resolve r
if [ "$status" -eq 0 ] && [ -z "$(ls -A "$case_dir/tmp")" ]; then
  pass "the scratch repository the step makes is removed"
else
  fail "the scratch repository the step makes is removed" "exit $status; left behind: $(ls "$case_dir/tmp" | tr '\n' ' ')"
fi

# ── what a dispatch refuses, before anything is built ────────────────────────

bad_tags=(
  "1.0.0" "v1.0" "v1.0.0.1" "v1.0.0-" "vx.y.z" "V1.0.0" "main" "refs/tags/v1.0.0" "v1.0.0/x"
  " v1.0.0" "v1.0.0 " "v1.0.0^{}" "v1.0.0~1" "-v1.0.0" "v1.0.0-rc 1"
)
for t in "${bad_tags[@]}"; do
  reset_ctx
  INPUT_TAG=$t
  run_step resolve r
  refused "a tag that is not vN.N.N[-pre|+build] is refused: \"$t\"" "is not a release tag"
done

reset_ctx
INPUT_TAG=""
run_step resolve r
refused "an empty tag is refused" "is not a release tag"

reset_ctx
INPUT_TAG=$'v1.0.0\n'
run_step resolve r
refused "a tag with a trailing newline is refused" "is not a release tag"

reset_ctx
INPUT_TAG=$'v1.0.0\nv1.1.0'
run_step resolve r
refused "two tags on two lines are refused" "is not a release tag"

reset_ctx
INPUT_TAG=$'v\xd9\xa1.0.0'
run_step resolve r
refused "a tag with non-ASCII digits is refused" "is not a release tag"

reset_ctx
EVENT=push
REF_NAME=v1.0.0rc1
run_step resolve r
refused "a pushed tag that is not well-formed is refused" "is not a release tag"

reset_ctx
INPUT_TAG=v9.9.9
run_step resolve r
refused "a tag that does not exist is refused" "does not exist"

reset_ctx
INPUT_TAG=v3.0.0
run_step resolve r
refused "a branch of the same name is not a tag" "does not exist"

reset_ctx
INPUT_TAG=v7.7.7
run_step resolve r
refused "a ref that only ends in the tag's name is not the tag" "does not exist"

reset_ctx
INPUT_TAG=v2.0.0
run_step resolve r
refused "an annotated tag of a tree is refused" "does not point at a commit"

reset_ctx
INPUT_TAG=v2.1.0
run_step resolve r
refused "a lightweight tag of a tree is refused" "does not point at a commit"

reset_ctx
INPUT_TAG=v2.2.0
run_step resolve r
refused "a tag of a blob is refused" "does not point at a commit"

reset_ctx
INPUT_TAG=v1.0.0
STEP_REMOTE="$WORK/not-there.git"
run_step resolve r
refused "a remote that cannot be read is a refusal, not a guess" "cannot read the tags"

# ── an input is data, never code ──────────────────────────────────────────────

injections=(
  'v1.0.0"; touch pwned; "'
  'v1.0.0$(touch pwned)'
  'v1.0.0`touch pwned`'
  "v1.0.0'; touch pwned; '"
  $'v1.0.0\ntouch pwned'
  $'v1.0.0\nbuild_ref=0000000000000000000000000000000000000000'
)
for t in "${injections[@]}"; do
  reset_ctx
  INPUT_TAG=$t
  run_step resolve r
  shown=$(printf '%s' "$t" | tr '\n' '~')
  refused "an input shaped like a command is refused and nothing runs: $shown" "is not a release tag"
done

# ── the job's token reaches git as a scoped header, and is not in a command line ────────

mkdir -p "$WORK/shim"
cat >"$WORK/shim/git" <<'EOF'
#!/bin/sh
{
  printf 'count=%s\nkey=%s\nvalue=%s\n' "${GIT_CONFIG_COUNT-}" "${GIT_CONFIG_KEY_0-}" "${GIT_CONFIG_VALUE_0-}"
  printf 'args=%s\n' "$*"
} >>"$SHIM_LOG"
for a; do
  shift
  if [ "$a" = "$SHIM_URL" ]; then set -- "$@" "$SHIM_TARGET"; else set -- "$@" "$a"; fi
done
exec "$SHIM_REAL_GIT" "$@"
EOF
chmod +x "$WORK/shim/git"

TOKEN_VALUE=ghs_0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyzAB
TOKEN_B64=$(printf 'x-access-token:%s' "$TOKEN_VALUE" | base64 | tr -d '\n')
reset_ctx
INPUT_TAG=v1.0.0
TOKEN=$TOKEN_VALUE
SERVER_URL=https://github.example
STEP_REMOTE=https://github.example/example/lumen.git
STEP_PATH="$WORK/shim:$PATH"
export SHIM_LOG="$WORK/shim.log" SHIM_URL=$STEP_REMOTE SHIM_TARGET=$REMOTE SHIM_REAL_GIT
SHIM_REAL_GIT=$(command -v git)
: >"$SHIM_LOG"
run_step resolve r
resolved "with a token and an https remote the tag still resolves" v1.0.0 1.0.0 "$RELEASE_SHA"
unset SHIM_LOG SHIM_URL SHIM_TARGET SHIM_REAL_GIT
if grep -qxF "key=http.https://github.example/.extraheader" "$WORK/shim.log" &&
  grep -qxF "value=AUTHORIZATION: basic $TOKEN_B64" "$WORK/shim.log"; then
  pass "the token is sent as an Authorization header scoped to the server's URL"
else
  fail "the token is sent as an Authorization header scoped to the server's URL" \
    "git saw: $(grep -E '^(key|value)=' "$WORK/shim.log" | sort -u | head -c 240 | tr '\n' ' ')"
fi
if ! grep '^args=' "$WORK/shim.log" | grep -qF -e "$TOKEN_VALUE" -e "$TOKEN_B64" &&
  ! said | grep -qF -e "$TOKEN_VALUE" &&
  said | grep -qxF "::add-mask::$TOKEN_B64"; then
  pass "the token is in no command line or log line, and its header is masked"
else
  fail "the token is in no command line or log line, and its header is masked" \
    "args: $(grep '^args=' "$WORK/shim.log" | head -c 200 | tr '\n' ' ') log: $(brief)"
fi

# ── the workflow around the step ──────────────────────────────────────────────

code="$WORK/code.txt"
awk '!/^[ \t]*#/ { print NR ": " $0 }' "$wf" >"$code"

job_names() { awk '/^jobs:/ { on = 1; next } on && /^  [A-Za-z0-9_-]+:[ \t]*$/ { sub(/^  /, ""); sub(/:.*$/, ""); print }' "$wf"; }
job_code() {
  awk -v j="$1" '$0 == "  " j ":" { on = 1; next } on && /^  [A-Za-z0-9_-]+:[ \t]*$/ { exit } on && !/^[ \t]*#/' "$wf"
}

if ! grep -q 'github\.sha' "$code"; then
  pass "no job reads github.sha"
else
  fail "no job reads github.sha" "$(grep 'github\.sha' "$code" | head -c 240 | tr '\n' ' ')"
fi

stray=""
for j in $(job_names); do
  [ "$j" = resolve ] && continue
  if job_code "$j" | grep -qE 'GITHUB_REF|github\.ref'; then stray="$stray $j"; fi
done
if [ -z "$stray" ]; then
  pass "no job but resolve reads the ref it was started on"
else
  fail "no job but resolve reads the ref it was started on" "read in:$stray"
fi

if grep -q 'inputs\.tag' "$code" &&
  ! grep 'inputs\.tag' "$code" | grep -vqE '^[0-9]+: +[A-Z][A-Z0-9_]*: [$][{][{] inputs[.]tag [}][}]$'; then
  pass "the dispatch input reaches a script only through env"
else
  fail "the dispatch input reaches a script only through env" \
    "$(grep 'inputs\.tag' "$code" | grep -vE '^[0-9]+: +[A-Z][A-Z0-9_]*: [$][{][{] inputs[.]tag [}][}]$' | head -c 240 | tr '\n' ' ')"
fi

if job_code resolve | grep -qF 'build_ref: ${{ steps.r.outputs.build_ref }}'; then
  pass "resolve publishes the commit as build_ref"
else
  fail "resolve publishes the commit as build_ref" "no build_ref output taken from the r step"
fi

awk '
  function leave() { if (co && !repo) print job, (ok ? "ok" : "BAD"); co = 0; repo = 0; ok = 0 }
  /^  [A-Za-z0-9_-]+:[ \t]*$/ { leave(); job = $0; sub(/^  /, "", job); sub(/:.*/, "", job); next }
  /^      - / { leave() }
  /uses: actions\/checkout@/ { co = 1 }
  /^ +repository:/ { repo = 1 }
  /^ +ref: [$][{][{] (needs[.]resolve|steps[.]r)[.]outputs[.]build_ref [}][}][ \t]*$/ { ok = 1 }
  END { leave() }
' "$wf" >"$WORK/checkouts"
missing=""
for j in resolve build build-cli build-gui-windows build-gui-linux update-tap; do
  grep -qxF "$j ok" "$WORK/checkouts" || missing="$missing $j"
done
if [ -z "$missing" ] && ! grep -q ' BAD$' "$WORK/checkouts"; then
  pass "every checkout of this repository is of the resolved commit"
else
  fail "every checkout of this repository is of the resolved commit" \
    "not on build_ref:$missing $(grep ' BAD$' "$WORK/checkouts" | tr '\n' ' ')"
fi

failure_job=dispatch-build-failed
failure_code=$(job_code "$failure_job")
need=""
for j in resolve build-cli build-gui-windows build-gui-linux; do
  printf '%s\n' "$failure_code" | grep -E '^    needs:' | grep -qF -- "$j" || need="$need $j"
done
cond=""
printf '%s\n' "$failure_code" | grep -qF "github.event_name == 'workflow_dispatch'" || cond="$cond dispatch-only"
printf '%s\n' "$failure_code" | grep -qF "needs.resolve.result == 'success'" || cond="$cond resolve-succeeded"
for j in build-cli build-gui-windows build-gui-linux; do
  printf '%s\n' "$failure_code" | grep -qF "needs.${j}.result == 'failure'" || cond="$cond $j-failed"
done
if [ -n "$failure_code" ] && [ -z "$need" ] && [ -z "$cond" ]; then
  pass "a last job reports a failed dispatch build, and not a refused tag or a push"
else
  fail "a last job reports a failed dispatch build, and not a refused tag or a push" \
    "${failure_code:+job $failure_job: }${failure_code:-no $failure_job job;}${need:+ does not need:$need;}${cond:+ condition lacks:$cond}"
fi

reset_ctx
NEEDS=(
  "CTX_NEEDS_RESOLVE_OUTPUTS_TAG=v1.0.0" "CTX_NEEDS_RESOLVE_OUTPUTS_BUILD_REF=$RELEASE_SHA"
  "CTX_NEEDS_BUILD_CLI_RESULT=failure" "CTX_NEEDS_BUILD_GUI_WINDOWS_RESULT=success"
  "CTX_NEEDS_BUILD_GUI_LINUX_RESULT=skipped"
)
run_step "$failure_job" refuse
why=""
[ "$status" -ne 0 ] || why="$why the job did not fail the run (exit $status);"
for want in "::error" "v1.0.0 ($RELEASE_SHA) does not build" "build-cli" "never builds from main" "patch release"; do
  said | grep -qF -- "$want" || why="$why the annotation lacks \"$want\";"
done
for want in "v1.0.0" "$RELEASE_SHA" "build-cli" "patch release"; do
  grep -qF -- "$want" "$case_dir/summary" || why="$why the job summary lacks \"$want\";"
done
if [ -z "$why" ]; then
  pass "a failed dispatch build says the tag does not build, that main is never the fallback, and what to do"
else
  fail "a failed dispatch build says the tag does not build, that main is never the fallback, and what to do" \
    "$why $(brief)"
fi

if [ "$failed" -ne 0 ]; then
  printf '\n%s check(s) failed\n' "$failed"
  exit 1
fi
printf '\nall checks passed\n'

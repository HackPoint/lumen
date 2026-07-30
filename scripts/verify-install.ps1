# verify-install.ps1 — check that an installed Lumen actually works, on Windows.
#
# The PowerShell counterpart to verify-install.sh, and the only way this platform gets
# checked at all: the development machine is macOS, which cannot run Windows containers,
# so every Windows claim here is made by CI or not at all.
#
# Like the shell version, this drives the MCP server over stdio with a real JSON-RPC
# handshake rather than asserting a file exists. "Present" and "answers" are different
# claims and only the second matters.
#
# Usage:
#   .\scripts\verify-install.ps1 -BinDir target\release -Expect 1.5.0
#   .\scripts\verify-install.ps1 -CliOnly

[CmdletBinding()]
param(
    [string]$BinDir = "",
    [string]$Expect = "",
    [switch]$CliOnly
)

$script:Pass = 0
$script:Fail = 0
$script:Skip = 0

function Ok    ($m) { Write-Host "  PASS  $m" -ForegroundColor Green;  $script:Pass++ }
function Bad   ($m) { Write-Host "  FAIL  $m" -ForegroundColor Red;    $script:Fail++ }
function Skipped($m){ Write-Host "  SKIP  $m" -ForegroundColor Yellow; $script:Skip++ }
function Section($m){ Write-Host ""; Write-Host $m -ForegroundColor White }

Section "Lumen install verification — windows, $env:PROCESSOR_ARCHITECTURE"

function Find-Bin([string]$Name) {
    if ($BinDir -and (Test-Path (Join-Path $BinDir "$Name.exe"))) {
        return (Resolve-Path (Join-Path $BinDir "$Name.exe")).Path
    }
    # The installer puts the app under Program Files; the name mirrors the bundle.
    foreach ($root in @("$env:ProgramFiles\Lumen", "$env:LOCALAPPDATA\Programs\Lumen")) {
        if (Test-Path (Join-Path $root "$Name.exe")) { return (Join-Path $root "$Name.exe") }
    }
    $c = Get-Command "$Name.exe" -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    return $null
}

# The CLI ships as `lumen.exe` from the zip and as `lumen-cli.exe` inside the bundle.
$cli = Find-Bin "lumen-cli"; if (-not $cli) { $cli = Find-Bin "lumen" }
$mcp = Find-Bin "lumen-mcp"
$tok = Find-Bin "lumen-tok"

Section "Binaries"
if ($cli) { Ok "CLI found: $cli" } else { Bad "CLI not found" }
foreach ($p in @(@{n="MCP";v=$mcp}, @{n="TOK";v=$tok})) {
    if ($p.v)        { Ok "$($p.n) found: $($p.v)" }
    elseif ($CliOnly){ Skipped "$($p.n) absent — expected for a CLI-only install" }
    else             { Bad "$($p.n) not found" }
}

Section "Version"
if ($cli) {
    $raw = (& $cli --version 2>$null | Out-String).Trim()
    $ver = ($raw -split '\s+')[-1]
    if ($ver) {
        Ok "CLI reports $ver"
        if ($Expect) {
            if ($ver -eq $Expect) { Ok "matches expected $Expect" }
            # The check that catches a bundle whose sidecars are older than its UI.
            else { Bad "expected $Expect, installed $ver" }
        }
    } else { Bad "CLI produced no version" }
} else { Bad "no CLI to version-check" }

Section "MCP server (JSON-RPC over stdio)"
if (-not $mcp) {
    if ($CliOnly) { Skipped "no MCP binary in a CLI-only install" } else { Bad "no MCP binary to drive" }
} else {
    $requests = @(
        '{"jsonrpc":"2.0","id":1,"method":"initialize"}',
        '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
    ) -join "`n"
    # Piped through stdin so the server is exercised the way Claude Code drives it.
    $out = ($requests | & $mcp 2>$null | Out-String)

    if ($out -match '"protocolVersion"') { Ok "initialize returned a protocolVersion" }
    else { Bad "initialize did not return a protocolVersion" }

    foreach ($tool in @("smart_read","recall_file","compress_logs","lumen_ping")) {
        if ($out -match "`"$tool`"") { Ok "tool advertised: $tool" } else { Bad "tool missing: $tool" }
    }

    # The advertised threshold must match the hook's; these disagreed in 1.4.0.
    if     ($out -match 'files ≥300 lines') { Ok "smart_read advertises the >=300-line threshold" }
    elseif ($out -match 'files ≥100 lines') { Bad "smart_read still advertises >=100 lines (stale binary)" }
    else   { Skipped "threshold text not found in the tool description" }

    $firstLine = ($out -split "`r?`n" | Where-Object { $_ -ne "" } | Select-Object -First 1)
    if ($firstLine -and $firstLine.StartsWith("{")) { Ok "stdout is pure JSON-RPC (diagnostics went to stderr)" }
    else { Bad "stdout was polluted with non-protocol output" }
}

Section "Tokenizer"
if ($tok) {
    $n = ("fn main() {}" | & $tok 2>$null | Out-String).Trim()
    if ($n -match '^\d+$' -and [int]$n -gt 0) { Ok "lumen-tok counted $n tokens" }
    else { Bad "lumen-tok produced no count — metering would fall back to bytes/4" }
} elseif ($CliOnly) {
    Skipped "no lumen-tok in a CLI-only install — hook metering would fall back to bytes/4"
} else { Bad "no lumen-tok" }

Section "CLI report path"
if ($cli) {
    & $cli report --help *>$null
    if ($LASTEXITCODE -eq 0) { Ok "report subcommand present" } else { Bad "report subcommand missing" }
    # Must refuse to file without --yes; a regression here publishes without consent.
    & $cli report --faults nonexistent-fixture.json *>$null
    if ($LASTEXITCODE -ne 0) { Ok "report exits non-zero without --dry-run/--yes ($LASTEXITCODE)" }
    else { Bad "report exited 0 without being asked to file or dry-run" }
}

Section "Hook scripts"
$hookDir = Join-Path $env:USERPROFILE ".claude\lumen"
if (-not (Test-Path $hookDir)) {
    Skipped "no ~/.claude/lumen — Setup has not run on this machine"
} else {
    foreach ($f in @("lumen_read_intercept.sh","lumen_meter.sh")) {
        if (Test-Path (Join-Path $hookDir $f)) { Ok "$f present" } else { Bad "$f missing" }
    }
    $intercept = Join-Path $hookDir "lumen_read_intercept.sh"
    if (Test-Path $intercept) {
        $body = Get-Content $intercept -Raw
        # Their absence is what deadlocked a session, and the fix reached the developer
        # copy a full release before it reached the installed one.
        if ($body -match 'lumen_mcp_missing' -and $body -match 'retry_escape_valve') {
            Ok "intercept has both fail-open guards"
        } else { Bad "intercept is missing a fail-open guard — a session can deadlock" }
    }
}

Section "Tray widget"
if ($CliOnly) {
    Skipped "-CliOnly: GUI checks not applicable"
} else {
    $exe = Find-Bin "Lumen"
    if ($exe) { Ok "app executable installed: $exe" } else { Bad "no Lumen.exe found" }
    if (Get-Process -Name "Lumen" -ErrorAction SilentlyContinue) {
        Ok "tray process is running"
    } else {
        Skipped "tray process not running (CI has no desktop session to run it in)"
    }
    # Autostart on Windows is a Run key, so "runs automatically after setup" is checkable.
    $run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    $entry = (Get-ItemProperty -Path $run -ErrorAction SilentlyContinue).PSObject.Properties |
             Where-Object { $_.Name -like "*Lumen*" -or $_.Value -like "*Lumen*" }
    if ($entry) { Ok "autostart Run key registered: $($entry.Name)" }
    else { Skipped "no autostart Run key — Setup has not run, or autostart is off" }
    # A tray icon needs an interactive session; CI has none, and asserting otherwise
    # would be theatre.
    Skipped "tray placement itself needs an interactive desktop session to confirm"
}

Section "Result"
Write-Host ("  {0} passed, {1} failed, {2} skipped" -f $script:Pass, $script:Fail, $script:Skip)
Write-Host ""
if ($script:Fail -gt 0) { exit 1 } else { exit 0 }

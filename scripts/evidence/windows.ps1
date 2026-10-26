# A published release's evidence on Windows, installed the way users install it: setup.exe and
# the CLI zip from the GitHub release. Keeps the installer's publisher, the app's tabs (through
# WebDriver, lumenator/e2e/evidence.mjs), the CLI in PowerShell and in cmd (PNG and raw text), and
# for an upgrade the all-time savings before and after, which must match.
#
# Environment: VERSION, SCENARIO (fresh | from-1.9.0 | from-1.8.0), OUT, HELPERS (the app crate's
# test binary), LUMEN_E2E_NATIVE_DRIVER (msedgedriver). Installs into the real profile:
# GitHub-hosted runners only.
$ErrorActionPreference = 'Continue'
foreach ($v in 'VERSION', 'SCENARIO', 'OUT') {
  if (-not [Environment]::GetEnvironmentVariable($v)) { Write-Error "$v is not set"; exit 2 }
}
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
  Write-Error 'refusing: this installs into the real profile; GitHub-hosted runners only'
  exit 2
}
$Version = $env:VERSION; $Scenario = $env:SCENARIO; $Out = $env:OUT
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Label = "windows-$Scenario"
$Notes = Join-Path $Out "notes-$Label.txt"
$script:Failed = 0
function Note($m) { $m | Tee-Object -FilePath $Notes -Append }
function Cannot($m) { Note "CANNOT SHOW: $m" }
function Fail($m) { Note "FAIL: $m"; $script:Failed = 1 }
function Helper($name) {
  & $env:HELPERS --ignored --exact "setup::hook_e2e::upgrade_ci::$name" --nocapture *>> (Join-Path $Out "helpers-$Label.txt")
  return $LASTEXITCODE -eq 0
}
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
function Shoot($file) {
  $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
  $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
}
$Work = Join-Path $env:RUNNER_TEMP "evidence-$Label"
New-Item -ItemType Directory -Force -Path $Work | Out-Null

# ── What was published ─────────────────────────────────────────────────────────────────────
gh release download "v$Version" -R HackPoint/lumen -D $Work --clobber `
  -p "Lumen_${Version}_x64-setup.exe" -p "lumen-v$Version-x86_64-pc-windows-msvc.zip" *>> $Notes
$Setup = Join-Path $Work "Lumen_${Version}_x64-setup.exe"
if (-not (Test-Path $Setup)) { Fail "the v$Version installer could not be downloaded"; exit 1 }
Expand-Archive (Join-Path $Work "lumen-v$Version-x86_64-pc-windows-msvc.zip") -DestinationPath (Join-Path $Work 'cli') -Force
$Lumen = (Get-ChildItem (Join-Path $Work 'cli') -Recurse -Filter lumen.exe | Select-Object -First 1).FullName
if (-not $Lumen) { Fail 'no lumen.exe in the CLI zip' }
$env:PATH = "$(Split-Path $Lumen);$env:PATH"

# ── The installer's publisher line ─────────────────────────────────────────────────────────
$vi = (Get-Item $Setup).VersionInfo
$sig = Get-AuthenticodeSignature $Setup
@(
  "File:            $($Setup | Split-Path -Leaf)",
  "CompanyName:     $($vi.CompanyName)",
  "ProductName:     $($vi.ProductName)",
  "FileVersion:     $($vi.FileVersion)",
  "Authenticode:    $($sig.Status)",
  "Signer:          $(if ($sig.SignerCertificate) { $sig.SignerCertificate.Subject } else { '(none: not signed)' })"
) | Set-Content (Join-Path $Out "installer-publisher-$Label.txt")
Note "installer: CompanyName '$($vi.CompanyName)', Authenticode $($sig.Status)"
Cannot "the installer's own dialog and a UAC prompt: the install is silent (/S) and a runner has no UAC prompt; the publisher it carries is in installer-publisher-$Label.txt"

# ── 1. The previous install, for an upgrade, with history in it ────────────────────────────
$PrevDb = Join-Path $env:APPDATA 'example.previous.app\lumen.db'
if ($Scenario -ne 'fresh') {
  $env:LUMEN_UPGRADE_FROM = $Scenario -replace '^from-', ''
  $env:LUMEN_UPGRADE_LOGIN = 'off'
  if (Helper 'seed') { Note "seeded a previous install from $env:LUMEN_UPGRADE_FROM (simulated: its binaries are no longer published)" } else { Fail 'seeding the previous install' }
  $env:LUMEN_DB = $PrevDb
  & $Lumen report --json > (Join-Path $Out "before-report-$Label.json")
  & $Lumen report *> (Join-Path $Out "before-report-$Label.txt")
  Remove-Item Env:\LUMEN_DB
}

# ── 2. Install, silently, then the first start ─────────────────────────────────────────────
Start-Process -FilePath $Setup -ArgumentList '/S' -Wait
$App = Join-Path $env:LOCALAPPDATA 'Lumen\Lumen.exe'
if (-not (Test-Path $App)) { Fail "no app at $App after the install" }
$pub = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Lumen' -ErrorAction SilentlyContinue).Publisher
Note "Apps & features: Lumen, publisher '$pub'"
if ($pub -ne 'HackPoint') { Fail "the Apps & features publisher is '$pub', not HackPoint" }
$got = (& $Lumen --version) 2>&1
Note "lumen --version: $got"
if ("$got" -notmatch [regex]::Escape($Version)) { Fail "the CLI zip holds '$got', not $Version" }
Start-Process -FilePath $App
if ($Scenario -ne 'fresh') {
  if (Helper 'wait_for_the_move') { Note 'the start moved the previous install, and Setup ran' } else { Fail 'the start did not move the previous install' }
} else {
  Start-Sleep -Seconds 15
}
Get-Process | Where-Object { $_.Name -match '^(Lumen|lumen-daemon)$' } | Stop-Process -Force
Start-Sleep -Seconds 3

# ── 3. All-time savings after ───────────────────────────────────────────────────────────────
& $Lumen report --json > (Join-Path $Out "after-report-$Label.json")
& $Lumen report *> (Join-Path $Out "after-report-$Label.txt")
if ($Scenario -ne 'fresh') {
  $pick = { param($p) $a = (Get-Content $p -Raw | ConvertFrom-Json).allTime
    [ordered]@{ tokensSaved = $a.tokensSaved; tokensFull = $a.tokensFull; sharePct = $a.sharePct; net = $a.measured.net } | ConvertTo-Json -Compress -Depth 5 }
  $before = & $pick (Join-Path $Out "before-report-$Label.json")
  $after = & $pick (Join-Path $Out "after-report-$Label.json")
  Note "all-time before: $before"
  Note "all-time after:  $after"
  if ($before -eq $after) { Note 'all-time savings match before and after the upgrade' } else { Fail 'all-time savings differ before and after the upgrade' }
}

# ── 4. The app's tabs ───────────────────────────────────────────────────────────────────────
node (Join-Path $Root 'lumenator\e2e\evidence.mjs') $App $Out $Label *>> $Notes
if ($LASTEXITCODE -ne 0) { Fail "the app's tabs could not be captured" }
Get-Process | Where-Object { $_.Name -match '^(Lumen|lumen-daemon)$' } | Stop-Process -Force
Cannot "the tray menu: the notification area's menus cannot be opened reliably on a runner"

# ── 5. The CLI, in PowerShell and in cmd, and as raw text ──────────────────────────────────
& $Lumen --version *> (Join-Path $Out "cli-version-$Label.txt")
& $Lumen *> (Join-Path $Out "cli-line-$Label.txt")
& $Lumen report *> (Join-Path $Out "cli-report-$Label.txt")
& $Lumen doctor *> (Join-Path $Out "cli-doctor-$Label.txt")
$cmds = 'lumen --version; lumen; lumen report; lumen doctor'
$ps = Start-Process powershell -PassThru -ArgumentList '-NoExit', '-Command', "mode con: cols=160 lines=75; `$env:PATH='$(Split-Path $Lumen);' + `$env:PATH; $cmds"
Start-Sleep -Seconds 10
Shoot (Join-Path $Out "cli-powershell-$Label.png")
Stop-Process -Id $ps.Id -Force
$cm = Start-Process cmd -PassThru -ArgumentList '/k', "mode con: cols=160 lines=75 & set PATH=$(Split-Path $Lumen);%PATH% & lumen --version & lumen & lumen report & lumen doctor"
Start-Sleep -Seconds 10
Shoot (Join-Path $Out "cli-cmd-$Label.png")
Stop-Process -Id $cm.Id -Force
Note "the CLI in PowerShell and cmd: cli-powershell-$Label.png, cli-cmd-$Label.png (a long report scrolls; the full output is in cli-*.txt)"

Note "done: $Label, failures: $script:Failed"
exit $script:Failed

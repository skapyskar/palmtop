#Requires -Version 5.1
<#
.SYNOPSIS
    Install the Palmtop host daemon on this Windows machine.

.DESCRIPTION
    Mirrors install.sh's structure and guarantees on the other platform this
    project supports: always a clean install (see uninstall.ps1), so a
    reinstall can never inherit a half-broken Scheduled Task, a stale
    binary, or a config written by an older version. -KeepPairing skips
    regenerating the pairing token/host key, matching install.sh's own flag.

    Installs palmtopd.exe (and the bundled ffmpeg.exe, if present next to
    this script) to %LOCALAPPDATA%\Palmtop, seeds host.toml under
    %APPDATA%\palmtop (palmtop_config::config_dir's Windows branch),
    registers a logon Scheduled Task -- the closest Windows equivalent to
    the systemd --user service install.sh registers, since a real Windows
    Service runs in Session 0 and cannot capture the desktop or inject
    input into it -- and runs --doctor.

    Nothing here needs Administrator. Screen capture goes through
    Windows.Graphics.Capture (a per-session API, no elevated privilege
    needed to capture your own desktop), and SendInput needs no special
    privilege either. A remote-control tool asking to run elevated would be
    a much larger thing to trust than one that doesn't.

.PARAMETER KeepPairing
    Keep the existing host.toml (pairing token + host key) instead of
    generating fresh ones. Phones stay paired across the reinstall.

.EXAMPLE
    .\install.ps1
.EXAMPLE
    .\install.ps1 -KeepPairing
#>
[CmdletBinding()]
param(
    [switch]$KeepPairing
)

$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$InstallDir = Join-Path $env:LOCALAPPDATA 'Palmtop'
$ConfigDir = Join-Path $env:APPDATA 'palmtop'

function Write-Info($msg) { Write-Host $msg }
function Write-Warn($msg) { Write-Host "warning: $msg" -ForegroundColor Yellow }
function Fail($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

# Two layouts, same distinction install.sh draws: a release archive has
# palmtopd.exe sitting right beside this script; a git checkout has
# Cargo.toml one directory up instead.
$ReleaseBinary = Join-Path $ScriptDir 'palmtopd.exe'
$ReleaseMode = Test-Path $ReleaseBinary

# --- 0. remove any previous install ------------------------------------------
$UninstallScript = Join-Path $ScriptDir 'uninstall.ps1'
if (Test-Path $UninstallScript) {
    Write-Info 'Removing any previous install...'
    $keepArg = @()
    if ($KeepPairing) { $keepArg = @('-KeepPairing') }
    & $UninstallScript -Yes -Quiet @keepArg
} else {
    Write-Warn 'uninstall.ps1 not found next to this script -- installing over whatever is there.'
}

# --- 1. get the binary --------------------------------------------------------
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

if ($ReleaseMode) {
    Copy-Item $ReleaseBinary (Join-Path $InstallDir 'palmtopd.exe') -Force
    $BundledFfmpeg = Join-Path $ScriptDir 'ffmpeg.exe'
    if (Test-Path $BundledFfmpeg) {
        Copy-Item $BundledFfmpeg (Join-Path $InstallDir 'ffmpeg.exe') -Force
    }
    Write-Info "Installed to $InstallDir\palmtopd.exe"
} else {
    $RepoRoot = Split-Path -Parent $ScriptDir
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail 'cargo not found. Install Rust from https://rustup.rs, or download a release build and run its own install.ps1'
    }
    Write-Info 'Building palmtopd (this takes a few minutes the first time)...'
    Push-Location $RepoRoot
    try {
        cargo build --release -p palmtopd
        if ($LASTEXITCODE -ne 0) { Fail 'build failed' }
    } finally {
        Pop-Location
    }
    Copy-Item (Join-Path $RepoRoot 'target\release\palmtopd.exe') (Join-Path $InstallDir 'palmtopd.exe') -Force
    Write-Info "Installed to $InstallDir\palmtopd.exe"
}

# ffmpeg is checked, not assumed: an install that silently has no working
# encoder fails much later and much less clearly, at the first real stream
# attempt, when the phone connects to a blank screen.
$BinFfmpeg = Join-Path $InstallDir 'ffmpeg.exe'
if (-not (Test-Path $BinFfmpeg) -and -not (Get-Command ffmpeg -ErrorAction SilentlyContinue)) {
    Write-Warn 'No ffmpeg.exe bundled and none found on PATH. Video encoding will not work until'
    Write-Warn 'one is available -- place ffmpeg.exe next to palmtopd.exe, or install it and add'
    Write-Warn 'it to PATH. https://www.gyan.dev/ffmpeg/builds/ has current Windows builds.'
}

# --- 2. config -----------------------------------------------------------------
# Normally absent by now: step 0 removed it, so this writes a fresh one and
# the daemon generates a new pairing token and host key on first start. With
# -KeepPairing the existing file survives and is reused as-is.
New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null
$HostToml = Join-Path $ConfigDir 'host.toml'
if (Test-Path $HostToml) {
    Write-Info "Keeping the existing $HostToml -- phones stay paired."
} else {
    $Template = if ($ReleaseMode) { Join-Path $ScriptDir 'host.example.toml' }
                else { Join-Path (Split-Path -Parent $ScriptDir) 'config\host.example.toml' }
    Write-Info "Creating $HostToml from the template..."
    Copy-Item $Template $HostToml
    Write-Info 'Host address left blank so it is detected at runtime.'
}

# --- 3. Scheduled Task ---------------------------------------------------------
# The closest equivalent to install.sh's systemd --user unit: runs as the
# current user, in the interactive desktop session (not Session 0, which is
# where a real Windows Service would run and which cannot capture the
# screen or inject input into the user's desktop), and restarts at every
# logon without needing Administrator to register for the current user.
$TaskName = 'Palmtop'
$ExePath = Join-Path $InstallDir 'palmtopd.exe'
$LogPath = Join-Path $InstallDir 'palmtopd.log'

# Task Scheduler does not capture a launched process's stdout/stderr
# anywhere by default -- it just runs, and whatever it prints (including,
# critically, the exact pairing token/QR path and any startup error) is
# lost. schtasks itself has no redirection option, so the Task points at a
# tiny wrapper batch file that does the redirecting, rather than fighting
# schtasks /tr's nested-quoting rules to inline a `cmd /c ... > log` command
# directly -- a wrapper file keeps every layer of quoting to exactly one
# level. `>` (not `>>`) so the log reflects only the current run, not an
# ever-growing history across every logon since install.
$WrapperPath = Join-Path $InstallDir 'run-palmtopd.cmd'
@"
@echo off
"$ExePath" > "$LogPath" 2>&1
"@ | Set-Content -Path $WrapperPath -Encoding ASCII

Write-Info 'Registering the logon Scheduled Task...'
& schtasks /create /tn $TaskName /tr "`"$WrapperPath`"" /sc onlogon /rl limited /f | Out-Null
if ($LASTEXITCODE -ne 0) {
    Fail 'could not register the Scheduled Task -- see the schtasks error above'
}
# Starts it now too, rather than waiting for the next logon, so install.ps1
# leaves the daemon actually running, matching install.sh's own behavior.
& schtasks /run /tn $TaskName | Out-Null

Start-Sleep -Seconds 2
$Running = Get-Process -Name 'palmtopd' -ErrorAction SilentlyContinue
if ($Running) {
    Write-Info 'palmtopd is running.'
} else {
    Write-Warn "palmtopd does not appear to be running. Check $LogPath for why, or Task Scheduler > Task Scheduler Library > Palmtop for its last run result."
}

# --- 4. check it can actually work ---------------------------------------------
Write-Info ''
Write-Info 'Checking this machine can capture and encode...'
$env:PALMTOP_CONFIG_DIR = $ConfigDir
& $ExePath --doctor
if ($LASTEXITCODE -ne 0) {
    Write-Warn 'Some checks failed -- see above. Pairing will still work, but the phone may'
    Write-Warn 'not get a picture until those are fixed. Re-run any time with:'
    Write-Warn "  & `"$ExePath`" --doctor"
}

# --- 5. firewall (offered, not applied) -----------------------------------------
# Adding a firewall rule needs Administrator, which install.ps1 otherwise
# never asks for -- offered as a copy-pasteable command rather than run
# automatically, so this script's "no elevation needed" guarantee stays
# true. Without it, Windows Defender Firewall will prompt on the daemon's
# first incoming connection instead; allowing it there works just as well.
Write-Info ''
Write-Info 'Windows Defender Firewall may prompt to allow palmtopd when a phone first'
Write-Info 'connects -- allow it. To add the rule in advance instead (needs an admin'
Write-Info 'PowerShell prompt):'
Write-Info "  New-NetFirewallRule -DisplayName 'Palmtop' -Direction Inbound -Program '$ExePath' -Action Allow"

# --- 6. pair ---------------------------------------------------------------------
Write-Info ''
Write-Info '================================================================'
Write-Info ' Host ready. Now pair your phone.'
Write-Info '================================================================'
Write-Info ''
Write-Info '  1. Install the app on your phone'
Write-Info '  2. In the app: Devices -> Add by scanning QR'
Write-Info '  3. Scan the code Palmtop just wrote to:'
Write-Info "       $env:TEMP\palmtop-pair.svg"
Write-Info '     (open it in any image viewer or browser -- it embeds the pairing'
Write-Info '     token, so do not leave it open on a shared screen)'
Write-Info ''
Write-Info "  Check this machine can capture/encode:  & `"$ExePath`" --doctor"
Write-Info "  See what the background daemon is doing: $LogPath"
Write-Info "  Remove Palmtop completely:               .\uninstall.ps1"
Write-Info ''

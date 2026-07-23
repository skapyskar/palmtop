#Requires -Version 5.1
<#
.SYNOPSIS
    Remove everything Palmtop installed on this Windows machine.

.DESCRIPTION
    Mirrors uninstall.sh: removes the Scheduled Task, the installed binary
    directory, and (by default) the config holding the pairing token and
    host private key. Every phone paired with this laptop will have to pair
    again unless -KeepPairing is given.

    Nothing here needs Administrator, because nothing install.ps1 installs
    does either.

.PARAMETER Yes
    Skip the confirmation prompt.

.PARAMETER KeepPairing
    Keep host.toml (the pairing token and host key), so paired phones are
    unaffected.

.PARAMETER Quiet
    Suppress informational output (still prints nothing on success either
    way beyond the final summary; used by install.ps1's own "clean install"
    step, which prints its own messages instead).
#>
[CmdletBinding()]
param(
    [switch]$Yes,
    [switch]$KeepPairing,
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

function Say($msg) { if (-not $Quiet) { Write-Host $msg } }

$InstallDir = Join-Path $env:LOCALAPPDATA 'Palmtop'
$ConfigDir = Join-Path $env:APPDATA 'palmtop'
$TaskName = 'Palmtop'
$ExePath = Join-Path $InstallDir 'palmtopd.exe'
$HostToml = Join-Path $ConfigDir 'host.toml'
$QrFile = Join-Path $env:TEMP 'palmtop-pair.svg'

if (-not $Yes) {
    Say 'This will remove Palmtop from this machine:'
    Say "  Scheduled Task   $TaskName"
    Say "  binary           $InstallDir"
    if ($KeepPairing) {
        Say "  config           $HostToml  (KEPT -- phones stay paired)"
    } else {
        Say "  config           $HostToml  (including the pairing token and host key)"
        Say ''
        Say 'Every phone paired with this laptop will have to pair again.'
    }
    Say ''
    $reply = Read-Host 'Continue? [y/N]'
    if ($reply -notmatch '^[Yy]') {
        Say 'Nothing was changed.'
        exit 0
    }
}

# --- 1. Scheduled Task + running process --------------------------------------
# Stopped before the task definition is removed, and the task definition
# removed before the binary -- so an in-progress run never ends up pointed
# at a binary that no longer exists.
Get-Process -Name 'palmtopd' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
$existingTask = schtasks /query /tn $TaskName 2>$null
if ($LASTEXITCODE -eq 0) {
    schtasks /delete /tn $TaskName /f | Out-Null
    Say "removed  Scheduled Task '$TaskName'"
}

# --- 2. binary -----------------------------------------------------------------
if (Test-Path $InstallDir) {
    Remove-Item $InstallDir -Recurse -Force
    Say "removed  $InstallDir"
}

# --- 3. secrets ------------------------------------------------------------------
# Removed even with -KeepPairing: it's regenerated on every daemon start and
# embeds the pairing token, so keeping a stale copy around serves nothing.
if (Test-Path $QrFile) {
    Remove-Item $QrFile -Force
    Say "removed  $QrFile"
}

if ($KeepPairing) {
    if (Test-Path $HostToml) { Say "kept     $HostToml (phones stay paired)" }
} else {
    if (Test-Path $HostToml) {
        Remove-Item $HostToml -Force
        Say "removed  $HostToml"
    }
    # Only if now empty, and only ever this exact directory -- a user may
    # keep their own files here, and deleting a directory this script did
    # not create is a much worse bug than leaving an empty one behind.
    if ((Test-Path $ConfigDir) -and -not (Get-ChildItem $ConfigDir -Force -ErrorAction SilentlyContinue)) {
        Remove-Item $ConfigDir -Force -ErrorAction SilentlyContinue
        if (-not (Test-Path $ConfigDir)) { Say "removed  $ConfigDir\" }
    }
}

Say ''
Say 'Palmtop has been removed from this machine.'
if (-not $KeepPairing) {
    Say 'The app on your phone will still list this laptop -- forget it there'
    Say '(long-press the entry) or just pair again after reinstalling.'
}

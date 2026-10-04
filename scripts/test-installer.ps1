<#
.SYNOPSIS
  Installs Oynx silently into a scratch folder, checks the result, then
  uninstalls it again.

.DESCRIPTION
  Runs the installer the same way the in-app updater does when Oynx quits
  (/VERYSILENT /LAUNCH=0) so CI catches an installer that fails, prompts, or
  starts the app when it should not. Exits non-zero on any failure.

.EXAMPLE
  .\scripts\test-installer.ps1 -Setup dist\oynx-0.1.3-windows-x86_64-setup.exe
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Setup
)

$ErrorActionPreference = 'Stop'
$setupPath = (Resolve-Path -LiteralPath $Setup).Path
$scratch = Join-Path ([IO.Path]::GetTempPath()) "oynx-install-test-$PID"
$dir = Join-Path $scratch 'Oynx'
$log = Join-Path $scratch 'setup.log'
New-Item -ItemType Directory -Force -Path $scratch | Out-Null

$before = @(Get-Process -Name 'oynx' -ErrorAction SilentlyContinue | ForEach-Object Id)

try {
    $install = Start-Process -FilePath $setupPath -Wait -PassThru -ArgumentList @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/LAUNCH=0',
        "/DIR=`"$dir`"", "/LOG=`"$log`""
    )
    if ($install.ExitCode -ne 0) {
        throw "the installer exited with code $($install.ExitCode)"
    }

    foreach ($file in @('oynx.exe', 'unins000.exe', 'LICENSE.txt', 'THIRD_PARTY_NOTICES.md')) {
        if (-not (Test-Path -LiteralPath (Join-Path $dir $file))) {
            throw "the installer did not create $file in $dir"
        }
    }

    $global:LASTEXITCODE = 0
    & (Join-Path $PSScriptRoot 'assert-gui-subsystem.ps1') -Path (Join-Path $dir 'oynx.exe')
    if ($LASTEXITCODE -ne 0) { throw 'the installed oynx.exe is not a windowed app' }

    $started = @(Get-Process -Name 'oynx' -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id })
    if ($started.Count -gt 0) {
        $started | Stop-Process -Force -ErrorAction SilentlyContinue
        throw 'the installer started Oynx even though /LAUNCH=0 was passed'
    }

    $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'Oynx.lnk'
    if (-not (Test-Path -LiteralPath $shortcut)) {
        throw "the installer did not create the Start menu shortcut $shortcut"
    }
    Write-Host "PASS: installed to $dir with a Start menu shortcut"

    # The uninstaller copies itself to a temp folder and returns straight away,
    # so wait for the files to go rather than for the process.
    Start-Process -FilePath (Join-Path $dir 'unins000.exe') -Wait -ArgumentList @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART'
    ) | Out-Null
    $deadline = (Get-Date).AddSeconds(60)
    while ((Test-Path -LiteralPath (Join-Path $dir 'oynx.exe')) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
    }
    if (Test-Path -LiteralPath (Join-Path $dir 'oynx.exe')) {
        throw 'the uninstaller did not remove oynx.exe'
    }
    if (Test-Path -LiteralPath $shortcut) {
        throw 'the uninstaller did not remove the Start menu shortcut'
    }
    Write-Host 'PASS: uninstalled cleanly'
}
catch {
    if (Test-Path -LiteralPath $log) {
        Write-Host '=== setup log ==='
        Get-Content -LiteralPath $log | Write-Host
    }
    Write-Host "FAIL: $_"
    exit 1
}
finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
exit 0

<#
.SYNOPSIS
  Packages a built oynx.exe into the Windows installer.

.DESCRIPTION
  Compiles installer\oynx.iss with Inno Setup, installing Inno Setup through
  Chocolatey first if the machine does not have it. The version comes from
  Cargo.toml so the installer always matches the binary it wraps. Prints the
  full path of the setup exe it produced.

.EXAMPLE
  .\scripts\build-installer.ps1 -Exe target\release\oynx.exe
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Exe,

    [string]$OutputDir = 'dist'
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot

$cargoToml = Join-Path $repo 'Cargo.toml'
$version = (Select-String -Path $cargoToml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
if ([string]::IsNullOrWhiteSpace($version)) {
    throw "could not read the package version from $cargoToml"
}

$exePath = (Resolve-Path -LiteralPath $Exe).Path

function Find-Iscc {
    $command = Get-Command 'iscc.exe' -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    foreach ($root in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if (-not $root) { continue }
        $candidate = Join-Path $root 'Inno Setup 6\ISCC.exe'
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    return $null
}

$iscc = Find-Iscc
if (-not $iscc) {
    Write-Host 'Inno Setup not found; installing it with Chocolatey'
    choco install innosetup --yes --no-progress | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'failed to install Inno Setup' }
    $iscc = Find-Iscc
    if (-not $iscc) { throw 'Inno Setup was installed but ISCC.exe could not be found' }
}
Write-Host "using $iscc"

$out = New-Item -ItemType Directory -Force -Path $OutputDir
$name = "oynx-$version-windows-x86_64-setup"
$script = Join-Path $repo 'installer\oynx.iss'

& $iscc "/DAppVersion=$version" "/DSourceExe=$exePath" "/O$($out.FullName)" "/F$name" $script | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed with exit code $LASTEXITCODE" }

$setup = Join-Path $out.FullName "$name.exe"
if (-not (Test-Path -LiteralPath $setup)) {
    throw "expected the installer at $setup"
}
Write-Host "built $setup"
Write-Output $setup

<#
.SYNOPSIS
    Fails unless an executable is linked against the Windows GUI subsystem.

.DESCRIPTION
    The "no console window" requirement is decided at link time by the PE
    optional header's Subsystem field:

        2 = IMAGE_SUBSYSTEM_WINDOWS_GUI  (windowed app, no console)
        3 = IMAGE_SUBSYSTEM_WINDOWS_CUI  (console app, console window appears)

    Reading the header is far more reliable than launching the exe and trying
    to spot a console window, which is impossible to observe on a headless
    runner. This is the check that actually guards the
    `windows_subsystem = "windows"` attribute in src/main.rs from being
    dropped in a future refactor.

.PARAMETER Path
    Path to the .exe to inspect.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Path
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Path)) {
    Write-Error "Executable not found: $Path"
    exit 1
}

$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $Path))

if ($bytes.Length -lt 0x40 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
    Write-Error 'Not a PE image: missing the MZ DOS header.'
    exit 1
}

# e_lfanew at 0x3C is the file offset of the NT headers.
$peOffset = [System.BitConverter]::ToInt32($bytes, 0x3C)

if ($peOffset -lt 0 -or ($peOffset + 4) -ge $bytes.Length) {
    Write-Error "Invalid e_lfanew (0x{0:X}); file is truncated or not a PE image." -f $peOffset
    exit 1
}

if ($bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or
    $bytes[$peOffset + 2] -ne 0x00 -or $bytes[$peOffset + 3] -ne 0x00) {
    Write-Error 'Not a PE image: missing the "PE\0\0" signature.'
    exit 1
}

# Optional header starts 24 bytes past the PE signature (20-byte COFF header + 4).
$optionalHeader = $peOffset + 24
$magic = [System.BitConverter]::ToUInt16($bytes, $optionalHeader)

if ($magic -ne 0x10B -and $magic -ne 0x20B) {
    Write-Error ("Unexpected optional header magic 0x{0:X}; expected 0x10B (PE32) or 0x20B (PE32+)." -f $magic)
    exit 1
}

# Subsystem sits 68 bytes into the optional header for both PE32 and PE32+.
$subsystem = [System.BitConverter]::ToUInt16($bytes, $optionalHeader + 68)

$names = @{
    2 = 'WINDOWS_GUI'
    3 = 'WINDOWS_CUI'
}

# Cast to [int] for the lookup: the value read from the header is a [uint16],
# and a [hashtable] key match is .Equals(), which is false across those types.
$name = if ($names.ContainsKey([int]$subsystem)) { $names[[int]$subsystem] } else { 'UNKNOWN' }

$peKind = if ($magic -eq 0x20B) { 'PE32+' } else { 'PE32' }
Write-Host "$Path -> $peKind, subsystem $subsystem ($name)"

if ($subsystem -ne 2) {
    if ($subsystem -eq 3) {
        Write-Error @'
The executable is a console application, so a terminal window appears when it
starts. Add this to the crate root in src/main.rs:

    #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
'@
    }
    else {
        Write-Error "Unexpected subsystem $subsystem ($name); expected 2 (WINDOWS_GUI)."
    }
    exit 1
}

Write-Host 'OK: GUI subsystem, so launching this exe will not open a console window.'
exit 0

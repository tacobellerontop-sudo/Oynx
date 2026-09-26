<#
.SYNOPSIS
    Fails unless a live top-level window owned by $ProcessId matches $ExpectedTitle.

.DESCRIPTION
    Used by CI to prove the GUI actually opened, not merely that the process
    survived. A crash during eframe/winit startup is indistinguishable from a
    healthy process if you only check liveness, because a few different
    startup failures all end in "process no longer running" with no output.

    Enumerates windows on the current window station and desktop, so it works
    on a headless runner where nothing is visible on screen.

.PARAMETER ProcessId
    PID of the running app.

.PARAMETER ExpectedTitle
    Substring the window title must contain. Pass $null to accept any window.

.PARAMETER TimeoutSeconds
    How long to keep re-checking before giving up. Windows can take a moment
    to appear, especially on a loaded CI runner.

.EXAMPLE
    .\assert-window.ps1 -ProcessId 1234 -ExpectedTitle 'Oynx' -TimeoutSeconds 60
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [int]$ProcessId,

    [AllowNull()]
    [string]$ExpectedTitle,

    [int]$TimeoutSeconds = 60
)

$ErrorActionPreference = 'Stop'

if (-not ('AssertWindow.Native' -as [type])) {
    Add-Type -Namespace 'AssertWindow' -Name 'Native' -MemberDefinition @'
[DllImport("user32.dll")]
public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, System.IntPtr lParam);

public delegate bool EnumWindowsProc(System.IntPtr hWnd, System.IntPtr lParam);

[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern int GetWindowTextLength(System.IntPtr hWnd);

[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern int GetWindowText(System.IntPtr hWnd, System.Text.StringBuilder text, int count);

[DllImport("user32.dll")]
public static extern uint GetWindowThreadProcessId(System.IntPtr hWnd, out uint processId);

[DllImport("user32.dll")]
public static extern bool IsWindowVisible(System.IntPtr hWnd);

[DllImport("user32.dll")]
public static extern bool IsIconic(System.IntPtr hWnd);

[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern int GetClassName(System.IntPtr hWnd, System.Text.StringBuilder name, int count);
'@
}

function Get-AppWindows {
    param([int]$OwnerProcessId)

    $found = [System.Collections.Generic.List[object]]::new()

    $callback = [AssertWindow.Native+EnumWindowsProc] {
        param([IntPtr]$hWnd, [IntPtr]$lParam)

        [uint32]$owner = 0
        [void][AssertWindow.Native]::GetWindowThreadProcessId($hWnd, [ref]$owner)

        if ($owner -eq $OwnerProcessId) {
            $length = [AssertWindow.Native]::GetWindowTextLength($hWnd)
            $title = ''
            if ($length -gt 0) {
                $builder = [System.Text.StringBuilder]::new($length + 1)
                [void][AssertWindow.Native]::GetWindowText($hWnd, $builder, $builder.Capacity)
                $title = $builder.ToString()
            }

            $class = [System.Text.StringBuilder]::new(256)
            [void][AssertWindow.Native]::GetClassName($hWnd, $class, $class.Capacity)

            $found.Add([pscustomobject]@{
                    Handle     = $hWnd
                    Title      = $title
                    Class      = $class.ToString()
                    Visible    = [AssertWindow.Native]::IsWindowVisible($hWnd)
                    Minimized  = [AssertWindow.Native]::IsIconic($hWnd)
                })
        }

        # Keep enumerating.
        return $true
    }

    [void][AssertWindow.Native]::EnumWindows($callback, [IntPtr]::Zero)
    return $found
}

if (-not (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue)) {
    Write-Error "Process $ProcessId is no longer running; the app exited during startup."
    exit 1
}

$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$startedAt = Get-Date
$match = $null
$seen = @()

while ((Get-Date) -lt $deadline) {
    $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if (-not $process) {
        Write-Error "Process $ProcessId exited after $([int]((Get-Date) - $startedAt).TotalSeconds) seconds."
        exit 1
    }

    $windows = @(Get-AppWindows -OwnerProcessId $ProcessId)
    $seen = $windows

    $candidates = $windows | Where-Object { $_.Title -or $_.Class -notlike '*tooltip*' }
    if ($null -eq $ExpectedTitle) {
        $match = $candidates | Select-Object -First 1
    }
    else {
        $match = $candidates | Where-Object { $_.Title -like "*$ExpectedTitle*" } | Select-Object -First 1
    }

    if ($match) {
        $elapsed = [int]((Get-Date) - $startedAt).TotalSeconds
        Write-Host "Found window '$($match.Title)' (class $($match.Class), visible=$($match.Visible)) after ${elapsed}s"
        $match | Format-List | Out-String | Write-Host
        exit 0
    }

    Start-Sleep -Seconds 2
}

Write-Host "Windows seen for PID ${ProcessId}:"
if ($seen.Count -eq 0) {
    Write-Host '  (none - the process never created a top-level window)'
}
else {
    $seen | Format-Table -AutoSize | Out-String | Write-Host
}

$titleNote = if ($null -eq $ExpectedTitle) { 'any' } else { "'$ExpectedTitle'" }
Write-Error "No top-level window matching $titleNote appeared within $TimeoutSeconds seconds."
exit 1

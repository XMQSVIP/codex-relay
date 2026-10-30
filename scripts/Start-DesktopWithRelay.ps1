param(
    [ValidateRange(1024, 65535)][int]$BackendPort = 4500,
    [ValidateRange(1024, 65535)][int]$DesktopPort = 4501,
    [switch]$VerifyLan
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$entry = $null
foreach ($candidate in @('codex-relay.exe', 'target\release\codex-relay.exe', 'target\debug\codex-relay.exe')) {
    $candidatePath = Join-Path $projectRoot $candidate
    if (Test-Path -LiteralPath $candidatePath -PathType Leaf) { $entry = $candidatePath; break }
}
if (-not $entry) { throw 'Build Codex Relay first: pnpm.cmd run build:release' }
if (-not (Test-Path -LiteralPath (Join-Path $projectRoot '.cache\launch-config.json') -PathType Leaf)) { throw 'Run pnpm run prepare:desktop while the normal desktop is still running.' }
if ($BackendPort -eq $DesktopPort) { throw 'BackendPort and DesktopPort must be different.' }

# This is an interactive supervisor. It never terminates an existing desktop.
Push-Location $projectRoot
try {
    $launchArguments = @('launch', '--backend-port', $BackendPort, '--desktop-port', $DesktopPort)
    if ($VerifyLan) { $launchArguments += '--verify-lan' }
    & $entry @launchArguments
    if ($LASTEXITCODE -ne 0) { throw "Shared runtime exited with code $LASTEXITCODE. See the error above." }
} finally { Pop-Location }

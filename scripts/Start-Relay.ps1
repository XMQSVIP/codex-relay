param(
    [ValidateRange(1024, 65535)][int]$Port,
    [string]$NetworkConfig,
    [switch]$RequireAdminToken
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$entry = $null
foreach ($candidate in @('codex-relay.exe', 'target\release\codex-relay.exe', 'target\debug\codex-relay.exe')) {
    $candidatePath = Join-Path $projectRoot $candidate
    if (Test-Path -LiteralPath $candidatePath -PathType Leaf) { $entry = $candidatePath; break }
}
if (-not $entry) { throw 'Build the project first: pnpm.cmd run build:release' }
if (-not (Test-Path -LiteralPath (Join-Path $projectRoot 'web\dist\index.html') -PathType Leaf)) { throw 'Run pnpm build:web first.' }
Push-Location $projectRoot
try {
    $bridgeArguments = @('serve')
    if ($PSBoundParameters.ContainsKey('Port')) { $bridgeArguments += @('--port', $Port) }
    if ($NetworkConfig) { $bridgeArguments += @('--network-config', $NetworkConfig) }
    if ($RequireAdminToken) { $bridgeArguments += '--require-admin-token' }
    & $entry @bridgeArguments
    if ($LASTEXITCODE -ne 0) { throw "Codex Relay exited with code $LASTEXITCODE." }
} finally { Pop-Location }

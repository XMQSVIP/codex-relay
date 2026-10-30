$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$entry = $null
foreach ($candidate in @('codex-relay.exe', 'target\release\codex-relay.exe', 'target\debug\codex-relay.exe')) {
    $candidatePath = Join-Path $projectRoot $candidate
    if (Test-Path -LiteralPath $candidatePath -PathType Leaf) { $entry = $candidatePath; break }
}
if (-not $entry) { throw 'Build Codex Relay first: pnpm.cmd run build:release' }
Write-Host 'Keep the official Codex desktop running. This detects its native backend; it does not send model requests.'
Push-Location $projectRoot
try {
    & $entry prepare
    if ($LASTEXITCODE -ne 0) { throw 'Preparation failed. Check that the supported Codex desktop is installed and running.' }
    Write-Host 'Preparation complete. You can now open Codex Relay and configure a model route.'
} finally { Pop-Location }

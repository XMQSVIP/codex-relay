param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $projectRoot
try {
    if (-not $SkipBuild) {
        & pnpm build:web
        if ($LASTEXITCODE -ne 0) { throw 'Web build failed.' }
        $previousFlags = $env:RUSTFLAGS
        try {
            $env:RUSTFLAGS = '-C target-feature=+crt-static'
            & node scripts/cargo-with-memory.mjs build --release --locked
            if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
        } finally { $env:RUSTFLAGS = $previousFlags }
    }
    & python scripts/package_portable.py
    if ($LASTEXITCODE -ne 0) { throw 'Packaging failed.' }
} finally { Pop-Location }

param(
    [ValidateRange(1024, 65535)][int]$Port,
    [switch]$NoBrowser
)

$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$entry = $null
foreach ($candidate in @('codex-relay.exe', 'target\release\codex-relay.exe', 'target\debug\codex-relay.exe')) {
    $candidatePath = Join-Path $projectRoot $candidate
    if (Test-Path -LiteralPath $candidatePath -PathType Leaf) { $entry = $candidatePath; break }
}
if (-not $entry) { throw 'Build the project first: pnpm.cmd run build:release' }
Push-Location $projectRoot
try {
    $resolveArguments = @('serve', '--print-port')
    if ($PSBoundParameters.ContainsKey('Port')) { $resolveArguments += @('--port', "$Port") }
    $resolvedPort = & $entry @resolveArguments
    if ($LASTEXITCODE -ne 0) { throw 'Invalid relay.config.json or port. See the error above.' }
    $Port = [int]$resolvedPort
} finally { Pop-Location }
$cache = Join-Path $projectRoot '.cache'
$stdout = Join-Path $cache "launcher-$Port.stdout.log"
$stderr = Join-Path $cache "launcher-$Port.stderr.log"
$baseUrl = "http://127.0.0.1:$Port/"

function Get-BridgeHealth {
    $request = [System.Net.WebRequest]::Create("${baseUrl}healthz")
    $request.Proxy = $null
    $request.Timeout = 1500
    $request.ReadWriteTimeout = 1500
    $response = $null
    $reader = $null
    try {
        $response = $request.GetResponse()
        $reader = New-Object System.IO.StreamReader($response.GetResponseStream())
        $health = $reader.ReadToEnd() | ConvertFrom-Json
        return ($health.status -eq 'ok' -and $health.implementation -eq 'rust')
    } catch { return $false }
    finally {
        if ($reader) { $reader.Dispose() }
        if ($response) { $response.Dispose() }
    }
}

try {
    if (-not $entry) { throw 'Build the project first: pnpm.cmd run build:release' }
    if (-not (Test-Path -LiteralPath (Join-Path $projectRoot 'web\dist\index.html'))) { throw 'Build the web interface first: pnpm build:web' }
    $listeners = @(Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue)
    if ($listeners.Count) {
        foreach ($listener in $listeners) {
            $owner = Get-Process -Id $listener.OwningProcess -ErrorAction Stop
            if ($owner.Path -ne $entry -or $listener.LocalAddress -notin @('127.0.0.1', '0.0.0.0')) {
                throw "Port $Port is occupied by another service. No process was stopped."
            }
        }
        if (-not (Get-BridgeHealth)) { throw 'Codex Relay is running but is not ready. Check the existing service window or logs.' }
        Write-Host 'Codex Relay is already running.'
    } else {
        New-Item -ItemType Directory -Path $cache -Force | Out-Null
        $bridgeArguments = @('serve', '--port', "$Port")
        if (Test-Path -LiteralPath (Join-Path $projectRoot 'data\network.json')) {
            $bridgeArguments += @('--network-config', 'data/network.json')
        }
        # Prevent simultaneous launcher clicks from racing over the same log files.
        $lockPath = Join-Path $cache "launcher-$Port.lock"
        $launchLock = [System.IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
        try {
            if (@(Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue).Count) {
                throw 'A service started while launching. Double-click the launcher again.'
            }
            $bridgeProcess = Start-Process -FilePath $entry -ArgumentList $bridgeArguments -WorkingDirectory $projectRoot -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
            Set-Content -LiteralPath (Join-Path $cache 'rust-admin.pid') -Value $bridgeProcess.Id
            $deadline = (Get-Date).AddSeconds(25)
            while (-not (Get-BridgeHealth)) {
                $bridgeProcess.Refresh()
                if ($bridgeProcess.HasExited) { throw "Codex Relay failed to start. See $stderr" }
                if ((Get-Date) -gt $deadline) { throw "Startup is taking longer than expected. See $stdout and $stderr" }
                Start-Sleep -Milliseconds 250
            }
        } finally { $launchLock.Dispose() }
        Write-Host 'Codex Relay started in the background.'
    }
    Write-Host "Management: $baseUrl"
    if (-not $NoBrowser) { Start-Process $baseUrl }
} catch {
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
}

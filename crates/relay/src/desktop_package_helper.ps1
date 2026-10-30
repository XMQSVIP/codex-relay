$ErrorActionPreference = 'Stop'
$request = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__REQUEST_BASE64__')) | ConvertFrom-Json
try {
    if ([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() -gt $request.deadline) { exit 0 }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $request.executable
    $start.WorkingDirectory = Split-Path $request.executable
    $start.UseShellExecute = $false
    foreach ($key in @($start.EnvironmentVariables.Keys)) {
        if ($key -like 'CODEX_*' -and $key -ne 'CODEX_HOME') { $start.EnvironmentVariables.Remove($key) }
    }
    $start.EnvironmentVariables['CODEX_APP_SERVER_WS_URL'] = $request.url
    $start.EnvironmentVariables['NO_PROXY'] = $start.EnvironmentVariables['NO_PROXY'] + ',127.0.0.1,localhost,::1'
    $process = [Diagnostics.Process]::Start($start)
    $result = @{pid=$process.Id}
    $process.Dispose()
} catch {
    $cause = $_.Exception
    while ($cause.InnerException) { $cause = $cause.InnerException }
    $result = @{win32=$cause.NativeErrorCode; hresult=$cause.HResult}
}
$temporary = $request.result + '.tmp'
[IO.File]::WriteAllText($temporary, ($result | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
[IO.File]::Move($temporary, $request.result)

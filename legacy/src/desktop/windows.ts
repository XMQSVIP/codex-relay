import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { z } from 'zod';
import { BridgeError } from '../errors.js';

const execFileAsync = promisify(execFile);
const processSchema = z.object({
  pid: z.number().int().positive(),
  parentPid: z.number().int().nonnegative(),
  name: z.string(),
  executable: z.string().nullable(),
  isAppServer: z.boolean(),
});
export type DesktopProcess = z.infer<typeof processSchema>;
export const snapshotSchema = z.object({
  processes: z.array(processSchema),
  pipes: z.array(z.string()),
  listeners: z.array(z.object({ pid: z.number(), port: z.number(), address: z.string() })),
});
export type WindowsSnapshot = z.infer<typeof snapshotSchema>;

export async function powershell(script: string, env: NodeJS.ProcessEnv = process.env): Promise<string> {
  const { stdout } = await execFileAsync('powershell.exe', [
    '-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64'),
  ], { windowsHide: true, timeout: 15_000, maxBuffer: 4 * 1024 * 1024, env });
  return stdout.replace(/^\uFEFF/, '').trim();
}

export async function discoverWindows(): Promise<WindowsSnapshot> {
  if (process.platform !== 'win32') throw new BridgeError('PLATFORM', 'This desktop adapter requires Windows.');
  // Never return full command lines: they can contain credentials and host configuration.
  const output = await powershell(`
$ErrorActionPreference = 'Stop'
$processes = @(Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^(codex|ChatGPT|node)\\.exe$' } | ForEach-Object {
  [pscustomobject]@{ pid=[int]$_.ProcessId; parentPid=[int]$_.ParentProcessId; name=$_.Name; executable=$_.ExecutablePath; isAppServer=[bool]($_.Name -ieq 'codex.exe' -and $_.CommandLine -match '\\bapp-server\\b') }
})
$pipes = @(Get-ChildItem -LiteralPath '\\\\.\\pipe\\' | Where-Object { $_.Name -like 'codex-app-tools-*' } | ForEach-Object { '\\\\.\\pipe\\' + $_.Name })
$listeners = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $_.OwningProcess -in $processes.pid } | ForEach-Object {
  [pscustomobject]@{ pid=[int]$_.OwningProcess; port=[int]$_.LocalPort; address=$_.LocalAddress }
})
[pscustomobject]@{ processes=$processes; pipes=$pipes; listeners=$listeners } | ConvertTo-Json -Depth 5 -Compress
`);
  return snapshotSchema.parse(JSON.parse(output));
}

export function findDesktopOwners(snapshot: WindowsSnapshot): DesktopProcess[] {
  // A shared backend is no longer a desktop child. Exclude Electron subprocesses by parent image.
  return snapshot.processes.filter(p => /^(chatgpt|codex)\.exe$/i.test(p.name) && !p.isAppServer
    && !!p.executable && /[\\/]WindowsApps[\\/]OpenAI\.Codex_/i.test(p.executable)
    && !snapshot.processes.some(parent => parent.pid === p.parentPid && parent.executable === p.executable));
}

export async function verifyDesktopTcpPeer(peerPort: number, serverPort: number, pid: number, executable: string): Promise<boolean> {
  if (![peerPort, serverPort, pid].every(n => Number.isSafeInteger(n) && n > 0)) return false;
  const output = await powershell(`
$ErrorActionPreference = 'Stop'
$connection = @(Get-NetTCPConnection -State Established -LocalAddress '127.0.0.1' -LocalPort $env:CODEX_BRIDGE_PEER_PORT -RemoteAddress '127.0.0.1' -RemotePort $env:CODEX_BRIDGE_SERVER_PORT -ErrorAction SilentlyContinue)
$owner = Get-CimInstance Win32_Process -Filter ("ProcessId = " + $env:CODEX_BRIDGE_DESKTOP_PID)
if ($connection.Count -eq 1 -and $connection[0].OwningProcess -eq [int]$env:CODEX_BRIDGE_DESKTOP_PID -and $owner.ExecutablePath -eq $env:CODEX_BRIDGE_DESKTOP_EXE) { Write-Output 'verified' }
`, { ...process.env, CODEX_BRIDGE_PEER_PORT: String(peerPort), CODEX_BRIDGE_SERVER_PORT: String(serverPort), CODEX_BRIDGE_DESKTOP_PID: String(pid), CODEX_BRIDGE_DESKTOP_EXE: executable });
  return output === 'verified';
}

export function validatePipeName(pipe: string): void {
  if (!/^\\\\\.\\pipe\\codex-app-tools-[a-zA-Z0-9-]+$/.test(pipe)) {
    throw new BridgeError('PIPE_NAME', 'Only local codex-app-tools named pipes are accepted.');
  }
}

/** Verify the pipe server's PID; do not trust environment variables or a matching name. */
export async function pipeServerPid(pipe: string): Promise<number> {
  validatePipeName(pipe);
  const output = await powershell(`
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CodexBridgePipeIdentity {
  [DllImport("kernel32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool GetNamedPipeServerProcessId(IntPtr pipe, out uint processId);
}
'@
$pipeName = $env:CODEX_BRIDGE_PROBE_PIPE.Substring(9)
$stream = [System.IO.Pipes.NamedPipeClientStream]::new('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::Asynchronous)
try {
  $stream.Connect(2000)
  [uint32]$serverProcessId = 0
  if (-not [CodexBridgePipeIdentity]::GetNamedPipeServerProcessId($stream.SafePipeHandle.DangerousGetHandle(), [ref]$serverProcessId)) { throw 'Cannot verify pipe owner' }
  Write-Output $serverProcessId
} finally { $stream.Dispose() }
`, { ...process.env, CODEX_BRIDGE_PROBE_PIPE: pipe });
  const pid = Number(output);
  if (!Number.isSafeInteger(pid) || pid <= 0) throw new BridgeError('PIPE_OWNER', 'Invalid native pipe server identity.');
  return pid;
}

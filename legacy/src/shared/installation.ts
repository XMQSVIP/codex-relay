import { createReadStream } from 'node:fs';
import { mkdir, readFile, writeFile, access } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { z } from 'zod';
import { discoverWindows, findDesktopOwners, pipeServerPid, powershell } from '../desktop/windows.js';
import { inspectArchive } from '../desktop/asar.js';
import { BridgeError } from '../errors.js';

const installationSchema = z.object({
  format: z.literal(1), preparedAt: z.string(), desktopExecutable: z.string(), appServerExecutable: z.string(),
  appServerSha256: z.string().regex(/^[a-f0-9]{64}$/), archive: z.string(),
  sourceFiles: z.array(z.object({ path: z.string(), sha256: z.string() })),
});
export type Installation = z.infer<typeof installationSchema>;

async function hashFile(filename: string): Promise<string> {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(filename)) hash.update(chunk);
  return hash.digest('hex');
}

export async function prepareInstallation(desktopPid?: number): Promise<Installation> {
  const snapshot = await discoverWindows();
  if (desktopPid === undefined && process.env.CODEX_APP_TOOLS_PIPE_PATH) {
    try { desktopPid = await pipeServerPid(process.env.CODEX_APP_TOOLS_PIPE_PATH); } catch { /* Explicit selection below remains available. */ }
  }
  const desktops = findDesktopOwners(snapshot).filter(p => desktopPid === undefined || p.pid === desktopPid);
  if (desktops.length !== 1 || !desktops[0]?.executable) throw new BridgeError('DESKTOP_SELECTION', 'Run prepare while the normal desktop is running, or select --desktop-pid.');
  const desktop = desktops[0];
  const backends = snapshot.processes.filter(p => p.isAppServer && p.parentPid === desktop.pid && !!p.executable);
  if (backends.length !== 1 || !backends[0]?.executable) throw new BridgeError('DESKTOP_BACKEND', 'Cannot identify the exact CLI executable used by the normal desktop.');
  const archive = path.join(path.dirname(desktop.executable!), 'resources', 'app.asar');
  const source = await inspectArchive(archive);
  if (!source.files.some(f => f.sharedWebSocketOverride)) throw new BridgeError('DESKTOP_WS_UNSUPPORTED', 'This installed desktop has no recognized shared WebSocket override.');
  const installation: Installation = {
    format: 1, preparedAt: new Date().toISOString(), desktopExecutable: desktop.executable!,
    appServerExecutable: backends[0].executable, appServerSha256: await hashFile(backends[0].executable), archive,
    sourceFiles: source.files.map(f => ({ path: f.path, sha256: f.sha256 })),
  };
  await mkdir('.cache', { recursive: true });
  await writeFile('.cache/launch-config.json', JSON.stringify(installation, null, 2) + '\n');
  return installation;
}

export async function readInstallation(): Promise<Installation> {
  try { return installationSchema.parse(JSON.parse(await readFile('.cache/launch-config.json', 'utf8'))); }
  catch { throw new BridgeError('LAUNCH_CONFIG', 'Run pnpm run prepare:desktop from the normal desktop before switching transports.'); }
}

export async function validateInstallation(installation: Installation): Promise<void> {
  await access(installation.desktopExecutable);
  const installed = await powershell(`
$package = Get-AppxPackage -Name 'OpenAI.Codex' | Sort-Object Version -Descending | Select-Object -First 1
if ($package) { Write-Output $package.InstallLocation }
`);
  if (!installed || path.resolve(installation.desktopExecutable).toLowerCase() !== path.join(installed, 'app', path.basename(installation.desktopExecutable)).toLowerCase()) {
    throw new BridgeError('DESKTOP_VERSION', 'Installed desktop version changed. Re-run prepare from its normal launch.');
  }
  if (await hashFile(installation.appServerExecutable) !== installation.appServerSha256) throw new BridgeError('CLI_VERSION', 'The prepared app-server executable changed.');
  const source = await inspectArchive(installation.archive);
  if (JSON.stringify(source.files.map(f => ({ path: f.path, sha256: f.sha256 }))) !== JSON.stringify(installation.sourceFiles)) {
    throw new BridgeError('DESKTOP_VERSION', 'The prepared desktop bundles changed.');
  }
}

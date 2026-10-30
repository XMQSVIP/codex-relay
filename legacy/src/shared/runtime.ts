import { spawn } from 'node:child_process';
import { mkdir, open, readFile, rename, unlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { z } from 'zod';
import { readInstallation, validateInstallation } from './installation.js';
import { cleanChildEnvironment, startBackend } from './backend.js';
import { startDesktopProxy } from '../transport/desktop-proxy.js';
import { discoverWindows, findDesktopOwners, verifyDesktopTcpPeer } from '../desktop/windows.js';
import { BridgeError, errorCode } from '../errors.js';
import { verifySharedLan } from './lan-verification.js';

export const runtimeStatusSchema = z.object({
  format: z.literal(1), updatedAt: z.string(), supervisorPid: z.number().int().positive(),
  backendPid: z.number().int().positive().nullable(), desktopPid: z.number().int().positive().nullable(),
  backendPort: z.number().int(), desktopPort: z.number().int(),
  state: z.enum(['starting', 'waiting-for-desktop', 'desktop-connected', 'stopping', 'stopped', 'failed']),
  connections: z.number().int().nonnegative(), initializedConnections: z.number().int().nonnegative(),
  inferenceEnabled: z.literal(false),
  lanVerification: z.enum(['not-requested', 'waiting-for-desktop', 'running', 'passed', 'failed']).default('not-requested'),
});
export type RuntimeStatus = z.infer<typeof runtimeStatusSchema>;

export async function readRuntimeStatus(): Promise<RuntimeStatus | null> {
  try { return runtimeStatusSchema.parse(JSON.parse(await readFile('.cache/shared-runtime.json', 'utf8'))); }
  catch { return null; }
}

export async function runSharedRuntime(options: { backendPort: number; desktopPort: number; verifyLan?: boolean }): Promise<void> {
  if (options.backendPort === options.desktopPort) throw new BridgeError('PORT_CONFLICT', 'Backend and desktop ports must be different.');
  const installation = await readInstallation();
  await validateInstallation(installation);
  if (findDesktopOwners(await discoverWindows()).length) throw new BridgeError('DESKTOP_RUNNING', 'Save work and fully quit Codex/ChatGPT before launching the shared runtime. No process was stopped.');
  await mkdir('.cache', { recursive: true });
  // Exclusive local startup lock. Never kill a process based only on a stale PID in a file.
  let lock;
  try { lock = await open('.cache/shared-runtime.lock', 'wx'); }
  catch { throw new BridgeError('RUNTIME_LOCKED', 'A runtime lock exists. Check the recorded supervisor PID before removing a stale lock.'); }
  await lock.writeFile(String(process.pid));
  await lock.close();
  const state: RuntimeStatus = {
    format: 1, updatedAt: new Date().toISOString(), supervisorPid: process.pid,
    backendPid: null, desktopPid: null, backendPort: options.backendPort, desktopPort: options.desktopPort,
    state: 'starting', connections: 0, initializedConnections: 0, inferenceEnabled: false,
    lanVerification: options.verifyLan ? 'waiting-for-desktop' : 'not-requested',
  };
  let writes = Promise.resolve();
  const persist = () => {
    state.updatedAt = new Date().toISOString();
    const data = JSON.stringify(state, null, 2) + '\n';
    writes = writes.then(async () => { await writeFile('.cache/shared-runtime.json.tmp', data); await rename('.cache/shared-runtime.json.tmp', '.cache/shared-runtime.json'); });
    // The awaited chain still rejects at shutdown; suppress premature unhandled rejection.
    void writes.catch(() => {});
    return writes;
  };
  let backend: Awaited<ReturnType<typeof startBackend>> | undefined;
  let proxy: Awaited<ReturnType<typeof startDesktopProxy>> | undefined;
  let verification: Promise<void> | undefined;
  let retainLock = false;
  let markDesktopReady: () => void = () => {};
  const desktopReady = new Promise<'ready'>(resolve => { markDesktopReady = () => resolve('ready'); });
  let stopRequested = false;
  let signalStop: () => void = () => {};
  const stopped = new Promise<'stop-requested'>(resolve => { signalStop = () => { stopRequested = true; resolve('stop-requested'); }; });
  process.once('SIGINT', signalStop); process.once('SIGTERM', signalStop);
  try {
    await persist();
    backend = await startBackend({ executable: installation.appServerExecutable, port: options.backendPort, cwd: path.dirname(installation.appServerExecutable) });
    if (stopRequested) return;
    state.backendPid = backend.child.pid ?? null;
    proxy = await startDesktopProxy({
      port: options.desktopPort, backendUrl: backend.endpoint, backendToken: backend.token,
      authorizePeer: (peerPort, serverPort) => state.desktopPid === null ? Promise.resolve(false)
        : verifyDesktopTcpPeer(peerPort, serverPort, state.desktopPid, installation.desktopExecutable),
      onState: connection => {
        Object.assign(state, connection);
        if (!['stopping', 'stopped', 'failed'].includes(state.state)) state.state = connection.initializedConnections > 0 ? 'desktop-connected' : 'waiting-for-desktop';
        if (connection.initializedConnections > 0) markDesktopReady();
        void persist();
      },
    });
    if (stopRequested) return;
    const env = cleanChildEnvironment();
    env.CODEX_APP_SERVER_WS_URL = proxy.url;
    // Per-process only: ordinary desktop launches continue to use their original transport.
    const desktop = spawn(installation.desktopExecutable, [], { env, cwd: path.dirname(installation.desktopExecutable), windowsHide: true, stdio: 'ignore' });
    const desktopExited = new Promise<'desktop-exited' | 'desktop-error'>(resolve => {
      desktop.once('exit', () => resolve('desktop-exited')); desktop.once('error', () => resolve('desktop-error'));
    });
    state.desktopPid = desktop.pid ?? null;
    state.state = 'waiting-for-desktop';
    await persist();
    console.log(JSON.stringify({ event: 'shared-runtime-started', supervisorPid: process.pid, backendPid: state.backendPid, desktopPid: state.desktopPid, inferenceEnabled: false }));
    console.log('Keep this supervisor running while the desktop uses it. Stop it after quitting the desktop. Ordinary desktop launch restores the previous transport.');
    const backendExited = backend.exited.then(() => 'backend-exited' as const);
    if (options.verifyLan) {
      const ownedBackend = backend;
      verification = (async () => {
        if (await Promise.race([desktopReady, desktopExited, backendExited, stopped]) !== 'ready' || stopRequested) return;
        state.lanVerification = 'running';
        try {
          await persist();
          await writeFile('.cache/lan-verification.json.tmp', JSON.stringify({ purpose: 'shared-desktop-real-lan-verification', startedAt: new Date().toISOString(), state: 'running', inferenceEnabled: false }) + '\n');
          await rename('.cache/lan-verification.json.tmp', '.cache/lan-verification.json');
          if (state.desktopPid === null) throw new BridgeError('LAN_DESKTOP_REQUIRED', 'No verified shared desktop PID.');
          const report = await verifySharedLan({ backend: ownedBackend, executable: installation.appServerExecutable, desktopPid: state.desktopPid,
            desktopConnected: () => !stopRequested && state.state === 'desktop-connected' && state.initializedConnections > 0,
            progress: step => console.log(JSON.stringify({ event: 'shared-lan-check', step })),
          });
          await writeFile('.cache/lan-verification.json.tmp', JSON.stringify(report, null, 2) + '\n');
          await rename('.cache/lan-verification.json.tmp', '.cache/lan-verification.json');
          state.lanVerification = report.liveChecksPassed ? 'passed' : 'failed';
          console.log(JSON.stringify({ event: 'shared-lan-check-finished', passed: report.liveChecksPassed, report: '.cache/lan-verification.json', inferenceEnabled: false }));
        } catch (error) {
          state.lanVerification = 'failed';
          console.log(JSON.stringify({ event: 'shared-lan-check-failed', code: errorCode(error), inferenceEnabled: false }));
          await writeFile('.cache/lan-verification.json', JSON.stringify({ purpose: 'shared-desktop-real-lan-verification', checkedAt: new Date().toISOString(), state: 'failed', code: errorCode(error), inferenceEnabled: false }) + '\n').catch(() => {});
        }
        await persist();
      })();
      void verification.catch(() => {});
    }
    const reason = await Promise.race([desktopExited, backendExited, stopped]);
    if (reason === 'backend-exited' || reason === 'desktop-error') throw new BridgeError('RUNTIME_EXIT', `Shared runtime stopped (${reason}). The desktop process was not terminated by the bridge.`);
  } catch (error) { state.state = 'failed'; retainLock = errorCode(error) === 'BACKEND_STOP'; throw error; }
  finally {
    signalStop();
    process.off('SIGINT', signalStop); process.off('SIGTERM', signalStop);
    if (state.state !== 'failed') state.state = 'stopping';
    let backendStopped = false;
    try {
      try { await proxy?.close(); } finally { await backend?.stop(); backendStopped = true; }
      await verification;
      if (state.state !== 'failed') state.state = 'stopped';
      await persist();
    } finally {
      // Keep the lock if the owned backend could not be stopped; never start a second owner accidentally.
      if (backendStopped && !retainLock) await unlink('.cache/shared-runtime.lock');
    }
  }
}

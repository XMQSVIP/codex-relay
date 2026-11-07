import path from 'node:path';
import { errorCode } from './errors.js';
import { discoverWindows, findDesktopOwners, pipeServerPid, type WindowsSnapshot } from './desktop/windows.js';
import { listNativeTools } from './transport/native.js';
import { inspectNativeCapabilities, releaseGate, type Check } from './capabilities.js';
import { readRuntimeStatus } from './shared/runtime.js';

export function selectDesktop(snapshot: WindowsSnapshot, pid?: number) {
  const owners = findDesktopOwners(snapshot).filter(p => pid === undefined || pid === p.pid);
  return owners.length === 1 ? owners[0] : undefined;
}

export async function doctor(options: { desktopPid?: number } = {}) {
  const checks: Check[] = [];
  const report = {
    reportVersion: 2, checkedAt: new Date().toISOString(), purpose: 'read-only-discovery',
    runtime: { node: process.version, platform: process.platform },
    desktop: null as { pid: number; executable: string; archive: string } | null,
    native: { connected: false, toolCount: 0 },
    shared: await readRuntimeStatus(), checks, gate: releaseGate(checks), nextActions: [] as string[],
  };
  try {
    const snapshot = await discoverWindows();
    let pid = options.desktopPid;
    if (pid === undefined && process.env.CODEX_APP_TOOLS_PIPE_PATH) {
      try { pid = await pipeServerPid(process.env.CODEX_APP_TOOLS_PIPE_PATH); } catch { /* Fall back to unambiguous process discovery. */ }
    }
    const owner = selectDesktop(snapshot, pid);
    if (!owner?.executable) {
      checks.push({ id: 'desktop_process', state: 'blocked', detail: 'Cannot select exactly one official desktop main process. Start Codex or choose --desktop-pid.' });
    } else {
      report.desktop = { pid: owner.pid, executable: owner.executable, archive: path.join(path.dirname(owner.executable), 'resources', 'app.asar') };
      checks.push({ id: 'desktop_process', state: 'passed', detail: 'Found the official desktop main process; no child app-server or debug port is required.' });
      const candidatePipes = [...new Set([process.env.CODEX_APP_TOOLS_PIPE_PATH, ...snapshot.pipes].filter((p): p is string => !!p))];
      for (const pipe of candidatePipes.slice(0, 16)) {
        try {
          if (await pipeServerPid(pipe) !== owner.pid) continue;
          const tools = await listNativeTools(pipe);
          if (await pipeServerPid(pipe) !== owner.pid) continue;
          report.native = { connected: true, toolCount: tools.length };
          checks.push({ id: 'native_pipe_owner', state: 'passed', detail: 'Read-only native tools discovery belongs to the selected desktop PID.' }, ...inspectNativeCapabilities(tools));
          break;
        } catch { /* A stale or inaccessible pipe cannot establish identity. */ }
      }
      const shared = report.shared;
      const listeners = shared ? snapshot.listeners.filter(l => l.port === shared.backendPort) : [];
      const ownedBackend = shared && snapshot.processes.some(p => p.pid === shared.backendPid && p.parentPid === shared.supervisorPid && p.isAppServer);
      const proxyListeners = shared ? snapshot.listeners.filter(l => l.port === shared.desktopPort) : [];
      const ownedProxy = !!shared && proxyListeners.length > 0 && proxyListeners.every(l => l.pid === shared.supervisorPid && l.address === '127.0.0.1');
      const connected = !!shared && shared.desktopPid === owner.pid && shared.state === 'desktop-connected'
        && shared.initializedConnections > 0 && ownedBackend && ownedProxy && listeners.length > 0
        && listeners.every(l => l.pid === shared.backendPid && l.address === '127.0.0.1');
      checks.push({ id: 'shared_desktop_connection', state: connected ? 'passed' : 'unverified', detail: connected
        ? 'Supervisor recorded a process-verified desktop connection and successful initialize; the owned backend is still listening locally.'
        : 'The current desktop has not been verified on the shared transport. Prepare, fully quit the desktop, then use the shared launcher.' });
    }
    checks.push(
      { id: 'same_desktop_instance', state: 'unverified', detail: 'Shared transport discovery alone does not prove an actual bridge turn is visible and executed in this desktop.' },
      { id: 'live_text_before_completion', state: 'unverified', detail: 'Real text deltas must precede the terminal event. No inference has been dispatched.' },
      { id: 'exact_turn_correlation', state: 'unverified', detail: 'Actual inference must match thread, turn and item IDs.' },
      { id: 'confirmed_interrupt', state: 'unverified', detail: 'Actual inference must reach an interrupted terminal state.' },
      { id: 'tools_disabled_before_dispatch', state: 'unverified', detail: 'Per-thread prevention of built-in, plugin and MCP tool execution has not been established. Read-only sandbox and empty dynamicTools are insufficient.' },
    );
    report.nextActions.push('Run pnpm run prepare:desktop while the normal desktop is running, then pnpm run probe:shared for an isolated protocol check.');
    report.nextActions.push('The user selected the existing LAN provider. Run inspect:provider for non-secret metadata and probe:provider for synthetic native auth/routing/cancellation checks; neither dispatches real model inference.');
    report.nextActions.push('After saving work and fully quitting the desktop, run scripts/Start-DesktopWithBridge.ps1 -VerifyLan in a separate PowerShell. It runs up to two real LAN diagnostic prompts after desktop initialization. Then inspect .cache/lan-verification.json and re-open this chat.');
    report.nextActions.push('Inference remains disabled until all five real acceptance checks pass. The proxy is not an externally accessible Responses endpoint.');
  } catch (error) {
    checks.push({ id: 'discovery', state: 'blocked', detail: `Desktop discovery failed (${errorCode(error)}).` });
  } finally { report.gate = releaseGate(checks); }
  return report;
}

import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import { BridgeError, errorCode } from '../errors.js';
import { releaseGate, type Check } from '../capabilities.js';
import { startLanTextGuard } from '../guard/loopback-guard.js';
import { verifyDesktopTcpPeer } from '../desktop/windows.js';
import { readLanProvider } from './provider.js';
import { runGuardedCheck } from './guarded-check.js';
import type { Backend } from './backend.js';

/** Runs only inside the supervisor with its already-connected desktop and owned backend. */
export async function verifySharedLan(options: {
  backend: Backend; executable: string; desktopPid: number;
  desktopConnected(): boolean;
  progress(step: string): void;
}) {
  const assertConnected = () => {
    if (!options.desktopConnected() || !options.backend.child.pid) throw new BridgeError('LAN_DESKTOP_REQUIRED', 'Real LAN checks require the connected shared desktop and its owned backend.');
  };
  assertConnected();
  const cwd = path.resolve('.cache/service-workspace'); await mkdir(cwd, { recursive: true });
  const provider = await readLanProvider(options.backend.rpc, cwd);
  const guard = await startLanTextGuard({ provider, timeoutMs: 90_000,
    authorizePeer: (peerPort, guardPort) => options.backend.child.pid
      ? verifyDesktopTcpPeer(peerPort, guardPort, options.backend.child.pid, options.executable) : Promise.resolve(false),
  });
  const checks: Check[] = [];
  const results: Awaited<ReturnType<typeof runGuardedCheck>>[] = [];
  try {
    assertConnected(); options.progress('text-stream');
    const text = await runGuardedCheck({ rpc: options.backend.rpc, guard, provider, cwd,
      prompt: '这是纯文本服务的连通性验证。请用中文分两段解释流式输出如何逐步显示文字，每段约一百字。只回答文字。',
    });
    results.push(text); assertConnected();
    const live = text.status === 'completed' && text.guard.state === 'completed' && text.guard.upstreamRequests === 1 && text.deltaCount > 0
      && text.firstDeltaAt !== null && text.guard.upstreamEndedAt !== null && text.firstDeltaAt < text.guard.upstreamEndedAt;
    checks.push({ id: 'live_text_before_completion', state: live ? 'passed' : 'blocked', detail: 'A text delta from the selected LAN model must reach the shared app-server client before the filtered upstream reaches EOF.' });
    checks.push({ id: 'exact_turn_correlation', state: text.exactCorrelation && text.status === 'completed' ? 'passed' : 'blocked', detail: 'Text events matched the native thread, turn and assistant item identifiers.' });
    if (!live || !text.exactCorrelation) throw new BridgeError('LAN_TEXT_CHECK', 'The real text check failed; no second diagnostic turn was submitted.');
    options.progress('interrupt');
    const interrupted = await runGuardedCheck({ rpc: options.backend.rpc, guard, provider, cwd, interruptAfterFirstDelta: true,
      prompt: '这是流式中断测试。请从 1 开始逐行编号，一直写到 1000，每行写一句不同的简短中文句子。只输出列表。',
    });
    results.push(interrupted); assertConnected();
    checks.push({ id: 'confirmed_interrupt', state: interrupted.status === 'interrupted' && interrupted.interruptAcknowledged && interrupted.guard.state === 'cancelled' ? 'passed' : 'blocked',
      detail: 'Cancellation must be acknowledged, reach native interrupted status, and close the upstream HTTP request.' });
    const after = await readLanProvider(options.backend.rpc, cwd);
    checks.push({ id: 'effective_provider_unchanged', state: provider.id === after.id && provider.model === after.model && provider.baseUrl === after.baseUrl ? 'passed' : 'blocked',
      detail: 'The native effective provider and model remain unchanged after both bridge-only thread overrides.' });
  } catch (error) { checks.push({ id: 'shared_lan_verification', state: 'blocked', detail: `Shared LAN verification stopped (${errorCode(error)}).` }); }
  finally { await guard.close(); }
  checks.push(
    { id: 'same_desktop_instance', state: 'unverified', detail: 'Both diagnostic turns used the process-verified shared desktop backend. Dedicated persistent project visibility and normal desktop functionality still need acceptance.' },
    { id: 'tools_disabled_before_dispatch', state: 'unverified', detail: 'The text guard is active on these native provider routes; final shared-desktop adversarial acceptance remains separate from synthetic tests.' },
  );
  return { purpose: 'shared-desktop-real-lan-verification', checkedAt: new Date().toISOString(),
    desktopPid: options.desktopPid, backendPid: options.backend.child.pid, provider, results, checks,
    liveChecksPassed: checks.filter(check => check.state !== 'unverified').length >= 4 && checks.every(check => check.state !== 'blocked'),
    gate: releaseGate(checks), inferenceEnabled: false,
    note: 'At most two fixed diagnostic prompts, submitted only after desktop initialization. This is not a public gateway or full desktop acceptance.' };
}

import { mkdtemp, mkdir } from 'node:fs/promises';
import { createServer } from 'node:net';
import path from 'node:path';
import { z } from 'zod';
import { startBackend } from './backend.js';
import { AppServerRpc } from '../transport/rpc.js';
import { errorCode } from '../errors.js';
import { releaseGate, type Check } from '../capabilities.js';

export async function unusedLoopbackPort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Missing port');
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

/** Uses a fresh home, no account credentials, no turn/start and no model inference. */
export async function probeSharedBackend(executable: string) {
  await mkdir('.cache/probes', { recursive: true });
  const home = await mkdtemp(path.resolve('.cache/probes/run-'));
  const cwd = path.join(home, 'workspace');
  await mkdir(cwd);
  const checks: Check[] = [];
  const backend = await startBackend({ executable, port: await unusedLoopbackPort(), cwd, home, isolated: true });
  let observer: AppServerRpc | undefined;
  try {
    checks.push({ id: 'authenticated_rpc', state: 'passed', detail: 'Real app-server initialized through an authenticated loopback WebSocket.' });
    const account = z.object({ account: z.null() }).safeParse(await backend.rpc.request('account/read', { refreshToken: false }));
    checks.push({ id: 'isolated_no_account', state: account.success ? 'passed' : 'blocked', detail: 'A fresh file-only credential store must have no signed-in account. Account details are never included in this report.' });
    if (!account.success) return { purpose: 'isolated-protocol-probe', checkedAt: new Date().toISOString(), home, checks, gate: releaseGate(checks) };
    try {
      const wrong = await AppServerRpc.connect(backend.endpoint, 'x'.repeat(43), 1500);
      wrong.close();
      checks.push({ id: 'reject_wrong_token', state: 'blocked', detail: 'Backend unexpectedly accepted a wrong token.' });
      return { purpose: 'isolated-protocol-probe', checkedAt: new Date().toISOString(), home, checks, gate: releaseGate(checks) };
    } catch { checks.push({ id: 'reject_wrong_token', state: 'passed', detail: 'A wrong capability token was rejected before initialization.' }); }
    observer = await AppServerRpc.connect(backend.endpoint, backend.token);
    await observer.initialize('codex_bridge_probe_observer');
    const response = z.object({
      thread: z.object({ id: z.string() }),
      approvalPolicy: z.string(),
      sandbox: z.object({ type: z.string() }).passthrough(),
    }).parse(await backend.rpc.request('thread/start', {
      cwd, ephemeral: true, approvalPolicy: 'never', sandbox: 'read-only',
      dynamicTools: [], environments: [], runtimeWorkspaceRoots: [], selectedCapabilityRoots: [],
    }));
    const loaded = z.object({ data: z.array(z.string()) }).parse(await observer.request('thread/loaded/list', { limit: 100 }));
    checks.push({ id: 'two_clients_share_thread', state: loaded.data.includes(response.thread.id) ? 'passed' : 'blocked', detail: 'Second authenticated connection checked the first connection\'s ephemeral thread in the shared in-memory thread list.' });
    checks.push({ id: 'thread_permission_acknowledgment', state: response.approvalPolicy === 'never' && response.sandbox.type === 'readOnly' ? 'passed' : 'blocked', detail: `App-server returned approvalPolicy=${response.approvalPolicy}, sandbox.type=${response.sandbox.type}. This does not establish absence of tools.` });
    checks.push({ id: 'tools_disabled_before_dispatch', state: 'unverified', detail: 'An empty dynamicTools list and read-only sandbox do not disable built-in tools. No inference was dispatched.' });
  } catch (error) {
    checks.push({ id: 'protocol_probe', state: 'blocked', detail: `Isolated probe stopped (${errorCode(error)}). No model inference was requested.` });
  } finally { observer?.close(); await backend.stop(); }
  return { purpose: 'isolated-protocol-probe', checkedAt: new Date().toISOString(), home, checks, gate: releaseGate(checks) };
}

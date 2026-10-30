import { createServer } from 'node:http';
import { once } from 'node:events';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { z } from 'zod';
import { startBackend } from './backend.js';
import { unusedLoopbackPort } from './probe.js';
import { startLoopbackTextGuard } from '../guard/loopback-guard.js';
import { verifyDesktopTcpPeer } from '../desktop/windows.js';
import { runGuardedCheck } from './guarded-check.js';
import { releaseGate, type Check } from '../capabilities.js';
import { errorCode } from '../errors.js';
import { setTimeout as delay } from 'node:timers/promises';
import type { LanProvider } from './provider.js';

const sse = (data: unknown) => `data: ${JSON.stringify(data)}\n\n`;

/** Synthetic credentials and responses, isolated home, real pinned CLI. Never calls the selected LAN model. */
export async function probeProviderRouting(executable: string) {
  await mkdir('.cache/probes', { recursive: true });
  const home = await mkdtemp(path.resolve('.cache/probes/provider-'));
  const cwd = path.join(home, 'workspace'); await mkdir(cwd);
  const checks: Check[] = [];
  const results: Awaited<ReturnType<typeof runGuardedCheck>>[] = [];
  let backend: Awaited<ReturnType<typeof startBackend>> | undefined;
  let scenario: 'guarded' | 'interrupt' | 'ordinary' = 'guarded';
  const observations: Array<{ scenario: string; authenticated: boolean; guarded: boolean; toolDeclarations: boolean }> = [];
  let cancelledUpstream = false;
  const fixture = createServer(async (request, response) => {
    try {
      const chunks: Buffer[] = []; let size = 0;
      for await (const chunk of request) { size += chunk.length; if (size > 8 * 1024 * 1024) throw new Error('size'); chunks.push(chunk); }
      const body = z.object({ tools: z.array(z.unknown()).optional(), tool_choice: z.unknown().optional(), input: z.array(z.object({ type: z.string().optional() }).passthrough()) }).parse(JSON.parse(Buffer.concat(chunks).toString()));
      const currentScenario = scenario;
      observations.push({ scenario: currentScenario,
        authenticated: request.headers.authorization === 'Bearer SYNTHETIC_PROVIDER_BEARER' && request.headers['x-openai-actor-authorization'] === 'SYNTHETIC_ACTOR',
        guarded: body.tool_choice === 'none' && body.tools?.length === 0,
        toolDeclarations: body.input.some(item => item.type === 'additional_tools') || !!body.tools?.length,
      });
      const item = { type: 'message', id: 'm_native', role: 'assistant', phase: 'final_answer', content: [] };
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.write(sse({ type: 'response.created', response: { id: 'r_native', status: 'in_progress', output: [] } }));
      response.write(sse({ type: 'response.output_item.added', output_index: 0, item }));
      response.write(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, item_id: item.id, delta: '原生认证与路由验证' }));
      const timer = setTimeout(() => {
        const completed = { ...item, status: 'completed', content: [{ type: 'output_text', text: '原生认证与路由验证', annotations: [] }] };
        response.write(sse({ type: 'response.output_item.done', output_index: 0, item: completed }));
        response.end(sse({ type: 'response.completed', response: { id: 'r_native', status: 'completed', output: [completed] } }));
      }, currentScenario === 'interrupt' ? 10_000 : 300);
      response.once('close', () => { clearTimeout(timer); if (currentScenario === 'interrupt' && !response.writableEnded) cancelledUpstream = true; });
    } catch { response.destroy(); }
  });
  fixture.listen(0, '127.0.0.1'); await once(fixture, 'listening');
  const address = fixture.address(); if (!address || typeof address === 'string') throw new Error('fixture port');
  const upstream = `http://127.0.0.1:${address.port}/v1`;
  const provider: LanProvider = { id: 'native_probe', model: 'gpt-6.1-sol', baseUrl: upstream, auth: 'native-bearer',
    forwardHeaders: ['authorization', 'x-openai-actor-authorization'], requiredHeaders: ['authorization', 'x-openai-actor-authorization'], mcpServers: [] };
  const guard = await startLoopbackTextGuard({ upstreamBaseUrl: upstream, fixtureCredentialHeaders: provider.forwardHeaders,
    authorizePeer: (peerPort, guardPort) => backend?.child.pid ? verifyDesktopTcpPeer(peerPort, guardPort, backend.child.pid, executable) : Promise.resolve(false),
  });
  try {
    await writeFile(path.join(home, 'config.toml'), `model = "${provider.model}"\nmodel_provider = "${provider.id}"\n[model_providers.${provider.id}]\nname = "Synthetic provider"\nbase_url = "${upstream}"\nwire_api = "responses"\nrequires_openai_auth = false\nexperimental_bearer_token = "SYNTHETIC_PROVIDER_BEARER"\nhttp_headers = { "X-OpenAI-Actor-Authorization" = "SYNTHETIC_ACTOR" }\nrequest_max_retries = 0\nstream_max_retries = 0\nsupports_websockets = false\n`);
    backend = await startBackend({ executable, port: await unusedLoopbackPort(), cwd, home, isolated: true, restrictProbeCapabilities: false });
    const text = await runGuardedCheck({ rpc: backend.rpc, guard, provider, cwd, prompt: 'Synthetic provider test.' });
    results.push(text);
    checks.push({ id: 'native_provider_auth_preserved', state: text.status === 'completed' && observations[0]?.authenticated && observations[0].guarded && !observations[0].toolDeclarations ? 'passed' : 'blocked',
      detail: 'Per-thread URL override retains native bearer and actor headers, while removing model-facing tools.' });
    checks.push({ id: 'native_provider_exact_stream', state: text.exactCorrelation && text.deltaCount > 0 && text.firstDeltaAt !== null && text.guard.upstreamEndedAt !== null && text.firstDeltaAt < text.guard.upstreamEndedAt ? 'passed' : 'blocked',
      detail: 'Native text deltas match thread, turn and item identifiers and arrive before the synthetic upstream finishes.' });
    scenario = 'interrupt';
    const interrupted = await runGuardedCheck({ rpc: backend.rpc, guard, provider, cwd, prompt: 'Synthetic cancellation test.', interruptAfterFirstDelta: true });
    results.push(interrupted);
    const closeDeadline = Date.now() + 2000;
    while (!cancelledUpstream && Date.now() < closeDeadline) await delay(25);
    checks.push({ id: 'native_provider_interrupt', state: interrupted.status === 'interrupted' && interrupted.interruptAcknowledged && cancelledUpstream ? 'passed' : 'blocked',
      detail: 'Native interrupt reaches an interrupted terminal state and closes the protected upstream connection.' });
    // A normal thread has no bridge config overrides and must still use the original provider route.
    scenario = 'ordinary';
    const ordinary = z.object({ thread: z.object({ id: z.string() }) }).parse(await backend.rpc.request('thread/start', { cwd, model: provider.model, modelProvider: provider.id, ephemeral: true }));
    let timer: NodeJS.Timeout | undefined; let unsubscribe: (() => void) | undefined;
    const terminal = new Promise<string>((resolve, reject) => {
      timer = setTimeout(() => reject(new Error('ordinary timeout')), 15_000);
      unsubscribe = backend!.rpc.subscribe(event => {
        const parsed = z.object({ threadId: z.literal(ordinary.thread.id), turn: z.object({ status: z.string() }) }).safeParse(event.params);
        if (event.method === 'turn/completed' && parsed.success) resolve(parsed.data.turn.status);
      });
    });
    void terminal.catch(() => {});
    let ordinaryStatus: string;
    try {
      await backend.rpc.request('turn/start', { threadId: ordinary.thread.id, input: [{ type: 'text', text: 'Ordinary synthetic control.', text_elements: [] }] });
      ordinaryStatus = await terminal;
    } finally { clearTimeout(timer); unsubscribe?.(); }
    const control = observations.find(item => item.scenario === 'ordinary');
    checks.push({ id: 'ordinary_provider_unchanged', state: ordinaryStatus === 'completed' && control?.authenticated && control.toolDeclarations && !control.guarded ? 'passed' : 'blocked',
      detail: 'An ordinary thread in the same backend still uses the original URL, credentials and tool declarations.' });
  } catch (error) { checks.push({ id: 'native_provider_probe', state: 'blocked', detail: `Provider probe stopped (${errorCode(error)}).` }); }
  finally { try { await backend?.stop(); } finally { await guard.close(); fixture.closeAllConnections(); await new Promise<void>(resolve => fixture.close(() => resolve())); } }
  return { purpose: 'isolated-native-provider-routing-probe', checkedAt: new Date().toISOString(), checks, results, observations, gate: releaseGate(checks),
    note: 'Real CLI, synthetic local credentials and responses. No LAN inference or shared desktop acceptance.' };
}

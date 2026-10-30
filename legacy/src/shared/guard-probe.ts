import { createServer } from 'node:http';
import { mkdir, mkdtemp } from 'node:fs/promises';
import path from 'node:path';
import { once } from 'node:events';
import { z } from 'zod';
import { startBackend } from './backend.js';
import { unusedLoopbackPort } from './probe.js';
import { textOnlyConfig } from './text-policy.js';
import { startLoopbackTextGuard, type GuardRouteStats } from '../guard/loopback-guard.js';
import { verifyDesktopTcpPeer } from '../desktop/windows.js';
import { errorCode } from '../errors.js';
import { releaseGate, type Check } from '../capabilities.js';

const model = 'gpt-6.1-sol';
const sse = (event: unknown) => `data: ${JSON.stringify(event)}\n\n`;
const marker = 'GUARD_CANARY_EXECUTED';
type Scenario = 'control' | 'tools' | 'text' | 'unknown' | 'truncated' | 'offline';
const calls = [
  { type: 'custom_tool_call', id: 'fc_code', call_id: 'code', name: 'exec', input: `text('${marker}');` },
  { type: 'function_call', id: 'fc_resource', call_id: 'resource', name: 'list_mcp_resources', arguments: '{}' },
];

/** Real app-server, synthetic model endpoint. No external service, account, or credential. */
export async function probeResponseGuard(executable: string) {
  await mkdir('.cache/probes', { recursive: true });
  const home = await mkdtemp(path.resolve('.cache/probes/guard-'));
  const cwd = path.join(home, 'workspace'); await mkdir(cwd);
  const checks: Check[] = [];
  const scenarios: Array<{ scenario: Scenario; status: string; guard: GuardRouteStats | null }> = [];
  let scenario: Scenario = 'control';
  let fixtureRequests = 0;
  let codeExecutedInControl = false;
  let resourcesListedInControl = false;
  let toolChoiceNone = false;
  let fixtureCompleted = false;
  let inputStructure: unknown[] = [];
  let backend: Awaited<ReturnType<typeof startBackend>> | undefined;
  const fixture = createServer(async (request, response) => {
    try {
      const chunks: Buffer[] = []; let bytes = 0;
      for await (const data of request) { bytes += data.length; if (bytes > 8 * 1024 * 1024) throw new Error('size'); chunks.push(data); }
      const body = z.object({ tool_choice: z.unknown().optional(), tools: z.array(z.unknown()).optional(), input: z.array(z.object({ type: z.string(), call_id: z.string().optional(), output: z.unknown().optional() }).passthrough()) }).parse(JSON.parse(Buffer.concat(chunks).toString()));
      fixtureRequests++;
      if (scenario === 'control' && fixtureRequests === 1) inputStructure = body.input.slice(0, 3).map(item => ({ type: item.type, role: item.role,
        keys: Object.keys(item), contentKind: Array.isArray(item.content) ? 'array' : typeof item.content,
        contentTypes: Array.isArray(item.content) ? item.content.map(p => p && typeof p === 'object' && 'type' in p ? p.type : typeof p) : undefined }));
      if (scenario !== 'control') toolChoiceNone ||= body.tool_choice === 'none' && body.tools?.length === 0 && !body.input.some(item => item.type === 'additional_tools');
      for (const item of body.input) {
        if (scenario !== 'control' || !['function_call_output', 'custom_tool_call_output'].includes(item.type)) continue;
        if (item.call_id === 'code' && JSON.stringify(item.output)?.includes(marker)) codeExecutedInControl = true;
        if (item.call_id === 'resource' && typeof item.output === 'string' && item.output.includes('"resources"')) resourcesListedInControl = true;
      }
      if (scenario === 'control' && fixtureRequests > 1) {
        response.writeHead(400, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ error: { message: 'Control fixture complete.', type: 'invalid_request_error' } })); return;
      }
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      if (scenario === 'control' || scenario === 'tools') {
        for (const item of calls) response.write(sse({ type: 'response.output_item.done', item }));
        response.end(sse({ type: 'response.completed', response: { id: 'resp_probe', output: calls } }));
      } else if (scenario === 'unknown') response.end(sse({ type: 'response.unrecognized_tool_event', item: calls[0] }));
      else {
        response.write(sse({ type: 'response.created', response: { id: 'resp_text', output: [], status: 'in_progress' } }));
        const item = { type: 'message', id: 'msg_text', role: 'assistant', phase: 'final_answer', content: [] };
        response.write(sse({ type: 'response.output_item.added', output_index: 0, item }));
        response.write(sse({ type: 'response.output_text.delta', item_id: 'msg_text', output_index: 0, content_index: 0, delta: '文本验证🌍' }));
        if (scenario === 'truncated') response.end();
        else setTimeout(() => {
          fixtureCompleted = true;
          const complete = { ...item, status: 'completed', content: [{ type: 'output_text', text: '文本验证🌍', annotations: [] }] };
          response.write(sse({ type: 'response.output_item.done', output_index: 0, item: complete }));
          response.end(sse({ type: 'response.completed', response: { id: 'resp_text', status: 'completed', output: [complete], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } }));
        }, 250);
      }
    } catch { response.destroy(); }
  });
  fixture.listen(0, '127.0.0.1'); await once(fixture, 'listening');
  const address = fixture.address(); if (!address || typeof address === 'string') throw new Error('Missing fixture port');
  const upstream = `http://127.0.0.1:${address.port}/v1`;
  const guard = await startLoopbackTextGuard({ upstreamBaseUrl: upstream, authorizePeer: async (peerPort, guardPort) => {
    if (!backend?.child.pid) return false;
    return verifyDesktopTcpPeer(peerPort, guardPort, backend.child.pid, executable);
  } });
  let guardClosed = false;
  try {
    backend = await startBackend({ executable, port: await unusedLoopbackPort(), cwd, home, isolated: true, restrictProbeCapabilities: false });
    const account = z.object({ account: z.null() }).safeParse(await backend.rpc.request('account/read', { refreshToken: false }));
    if (!account.success) throw new Error('Fixture must not have an account');
    const run = async (name: Scenario) => {
      scenario = name; fixtureRequests = 0; fixtureCompleted = false;
      const route = name === 'control' ? null : guard.createRoute(model);
      if (name === 'offline') { await guard.close(); guardClosed = true; }
      const providerName = `guard_probe_${name}`;
      const started = z.object({ thread: z.object({ id: z.string() }), model: z.literal(model), modelProvider: z.literal(providerName) }).parse(await backend!.rpc.request('thread/start', {
        modelProvider: providerName, model, allowProviderModelFallback: false, cwd, ephemeral: true,
        approvalPolicy: 'never', sandbox: 'read-only', dynamicTools: [], environments: [], selectedCapabilityRoots: [], runtimeWorkspaceRoots: [],
        config: { ...textOnlyConfig([]), 'features.enable_request_compression': false,
          [`model_providers.${providerName}`]: { name: 'Local guard protocol probe', base_url: route?.baseUrl ?? upstream, wire_api: 'responses', requires_openai_auth: false,
            supports_websockets: false, request_max_retries: 0, stream_max_retries: 0, stream_idle_timeout_ms: 5000 },
        },
      }));
      let timer: NodeJS.Timeout | undefined; let unsubscribe: (() => void) | undefined;
      let streamedBeforeFixtureCompletion = false;
      let sawUnexpectedToolItem = false;
      const terminal = new Promise<string>((resolve, reject) => {
        timer = setTimeout(() => reject(new Error('Guard probe timeout')), 20_000);
        unsubscribe = backend!.rpc.subscribe(event => {
          const parsed = z.object({ threadId: z.string(), item: z.object({ type: z.string() }).optional(), turn: z.object({ status: z.string() }).optional() }).passthrough().safeParse(event.params);
          if (!parsed.success || parsed.data.threadId !== started.thread.id) return;
          if (event.method === 'item/agentMessage/delta' && !fixtureCompleted) streamedBeforeFixtureCompletion = true;
          if (event.method === 'item/started' && parsed.data.item && !['userMessage', 'agentMessage', 'reasoning'].includes(parsed.data.item.type)) sawUnexpectedToolItem = true;
          if (event.method === 'turn/completed' && parsed.data.turn) resolve(parsed.data.turn.status);
        });
      });
      void terminal.catch(() => {});
      let status: string;
      try {
        await backend!.rpc.request('turn/start', { threadId: started.thread.id, input: [{ type: 'text', text: 'Local synthetic protocol test.', text_elements: [] }] });
        status = await terminal;
      } finally { clearTimeout(timer); unsubscribe?.(); }
      scenarios.push({ scenario: name, status, guard: route?.snapshot() ?? null });
      return { status, streamedBeforeFixtureCompletion, sawUnexpectedToolItem, stats: route?.snapshot() };
    };
    await run('control');
    checks.push({ id: 'unguarded_control_reproduces_remaining_tools', state: codeExecutedInControl && resourcesListedInControl ? 'passed' : 'blocked', detail: 'The same app-server must process the harmless code marker and resource query when the response guard is absent.' });
    const blocked = await run('tools');
    checks.push({ id: 'guard_stops_tools_before_app_server', state: blocked.status === 'failed' && blocked.stats?.state === 'blocked' && blocked.stats.upstreamRequests === 1 && blocked.stats.failureCode === 'GUARD_EVENT_BLOCKED'
      && blocked.stats.forwardedEvents === 0 && !blocked.sawUnexpectedToolItem ? 'passed' : 'blocked', detail: 'Tool-bearing frames must be rejected before forwarding; the app-server turn must fail without starting a tool item.' });
    const text = await run('text');
    checks.push({ id: 'guard_preserves_incremental_text', state: text.status === 'completed' && text.stats?.state === 'completed' && text.streamedBeforeFixtureCompletion ? 'passed' : 'blocked', detail: 'The real app-server must emit the fixture text delta before the fixture sends completion. This is a protocol test, not real model streaming acceptance.' });
    for (const name of ['unknown', 'truncated'] as const) {
      const result = await run(name);
      checks.push({ id: `guard_${name}_fails_closed`, state: result.status === 'failed' && result.stats?.state === 'blocked' && result.stats.upstreamRequests === 1
        && result.stats.failureCode === (name === 'unknown' ? 'GUARD_EVENT_BLOCKED' : 'GUARD_TRUNCATED') ? 'passed' : 'blocked', detail: 'Unsupported events or incomplete streams must fail the turn; they cannot bypass the guard.' });
    }
    const offline = await run('offline');
    checks.push({ id: 'guard_offline_no_fallback', state: offline.status === 'failed' && fixtureRequests === 0 ? 'passed' : 'blocked', detail: 'With the guard stopped, the app-server must fail without contacting the local upstream directly.' });
    checks.push({ id: 'guard_forces_no_tools_request', state: toolChoiceNone ? 'passed' : 'blocked', detail: 'The local upstream observed tools=[], tool_choice=none, and no additional_tools input. Response enforcement still applies if the upstream ignores them.' });
  } catch (error) { checks.push({ id: 'guard_probe', state: 'blocked', detail: `Guard integration probe stopped (${errorCode(error)}).` }); }
  finally {
    try { await backend?.stop(); } finally {
      try { if (!guardClosed) await guard.close(); } finally { fixture.closeAllConnections(); await new Promise<void>(resolve => fixture.close(() => resolve())); }
    }
  }
  return { purpose: 'isolated-pre-execution-guard-probe', checkedAt: new Date().toISOString(), home, scenarios, diagnostics: guard.diagnostics(), inputStructure, checks,
    gate: releaseGate(checks), note: 'Real app-server with local synthetic responses only. Production authentication/routing and shared desktop acceptance are not verified or enabled.' };
}

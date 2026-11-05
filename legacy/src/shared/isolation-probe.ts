import { createServer } from 'node:http';
import { mkdir, mkdtemp, writeFile, access } from 'node:fs/promises';
import path from 'node:path';
import { once } from 'node:events';
import { randomUUID } from 'node:crypto';
import { z } from 'zod';
import { startBackend } from './backend.js';
import { unusedLoopbackPort } from './probe.js';
import { textOnlyConfig } from './text-policy.js';
import { errorCode } from '../errors.js';
import { releaseGate, type Check } from '../capabilities.js';

const exists = (filename: string) => access(filename).then(() => true, () => false);
const requestSchema = z.object({
  tools: z.array(z.unknown()).optional(),
  input: z.array(z.object({ type: z.string(), call_id: z.string().optional(), output: z.unknown().optional() }).passthrough()).optional(),
});
function toolNames(tools: unknown[]): string[] {
  return tools.flatMap(value => {
    const tool = z.object({ name: z.string().optional(), type: z.string(), tools: z.array(z.unknown()).optional() }).parse(value);
    return tool.tools ? toolNames(tool.tools).map(n => `${tool.name ?? tool.type}.${n}`) : [tool.name ?? tool.type];
  });
}

/** An isolated, non-generating local fixture. It never forwards anything to a model service. */
export async function probeToolIsolation(executable: string) {
  await mkdir('.cache/probes', { recursive: true });
  const home = await mkdtemp(path.resolve('.cache/probes/isolation-'));
  const cwd = path.join(home, 'workspace'); await mkdir(cwd);
  const canary = path.join(home, 'mcp-started');
  const mcpScript = path.join(home, 'mcp-canary.cjs');
  await writeFile(mcpScript, `const fs=require('node:fs');fs.writeFileSync(${JSON.stringify(canary)},'started');
const rl=require('node:readline').createInterface({input:process.stdin});rl.on('line',line=>{const q=JSON.parse(line);if(q.id===undefined)return;let result={};if(q.method==='initialize')result={protocolVersion:'2024-11-05',capabilities:{tools:{}},serverInfo:{name:'bridge-canary',version:'1'}};if(q.method==='tools/list')result={tools:[{name:'ping',description:'Test fixture only',inputSchema:{type:'object',properties:{}}}]};process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:q.id,result})+'\\n');});`);
  await writeFile(path.join(home, 'config.toml'), `[mcp_servers.bridge_canary]\ncommand = ${JSON.stringify(process.execPath)}\nargs = [${JSON.stringify(mcpScript)}]\nenabled = true\nstartup_timeout_sec = 5\n`);
  const checks: Check[] = [];
  let mode: 'restricted' | 'control' = 'restricted';
  let restrictedRequests = 0;
  let controlRequests = 0;
  let restrictedTools: string[] | undefined;
  let controlTools: string[] | undefined;
  const rejectedCalls = new Set<string>();
  let restrictedToolPreamble = false;
  let controlToolPreamble = false;
  let controlCallbackReached = false;
  let restrictedInstructionTools: string[] = [];
  const names = ['exec_command', 'apply_patch', 'mcp__bridge_canary__ping', 'exec', 'functions.exec', 'list_mcp_resources', 'clock__curr_time'];
  const calls = names.map((name, index) => ({ id: `fc_${index}`, call_id: `probe_${index}`, name,
    ...([1, 3, 4].includes(index) ? { type: 'custom_tool_call', input: index === 1
      ? '*** Begin Patch\n*** Add File: probe-should-not-exist.txt\n+unexpected\n*** End Patch'
      : "await tools.exec_command({cmd:'echo bridge-isolation-probe'});" }
      : { type: 'function_call', arguments: JSON.stringify(index === 0 ? { cmd: 'echo bridge-isolation-probe' } : {}) }) }));
  const server = createServer(async (request, response) => {
    try {
      const chunks: Buffer[] = []; let bytes = 0;
      for await (const data of request) { bytes += data.length; if (bytes > 8 * 1024 * 1024) throw new Error('Limit'); chunks.push(data); }
      const raw = JSON.parse(Buffer.concat(chunks).toString()) as Record<string, unknown>;
      const rawText = JSON.stringify(raw);
      const toolPreamble = /# Tools|declare const tools:|namespace functions/.test(rawText);
      const body = requestSchema.parse(raw);
      const embedded = z.object({ input: z.array(z.object({ type: z.string(), tools: z.array(z.unknown()).optional() }).passthrough()) }).parse(raw);
      const exposed = [...toolNames(body.tools ?? []), ...embedded.input.flatMap(item => item.type === 'additional_tools' ? toolNames(item.tools ?? []) : [])];
      if (mode === 'restricted') {
        restrictedRequests++;
        if (restrictedRequests === 1) restrictedInstructionTools = [...new Set([...rawText.matchAll(/declare const tools: \{ (\w+)/g)].map(m => m[1]!))];
        restrictedToolPreamble ||= toolPreamble;
        restrictedTools = [...new Set([...(restrictedTools ?? []), ...exposed])];
        for (const item of body.input ?? []) if (['function_call_output', 'custom_tool_call_output'].includes(item.type) && item.call_id
          && typeof item.output === 'string' && /unknown tool|not found|unsupported|not available|unrecognized/i.test(item.output)) rejectedCalls.add(item.call_id);
        if (restrictedRequests === 1) {
          response.writeHead(200, { 'content-type': 'text/event-stream' });
          for (const item of calls) response.write(`event: response.output_item.done\ndata: ${JSON.stringify({ type: 'response.output_item.done', item })}\n\n`);
          response.end(`event: response.completed\ndata: ${JSON.stringify({ type: 'response.completed', response: { id: randomUUID(), output: calls, usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
          return;
        }
      } else {
        controlTools = exposed; controlToolPreamble ||= toolPreamble; controlRequests++;
        if (controlRequests === 1) {
          const call = { type: 'function_call', id: 'fc_control', call_id: 'probe_control', name: 'bridge_control_tool', arguments: '{}' };
          response.writeHead(200, { 'content-type': 'text/event-stream' });
          response.write(`event: response.output_item.done\ndata: ${JSON.stringify({ type: 'response.output_item.done', item: call })}\n\n`);
          response.end(`event: response.completed\ndata: ${JSON.stringify({ type: 'response.completed', response: { id: randomUUID(), output: [call], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
          return;
        }
      }
      response.writeHead(400, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: { message: 'Local fixture ends here; no model service is used.', type: 'invalid_request_error' } }));
    } catch {
      response.writeHead(400); response.end();
      checks.push({ id: 'fixture_request', state: 'blocked', detail: 'The local fixture could not parse the isolated request.' });
    }
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Missing fixture port');
  let backend: Awaited<ReturnType<typeof startBackend>> | undefined;
  try {
    backend = await startBackend({ executable, port: await unusedLoopbackPort(), cwd, home, isolated: true, restrictProbeCapabilities: false });
    backend.rpc.observeRejectedCallbacks(method => { if (mode === 'control' && method === 'item/tool/call') controlCallbackReached = true; });
    const provider = { 'model_providers.bridge_probe': { name: 'Local non-generating test fixture', base_url: `http://127.0.0.1:${address.port}/v1`, wire_api: 'responses', requires_openai_auth: false, supports_websockets: false, request_max_retries: 0, stream_max_retries: 0 }, 'features.enable_request_compression': false };
    const run = async (restricted: boolean) => {
      const started = z.object({ thread: z.object({ id: z.string() }), modelProvider: z.literal('bridge_probe') }).parse(await backend!.rpc.request('thread/start', {
        modelProvider: 'bridge_probe', model: 'gpt-6.1-sol', cwd, ephemeral: true, approvalPolicy: 'never', sandbox: 'read-only',
        config: { ...provider, ...(restricted ? textOnlyConfig(['bridge_canary']) : {}) },
        dynamicTools: restricted ? [] : [{ type: 'function', name: 'bridge_control_tool', description: 'Local test fixture only', inputSchema: { type: 'object', properties: {} } }],
        ...(restricted ? { environments: [], selectedCapabilityRoots: [], runtimeWorkspaceRoots: [] } : {}),
      }));
      let timer: NodeJS.Timeout | undefined;
      let unsubscribe: (() => void) | undefined;
      const terminal = new Promise<void>((resolve, reject) => {
        timer = setTimeout(() => reject(new Error('Fixture timeout')), 20_000);
        unsubscribe = backend!.rpc.subscribe(event => {
          if (event.method === 'turn/completed' && event.params && typeof event.params === 'object'
            && 'threadId' in event.params && event.params.threadId === started.thread.id) resolve();
        });
      });
      void terminal.catch(() => {});
      try {
        await backend!.rpc.request('turn/start', { threadId: started.thread.id, input: [{ type: 'text', text: 'Local protocol fixture. No model generation is requested.', text_elements: [] }] });
        await terminal;
      } finally { clearTimeout(timer); unsubscribe?.(); }
    };
    await run(true);
    checks.push({ id: 'thread_has_no_declared_tools', state: restrictedTools?.length === 0 && !restrictedToolPreamble ? 'passed' : 'blocked', detail: `Captured ${restrictedTools?.length ?? 'no request / unknown'} declarations across root tools and input additional_tools; tool instruction preamble present=${restrictedToolPreamble}. Thread-only policy; no global capability restrictions.` });
    checks.push({ id: 'forced_calls_rejected', state: names.every((_name, index) => rejectedCalls.has(`probe_${index}`)) ? 'passed' : 'blocked', detail: `${rejectedCalls.size}/${names.length} synthetic shell, patch, code-mode, MCP and clock calls received an unsupported-tool result from the real app-server.` });
    checks.push({ id: 'mcp_not_started_for_restricted_thread', state: await exists(canary) ? 'blocked' : 'passed', detail: 'The configured MCP canary process must not start for the restricted thread.' });
    checks.push({ id: 'no_patch_file', state: await exists(path.join(cwd, 'probe-should-not-exist.txt')) ? 'blocked' : 'passed', detail: 'The synthetic patch must not create its canary file.' });
    mode = 'control'; await run(false);
    checks.push({ id: 'control_thread_keeps_own_tools', state: controlToolPreamble && controlCallbackReached && await exists(canary) ? 'passed' : 'blocked', detail: `Control thread tool instructions present=${controlToolPreamble}; dynamic tool callback reached=${controlCallbackReached}. The control MCP process must also start.` });
  } catch (error) { checks.push({ id: 'isolation_probe', state: 'blocked', detail: `Isolated test failed (${errorCode(error)}).` }); }
  finally {
    try { await backend?.stop(); } finally { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); }
  }
  const forcedCallResults = names.map((name, index) => ({ name, unsupportedResultObserved: rejectedCalls.has(`probe_${index}`) }));
  return { purpose: 'isolated-tool-enforcement-test', checkedAt: new Date().toISOString(), home, restrictedTools, restrictedInstructionTools, controlTools, forcedCallResults, checks,
    gate: releaseGate(checks), note: 'This uses synthetic local responses to test enforcement, not real inference or real streaming. Live desktop acceptance remains required.' };
}

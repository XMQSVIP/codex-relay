/** Opt-in live acceptance. Tokens and API keys are never printed or saved.
 * Usage: node scripts/test_real_oauth.mjs --allow-real-requests --account UUID [--verify-route]
 * The account must already be linked/logged in. A temporary scoped client key is revoked on exit.
 */
import assert from 'node:assert/strict';
import { parseArgs } from 'node:util';
import { writeFile, mkdir } from 'node:fs/promises';
import WebSocket from 'ws';

const { values } = parseArgs({ options: {
  'allow-real-requests': { type: 'boolean' },
  'verify-route': { type: 'boolean' },
  'skip-text': { type: 'boolean' },
  'only-native': { type: 'boolean' },
  account: { type: 'string' },
  base: { type: 'string', default: 'http://127.0.0.1:4317' },
  model: { type: 'string', default: 'gpt-6.1-sol' },
}});
assert(values['allow-real-requests'], 'Pass --allow-real-requests to consume real account usage.');
assert(values.account, 'Pass --account for an already linked/logged-in bridge account.');
const base = new URL(values.base);
assert(['127.0.0.1', 'localhost', '[::1]'].includes(base.hostname), 'Live acceptance uses local management only.');
const adminToken = process.env.BRIDGE_ADMIN_TOKEN;
const report = { startedAt: new Date().toISOString(), model: values.model, realAccounts: 1, completed: false, tests: [], requestedTurns: 0 };
const headers = { 'content-type': 'application/json', ...(adminToken ? { authorization: `Bearer ${adminToken}` } : {}) };
async function admin(path, body, method = body === undefined ? 'GET' : 'POST') {
  const response = await fetch(new URL(path, base), { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(120000) });
  const data = await response.json();
  if (!response.ok) throw new Error(`Management ${path}: HTTP ${response.status}, ${data.error?.code ?? 'unknown'}`);
  return data;
}
const pass = name => { report.tests.push({ name, passed: true }); console.log(`PASS: ${name}`); };
const text = response => (response.output ?? []).filter(i => i.type === 'message').flatMap(i => i.content ?? []).filter(c => c.type === 'output_text').map(c => c.text).join('');
let key;
async function response(body) {
  report.requestedTurns++;
  const result = await fetch(new URL('/v1/responses', base), { method: 'POST', headers: { 'content-type': 'application/json', authorization: `Bearer ${key.secret}` }, body: JSON.stringify({ model: values.model, reasoning: { effort: 'low' }, ...body }), signal: AbortSignal.timeout(120000) });
  if (!result.ok) { const data = await result.json(); throw new Error(`Responses: HTTP ${result.status}, ${data.error?.code ?? 'unknown'}; ${data.error?.message ?? ''}`); }
  return result;
}
async function jsonResponse(body) {
  const result = await (await response(body)).json();
  assert.equal(result.status, 'completed', `Native turn status: ${result.status}; ${result.error?.code ?? ''}`);
  assert(result.usage?.total_tokens > 0, 'Real token usage must be present.');
  assert.equal(result.store, false);
  return result;
}
async function idle() {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    const state = await admin('/api/operations');
    if (state.active === 0 && state.queued === 0) return;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error('Native instance or admission slot remained active.');
}
async function websocketTurns() {
  const url = new URL('/v1/native', base); url.protocol = base.protocol === 'https:' ? 'wss:' : 'ws:';
  const socket = new WebSocket(url, { headers: { authorization: `Bearer ${key.secret}` } });
  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Native WebSocket deadline exceeded.')), 120000);
      let turns = 0, output = '', toolCalls = 0;
      const send = value => socket.send(JSON.stringify(value));
      socket.once('error', reject);
      socket.on('open', () => send({ id: 1, method: 'thread/start', params: { model: values.model, dynamicTools: [{ type: 'function', name: 'bridge_sum', description: 'Add two integers on the client.', inputSchema: { type: 'object', properties: { a: { type: 'integer' }, b: { type: 'integer' } }, required: ['a', 'b'], additionalProperties: false } }] } }));
      socket.on('message', bytes => {
        try {
          const event = JSON.parse(bytes.toString());
          if (event.error || event.method === 'bridge/error') throw new Error(`Native WebSocket: ${event.error?.code ?? event.params?.code}`);
          if (event.id === 1 && event.result) {
            report.requestedTurns++;
            send({ id: 2, method: 'turn/start', params: { input: [{ type: 'text', text: 'Call bridge_sum once with a=19 and b=23. After the tool returns, reply with the result as digits only.', text_elements: [] }] } });
          }
          if (event.method === 'rawResponseItem/completed') {
            const kind = event.params.item.type;
            report.nativeItemTypes ??= [];
            if (!report.nativeItemTypes.includes(kind)) { report.nativeItemTypes.push(kind); console.log(`Native item type: ${kind}`); }
          }
          if (event.method === 'item/tool/call') {
            assert.equal(event.params.tool, 'bridge_sum');
            assert.deepEqual(event.params.arguments, { a: 19, b: 23 });
            toolCalls++;
            send({ id: event.id, result: { success: true, contentItems: [{ type: 'inputText', text: String(event.params.arguments.a + event.params.arguments.b) }] } });
          }
          if (event.method === 'item/agentMessage/delta') output += event.params.delta;
          if (event.method === 'turn/completed') {
            assert.equal(event.params.turn.status, 'completed');
            assert(output.includes(turns === 0 ? '42' : '43'), 'Native multi-turn text was not preserved.');
            output = ''; turns++;
            if (turns === 1) {
              report.requestedTurns++;
              assert.equal(toolCalls, 1);
              send({ id: 3, method: 'turn/start', params: { input: [{ type: 'text', text: 'Using the previous result, reply with that value plus 1, digits only. Do not call any tools.', text_elements: [] }] } });
            } else send({ id: 4, method: 'session/close' });
          }
          if (event.id === 4 && event.result) { clearTimeout(timer); resolve(); }
        } catch (error) { clearTimeout(timer); reject(error); }
      });
      socket.on('close', () => { clearTimeout(timer); if (turns === 2) resolve(); else reject(new Error('Native WebSocket closed before completion.')); });
    });
  } finally { socket.terminate(); }
  await idle();
  pass('native WebSocket: two completed turns and process cleanup');
}

try {
  if (values['verify-route']) {
    const { revision } = await admin('/api/operations');
    await admin(`/api/accounts/${values.account}/route`, { revision, enabled: true });
    pass('OAuth access: enabled without model inference');
    report.requestedTurns++;
    await admin(`/api/accounts/${values.account}/connection-test`, { model: values.model });
    pass('Optional connection test: native real inference verified');
  }
  const operations = await admin('/api/operations');
  const account = operations.accounts?.find(a => a.id === values.account);
  assert(account?.route, 'Selected account needs enabled OAuth access.');
  key = await admin('/api/api-keys', { name: 'macOS real OAuth acceptance (temporary)', groupId: account.groupIds[0], models: [values.model], maxConcurrency: 1 });
  if (!values['skip-text'] && !values['only-native']) {
  const plain = await jsonResponse({ input: 'Reply exactly BRIDGE_MAC_OK.', store: false });
  assert(text(plain).includes('BRIDGE_MAC_OK'));
  await idle(); pass('Responses JSON text and real usage');

  const streamed = await response({ input: 'Reply exactly BRIDGE_STREAM_OK.', stream: true, store: false });
  const decoder = new TextDecoder(); let buffer = '', deltas = '', final;
  for await (const chunk of streamed.body) {
    buffer += decoder.decode(chunk, { stream: true });
    let boundary;
    while ((boundary = buffer.indexOf('\n\n')) >= 0) {
      const block = buffer.slice(0, boundary); buffer = buffer.slice(boundary + 2);
      const data = block.split('\n').filter(line => line.startsWith('data:')).map(line => line.slice(5).trim()).join('\n');
      if (!data) continue;
      const event = JSON.parse(data);
      if (event.type === 'response.output_text.delta') deltas += event.delta;
      if (event.type === 'response.completed') final = event.response;
      if (['response.failed', 'error', 'response.incomplete'].includes(event.type)) throw new Error(`SSE failed: ${event.code ?? event.response?.error?.code ?? event.type}`);
    }
  }
  assert(final && deltas.includes('BRIDGE_STREAM_OK')); assert.equal(deltas, text(final));
  await idle(); pass('Responses SSE deltas and completed terminal event');
  }

  if (!values['only-native']) {
  const tools = [{ type: 'function', name: 'bridge_sum', description: 'Add two integers on the client.', parameters: { type: 'object', properties: { a: { type: 'integer' }, b: { type: 'integer' } }, required: ['a', 'b'], additionalProperties: false }, strict: false }];
  const history = [{ role: 'user', content: 'Call bridge_sum exactly once with a=19 and b=23. Wait for the tool result. After receiving it, reply with the result as digits only.' }];
  const first = await jsonResponse({ input: history, tools });
  const calls = first.output.filter(item => item.type === 'function_call'); assert.equal(calls.length, 1); assert.equal(calls[0].name, 'bridge_sum');
  const args = JSON.parse(calls[0].arguments); assert.deepEqual(args, { a: 19, b: 23 });
  history.push(...first.output, { type: 'function_call_output', call_id: calls[0].call_id, output: String(args.a + args.b) });
  const second = await jsonResponse({ input: history, tools });
  assert(text(second).includes('42'));
  history.push(...second.output, { role: 'user', content: 'Using the result from earlier, reply with that value plus 1, digits only.' });
  const third = await jsonResponse({ input: history, tools, tool_choice: 'none' });
  assert(text(third).includes('43'));
  await idle(); pass('client function execution, tool result return, client-owned history and multi-turn context');
  }
  await websocketTurns();
  const usage = await fetch(new URL('/v1/usage', base), { headers: { authorization: `Bearer ${key.secret}` } }).then(r => r.json());
  report.usage = usage.summary;
  assert(report.usage.succeeded >= (values['only-native'] ? 1 : values['skip-text'] ? 4 : 6) && report.usage.inputTokens > 0);
  pass('shared Key/group scheduler and persisted usage');
  report.completed = true;
} finally {
  if (key) {
    await admin(`/api/api-keys/${key.key.id}`, { revision: key.key.revision, state: 'revoked' });
  }
  report.finishedAt = new Date().toISOString();
  await mkdir('.cache', { recursive: true });
  await writeFile(`.cache/macos-real-oauth-${report.startedAt.replace(/[:.]/g, '-')}.json`, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
  await writeFile('.cache/macos-real-oauth.json', JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
}
console.log(`PASS: ${report.requestedTurns} live model turns requested, one real account; temporary Key revoked.`);

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { startLoopbackTextGuard } from '../src/guard/loopback-guard.js';

const payload = { model: 'test-model', stream: true, input: [
  { type: 'additional_tools', role: 'developer', tools: [{ type: 'custom', name: 'exec', description: 'must be removed' }] },
  { role: 'user', content: 'test' },
] };
const event = (data: unknown) => `data: ${JSON.stringify(data)}\n\n`;

test('local guard changes tool choice, streams text, and refuses replay without another upstream request', async () => {
  let requests = 0; let toolsDisabled = false;
  const upstream = createServer(async (request, response) => {
    requests++;
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString()) as Record<string, unknown>;
    toolsDisabled = body.tool_choice === 'none' && Array.isArray(body.tools) && body.tools.length === 0 && !JSON.stringify(body.input).includes('additional_tools');
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    response.write(event({ type: 'response.output_item.added', output_index: 0, item: { type: 'message', id: 'm', role: 'assistant', content: [] } }));
    response.write(event({ type: 'response.output_text.delta', output_index: 0, content_index: 0, item_id: 'm', delta: 'early' }));
    setTimeout(() => response.end(event({ type: 'response.completed', response: { id: 'r', output: [] } })), 100);
  });
  upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening');
  const address = upstream.address(); if (!address || typeof address === 'string') throw new Error('No port');
  const guard = await startLoopbackTextGuard({ upstreamBaseUrl: `http://127.0.0.1:${address.port}/v1`, authorizePeer: async () => true });
  try {
    const route = guard.createRoute('test-model');
    const url = `${route.baseUrl}/responses`;
    const result = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(payload) });
    const reader = result.body!.getReader();
    const first = await reader.read();
    assert.match(new TextDecoder().decode(first.value), /early/);
    assert.equal(route.snapshot().completedAt, null);
    while (!(await reader.read()).done) { /* Consume remaining validated events. */ }
    assert.equal(route.snapshot().state, 'completed'); assert.equal(toolsDisabled, true);
    const retry = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(payload) });
    assert.equal(retry.status, 409); await retry.body?.cancel(); assert.equal(requests, 1);
  } finally { await guard.close(); upstream.closeAllConnections(); await new Promise<void>(resolve => upstream.close(() => resolve())); }
});

test('local guard blocks tools before forwarding and rejects credentials, wrong model, and remote upstreams', async () => {
  await assert.rejects(startLoopbackTextGuard({ upstreamBaseUrl: 'https://chatgpt.com/backend-api/codex', authorizePeer: async () => true }), { code: 'GUARD_FIXTURE_ENDPOINT' });
  const upstream = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    response.end(event({ type: 'response.output_item.done', item: { type: 'custom_tool_call', id: 't', call_id: 'c', name: 'exec', input: 'SECRET_TOOL_PROGRAM' } }));
  });
  upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening');
  const address = upstream.address(); if (!address || typeof address === 'string') throw new Error('No port');
  const guard = await startLoopbackTextGuard({ upstreamBaseUrl: `http://127.0.0.1:${address.port}/v1`, authorizePeer: async () => true });
  try {
    const route = guard.createRoute('test-model');
    const result = await fetch(`${route.baseUrl}/responses`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(payload) });
    const text = await result.text();
    assert.equal(text.includes('SECRET_TOOL_PROGRAM'), false); assert.match(text, /bridge_guard_blocked/);
    assert.equal(route.snapshot().state, 'blocked'); assert.equal(route.snapshot().forwardedEvents, 0);
    for (const [headers, body] of [
      [{ authorization: 'Bearer TEST_SECRET' }, payload],
      [{}, { ...payload, model: 'unapproved' }],
      [{}, { ...payload, tools: [{ type: 'function', name: 'exec' }] }],
    ] as const) {
      const blocked = guard.createRoute('test-model');
      const response = await fetch(`${blocked.baseUrl}/responses`, { method: 'POST', headers: { 'content-type': 'application/json', ...headers }, body: JSON.stringify(body) });
      assert.equal(response.status, 502); await response.body?.cancel(); assert.equal(blocked.snapshot().upstreamRequests, 0);
    }
  } finally { await guard.close(); upstream.closeAllConnections(); await new Promise<void>(resolve => upstream.close(() => resolve())); }
});

test('provider redirects cannot carry native credentials to another server', async () => {
  let leakedRequests = 0;
  const collector = createServer((_request, response) => { leakedRequests++; response.end('unexpected'); });
  collector.listen(0, '127.0.0.1'); await once(collector, 'listening');
  const collectorAddress = collector.address(); if (!collectorAddress || typeof collectorAddress === 'string') throw new Error('port');
  const upstream = createServer((_request, response) => {
    response.writeHead(307, { location: `http://127.0.0.1:${collectorAddress.port}/collect` }); response.end();
  });
  upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening');
  const address = upstream.address(); if (!address || typeof address === 'string') throw new Error('port');
  const guard = await startLoopbackTextGuard({ upstreamBaseUrl: `http://127.0.0.1:${address.port}/v1`, fixtureCredentialHeaders: ['authorization'], authorizePeer: async () => true });
  try {
    const route = guard.createRoute('test-model');
    const response = await fetch(`${route.baseUrl}/responses`, { method: 'POST', headers: { 'content-type': 'application/json', authorization: 'Bearer SYNTHETIC_SECRET' }, body: JSON.stringify(payload) });
    assert.equal(response.status, 502); assert.equal((await response.text()).includes('SYNTHETIC_SECRET'), false);
    assert.equal(route.snapshot().upstreamRequests, 1); assert.equal(leakedRequests, 0);
  } finally {
    await guard.close(); upstream.closeAllConnections(); collector.closeAllConnections();
    await Promise.all([new Promise<void>(resolve => upstream.close(() => resolve())), new Promise<void>(resolve => collector.close(() => resolve()))]);
  }
});

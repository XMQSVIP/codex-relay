import { test } from 'node:test';
import assert from 'node:assert/strict';
import { startLanTextGuard } from '../src/guard/loopback-guard.js';
import type { LanProvider } from '../src/shared/provider.js';

const provider: LanProvider = { id: 'OpenAI', model: 'test-model', baseUrl: 'http://192.168.1.10:48760/v1', auth: 'native-bearer',
  forwardHeaders: ['authorization', 'x-openai-actor-authorization'], requiredHeaders: ['authorization', 'x-openai-actor-authorization'], mcpServers: [] };
const body = JSON.stringify({ model: 'test-model', stream: true, input: [{ role: 'user', content: 'test' }] });
const credentials = { authorization: 'Bearer PRIVATE_FIXTURE_BEARER', 'x-openai-actor-authorization': 'PRIVATE_FIXTURE_ACTOR' };

test('LAN guard forwards only selected native credentials to the fixed target, blocks replay and missing auth', async t => {
  const nativeFetch = globalThis.fetch;
  let calls = 0;
  t.mock.method(globalThis, 'fetch', async (url: string, options: RequestInit) => {
    calls++;
    assert.equal(url, `${provider.baseUrl}/responses`); assert.equal(options.redirect, 'error');
    const headers = new Headers(options.headers);
    assert.equal(headers.get('authorization'), credentials.authorization);
    assert.equal(headers.get('x-openai-actor-authorization'), credentials['x-openai-actor-authorization']);
    assert.equal(headers.has('x-external-secret'), false);
    return new Response('data: {"type":"response.completed","response":{"id":"r","output":[]}}\n\n', { headers: { 'content-type': 'text/event-stream' } });
  });
  const guard = await startLanTextGuard({ provider, authorizePeer: async () => true });
  try {
    const route = guard.createRoute(provider.model);
    const response = await nativeFetch(`${route.baseUrl}/responses`, { method: 'POST', headers: { 'content-type': 'application/json', ...credentials, 'x-external-secret': 'not-forwarded' }, body });
    await response.text();
    assert.equal(route.snapshot().state, 'completed'); assert.equal(calls, 1);
    assert.equal(JSON.stringify({ stats: route.snapshot(), diagnostics: guard.diagnostics() }).includes('PRIVATE_FIXTURE'), false);
    const missing = guard.createRoute(provider.model);
    const blocked = await nativeFetch(`${missing.baseUrl}/responses`, { method: 'POST', headers: { 'content-type': 'application/json' }, body });
    assert.equal(blocked.status, 502); await blocked.text(); assert.equal(calls, 1);
    assert.equal(missing.snapshot().failureCode, 'GUARD_CREDENTIALS');
    route.release();
    const retry = await nativeFetch(`${route.baseUrl}/responses`, { method: 'POST', headers: { 'content-type': 'application/json', ...credentials }, body });
    assert.equal(retry.status, 404); await retry.text(); assert.equal(calls, 1);
  } finally { await guard.close(); }
});

test('LAN upstream auth rejection is sanitized, single-use, and never retried by the guard', async t => {
  const nativeFetch = globalThis.fetch; let calls = 0;
  t.mock.method(globalThis, 'fetch', async () => { calls++; return new Response('PRIVATE_VENDOR_ERROR_TOKEN', { status: 401 }); });
  const guard = await startLanTextGuard({ provider, authorizePeer: async () => true });
  try {
    const route = guard.createRoute(provider.model);
    const request = () => nativeFetch(`${route.baseUrl}/responses`, { method: 'POST', headers: { 'content-type': 'application/json', ...credentials }, body });
    const rejected = await request();
    assert.equal(rejected.status, 502); assert.equal((await rejected.text()).includes('PRIVATE_VENDOR_ERROR_TOKEN'), false);
    assert.equal(route.snapshot().failureCode, 'GUARD_UPSTREAM_AUTH');
    const retry = await request(); assert.equal(retry.status, 409); await retry.text(); assert.equal(calls, 1);
  } finally { await guard.close(); }
});

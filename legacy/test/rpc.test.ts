import { test } from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { WebSocketServer } from 'ws';
import { AppServerRpc, loopbackWebSocketUrl } from '../src/transport/rpc.js';

const token = 'a'.repeat(43);
async function server() {
  const wss = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await once(wss, 'listening');
  const address = wss.address();
  if (!address || typeof address === 'string') throw new Error('Missing port');
  return { wss, url: `ws://127.0.0.1:${address.port}`, close: async () => {
    for (const ws of wss.clients) ws.terminate();
    await new Promise<void>(resolve => wss.close(() => resolve()));
  } };
}

test('RPC uses independent IDs, delivers live notifications, and rejects client callbacks', async () => {
  const host = await server();
  const seen: unknown[] = [];
  const deniedMethods: string[] = [];
  let correctAuth = false;
  let callbackReply: unknown;
  host.wss.on('connection', (ws, request) => {
    correctAuth = request.headers.authorization === `Bearer ${token}`;
    const requests: { id: string; method: string }[] = [];
    ws.on('message', data => {
      const message = JSON.parse(data.toString()) as { id: string; method?: string; error?: unknown };
      if (message.method) {
        requests.push({ id: message.id, method: message.method });
        if (requests.length === 2) {
          ws.send(JSON.stringify({ method: 'item/agentMessage/delta', params: { threadId: 't', turnId: 'u', itemId: 'i', delta: '真实增量' } }));
          ws.send(JSON.stringify({ id: requests[1]!.id, result: 'second' }));
          ws.send(JSON.stringify({ id: requests[0]!.id, result: 'first' }));
          ws.send(JSON.stringify({ id: 'server-1', method: 'item/tool/call', params: { tool: 'must-not-execute' } }));
        }
      } else callbackReply = message.error;
    });
  });
  const rpc = await AppServerRpc.connect(host.url, token);
  try {
    rpc.subscribe(event => seen.push(event));
    rpc.observeRejectedCallbacks(method => deniedMethods.push(method));
    assert.deepEqual(await Promise.all([rpc.request('one', {}), rpc.request('two', {})]), ['first', 'second']);
    await delay(30);
    assert.equal(correctAuth, true);
    assert.equal(seen.length, 1);
    assert.deepEqual(callbackReply, { code: -32601, message: 'Client callbacks are disabled.' });
    assert.deepEqual(deniedMethods, ['item/tool/call']);
  } finally { rpc.close(); await host.close(); }
});

test('RPC timeout never replays, and disconnect fails outstanding calls without leaking remote errors', async () => {
  const host = await server();
  let requests = 0;
  host.wss.on('connection', ws => ws.on('message', data => {
    requests++;
    const message = JSON.parse(data.toString()) as { id: string; method: string };
    if (message.method === 'secret') ws.send(JSON.stringify({ id: message.id, error: { code: -1, message: 'SECRET_TOKEN' } }));
    if (message.method === 'close') ws.terminate();
  }));
  const rpc = await AppServerRpc.connect(host.url, token, 150);
  try {
    await assert.rejects(rpc.request('timeout', {}), { code: 'RPC_TIMEOUT' });
    assert.equal(requests, 1);
    await assert.rejects(rpc.request('secret', {}), error => error instanceof Error && !error.message.includes('SECRET_TOKEN'));
    await assert.rejects(rpc.request('close', {}), { code: 'RPC_DISCONNECTED' });
    assert.equal(requests, 3);
  } finally { rpc.close(); await host.close(); }
});

test('RPC restricts addresses and fails closed on malformed frames', async () => {
  for (const url of ['ws://localhost:1234', 'ws://192.168.1.1:1234', 'ws://u:p@127.0.0.1:1234', 'ws://127.0.0.1:1234/path', 'ws://127.0.0.1:1234/?secret=x']) {
    assert.throws(() => loopbackWebSocketUrl(url), { code: 'RPC_ENDPOINT' });
  }
  const host = await server();
  host.wss.on('connection', ws => ws.on('message', () => ws.send('not json')));
  const rpc = await AppServerRpc.connect(host.url, token);
  try { await assert.rejects(rpc.request('test', {}), { code: 'RPC_PROTOCOL' }); }
  finally { rpc.close(); await host.close(); }
});

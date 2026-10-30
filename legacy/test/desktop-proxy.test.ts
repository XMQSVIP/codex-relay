import { test } from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import WebSocket, { WebSocketServer } from 'ws';
import { startDesktopProxy } from '../src/transport/desktop-proxy.js';
import { verifyDesktopTcpPeer } from '../src/desktop/windows.js';

test('desktop proxy authenticates upstream, keeps each connection separate, and forwards notifications', async () => {
  const upstream = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await once(upstream, 'listening');
  const address = upstream.address();
  if (!address || typeof address === 'string') throw new Error('Missing port');
  const token = 'b'.repeat(43);
  let auth = true;
  let initializedConnections = 0;
  let number = 0;
  upstream.on('connection', (socket, request) => {
    const connection = ++number;
    auth &&= request.headers.authorization === `Bearer ${token}`;
    socket.on('message', data => {
      const message = JSON.parse(data.toString()) as { id: number };
      socket.send(JSON.stringify({ id: message.id, result: { connection } }));
      socket.send(JSON.stringify({ method: 'turn/started', params: { connection } }));
    });
  });
  const proxy = await startDesktopProxy({ port: 0, backendUrl: `ws://127.0.0.1:${address.port}`, backendToken: token,
    authorizePeer: async () => true, onState: state => { initializedConnections = state.initializedConnections; } });
  const peers: WebSocket[] = [];
  try {
    for (let index = 1; index <= 2; index++) {
      const peer = new WebSocket(proxy.url); peers.push(peer);
      await once(peer, 'open');
      const messages: Record<string, unknown>[] = [];
      const done = new Promise<void>(resolve => peer.on('message', data => { messages.push(JSON.parse(data.toString()) as Record<string, unknown>); if (messages.length === 2) resolve(); }));
      peer.send(JSON.stringify({ id: 1, method: 'initialize', params: { clientInfo: { name: 'desktop-test' } } }));
      await done;
      assert.deepEqual(messages[0], { id: 1, result: { connection: index } });
      assert.equal(messages[1]?.method, 'turn/started');
    }
    assert.equal(initializedConnections, 2); assert.equal(auth, true);
  } finally {
    for (const peer of peers) peer.terminate();
    await proxy.close();
    await new Promise<void>(resolve => upstream.close(() => resolve()));
  }
});

test('desktop proxy rejects unverified processes, foreign origins and unexpected URLs', async () => {
  let allow = false;
  const proxy = await startDesktopProxy({ port: 0, backendUrl: 'ws://127.0.0.1:1', backendToken: 'c'.repeat(43), authorizePeer: async () => allow });
  const refused = async (url: string, origin?: string) => {
    const ws = new WebSocket(url, origin ? { origin } : {});
    let opened = false;
    ws.on('open', () => { opened = true; ws.terminate(); });
    ws.on('error', () => {});
    await new Promise<void>(resolve => ws.once('close', () => resolve()));
    assert.equal(opened, false);
  };
  try {
    await refused(proxy.url);
    allow = true;
    await refused(proxy.url, 'https://unrelated.example');
    await refused(proxy.url + '?token=x');
  } finally { await proxy.close(); }
});

test('Windows TCP peer check accepts the exact process and rejects a different PID', { skip: process.platform !== 'win32' }, async () => {
  const proxy = await startDesktopProxy({ port: 0, backendUrl: 'ws://127.0.0.1:1', backendToken: 'd'.repeat(43),
    authorizePeer: async (peerPort, serverPort) => {
      assert.equal(await verifyDesktopTcpPeer(peerPort, serverPort, 1, process.execPath), false);
      return verifyDesktopTcpPeer(peerPort, serverPort, process.pid, process.execPath);
    } });
  const peer = new WebSocket(proxy.url);
  peer.on('error', () => {});
  try { await once(peer, 'open'); }
  finally { peer.terminate(); await proxy.close(); }
});

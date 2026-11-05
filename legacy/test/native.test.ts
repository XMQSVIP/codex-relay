import { test } from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import { randomUUID } from 'node:crypto';
import path from 'node:path';
import os from 'node:os';
import { encodeFrame, FrameDecoder } from '../src/transport/frames.js';
import { listNativeTools } from '../src/transport/native.js';

async function withPipe(handler: (socket: net.Socket, request: { id: string; method: string }) => void, run: (pipe: string) => Promise<void>) {
  const name = `codex-bridge-test-${randomUUID()}`;
  const pipe = process.platform === 'win32' ? `\\\\.\\pipe\\${name}` : path.join(os.tmpdir(), name);
  const sockets = new Set<net.Socket>();
  const server = net.createServer(socket => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {});
    const decoder = new FrameDecoder();
    socket.on('data', chunk => {
      for (const value of decoder.push(chunk)) handler(socket, value as { id: string; method: string });
    });
  });
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(pipe, resolve); });
  try { await run(pipe); }
  finally { for (const socket of sockets) socket.destroy(); await new Promise<void>(resolve => server.close(() => resolve())); }
}

test('native probe only sends tools/list, correlates ID and reads fragmented response', async () => {
  let requests = 0;
  await withPipe((socket, request) => {
    requests++; assert.equal(request.method, 'tools/list');
    const wire = Buffer.concat([
      encodeFrame({ jsonrpc: '2.0', id: 'unrelated', result: {} }),
      encodeFrame({ jsonrpc: '2.0', id: request.id, result: { tools: [{ name: 'read_thread', inputSchema: { description: '中文' } }] } }),
    ]);
    socket.write(wire.subarray(0, 5)); socket.write(wire.subarray(5));
  }, async pipe => { assert.equal((await listNativeTools(pipe))[0]?.name, 'read_thread'); });
  assert.equal(requests, 1);
});

test('native timeout does not retry the request', async () => {
  let requests = 0;
  await withPipe(() => { requests++; }, async pipe => {
    await assert.rejects(listNativeTools(pipe, 50), { code: 'NATIVE_TIMEOUT' });
  });
  assert.equal(requests, 1);
});

test('native explicit error and truncated result remain failures', async () => {
  await withPipe((socket, request) => socket.end(encodeFrame({ id: request.id, error: { message: 'rejected' } })), async pipe => {
    await assert.rejects(listNativeTools(pipe), { code: 'NATIVE_RPC_ERROR' });
  });
  await withPipe(socket => socket.end(Buffer.from([20, 0, 0, 0, 123])), async pipe => {
    await assert.rejects(listNativeTools(pipe), { code: 'TRUNCATED_FRAME' });
  });
});

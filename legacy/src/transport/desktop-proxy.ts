import { createServer } from 'node:http';
import WebSocket, { WebSocketServer, type RawData } from 'ws';
import { BridgeError } from '../errors.js';
import { loopbackWebSocketUrl, MAX_RPC_BYTES } from './rpc.js';

export interface DesktopProxyOptions {
  port: number;
  backendUrl: string;
  backendToken: string;
  authorizePeer(peerPort: number, serverPort: number): Promise<boolean>;
  onState?(state: { connections: number; initializedConnections: number }): void;
}

/** Transparent local transport, not a public API. Only the verified desktop process may upgrade. */
export async function startDesktopProxy(options: DesktopProxyOptions) {
  loopbackWebSocketUrl(options.backendUrl);
  const server = createServer((_request, response) => { response.writeHead(404); response.end(); });
  const sockets = new Set<WebSocket>();
  const peers = new Set<WebSocket>();
  const initialized = new Set<WebSocket>();
  const wss = new WebSocketServer({ noServer: true, maxPayload: MAX_RPC_BYTES, perMessageDeflate: false });
  let closing = false;
  let pendingUpgrades = 0;
  const publish = () => options.onState?.({ connections: peers.size, initializedConnections: initialized.size });
  server.on('upgrade', (request, socket, head) => {
    socket.on('error', () => socket.destroy());
    const address = server.address();
    if (closing || !address || typeof address === 'string' || peers.size + pendingUpgrades >= 8
        || request.url !== '/desktop' || request.headers.host !== `127.0.0.1:${address.port}`
        || request.socket.remoteAddress !== '127.0.0.1' || !request.socket.remotePort
        || (request.headers.origin !== undefined && request.headers.origin !== 'app://-')) {
      socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n'); return;
    }
    pendingUpgrades++;
    const timeout = setTimeout(() => socket.destroy(), 5000);
    void options.authorizePeer(request.socket.remotePort, address.port).then(allowed => {
      if (!allowed || closing || socket.destroyed) { socket.destroy(); return; }
      wss.handleUpgrade(request, socket, head, peer => wss.emit('connection', peer));
    }).catch(() => socket.destroy()).finally(() => { pendingUpgrades--; clearTimeout(timeout); });
  });
  wss.on('connection', (peer: WebSocket) => {
    if (closing) { peer.terminate(); return; }
    const backend = new WebSocket(options.backendUrl, {
      headers: { Authorization: `Bearer ${options.backendToken}` }, handshakeTimeout: 5000,
      maxPayload: MAX_RPC_BYTES, perMessageDeflate: false, followRedirects: false,
    });
    sockets.add(peer); sockets.add(backend); peers.add(peer); publish();
    let initId: string | number | undefined;
    let queue: Buffer[] = [];
    let queuedBytes = 0;
    let stopped = false;
    const stop = () => {
      if (stopped) return;
      stopped = true; queue = []; queuedBytes = 0;
      sockets.delete(peer); sockets.delete(backend); peers.delete(peer); initialized.delete(peer);
      peer.terminate(); backend.terminate(); publish();
    };
    const forward = (destination: WebSocket, data: Buffer) => {
      if (destination.readyState !== WebSocket.OPEN || destination.bufferedAmount + data.length > MAX_RPC_BYTES) { stop(); return; }
      destination.send(data, { binary: false }, error => { if (error) stop(); });
    };
    const bytes = (data: RawData) => Buffer.isBuffer(data) ? data : Array.isArray(data) ? Buffer.concat(data) : Buffer.from(data);
    peer.on('message', (data, binary) => {
      if (binary) { stop(); return; }
      const buffer = bytes(data);
      try {
        const message = JSON.parse(buffer.toString()) as Record<string, unknown>;
        if (message.method === 'initialize' && (typeof message.id === 'string' || typeof message.id === 'number')) initId = message.id;
      } catch { stop(); return; }
      if (backend.readyState === WebSocket.CONNECTING) {
        queuedBytes += buffer.length;
        if (queuedBytes > 1024 * 1024 || queue.length >= 64) { stop(); return; }
        queue.push(buffer);
      } else forward(backend, buffer);
    });
    backend.on('open', () => { for (const data of queue) { if (stopped) break; forward(backend, data); } queue = []; queuedBytes = 0; });
    backend.on('message', (data, binary) => {
      if (binary) { stop(); return; }
      const buffer = bytes(data);
      try {
        const message = JSON.parse(buffer.toString()) as Record<string, unknown>;
        if (initId !== undefined && message.id === initId && 'result' in message && !('error' in message)) { initialized.add(peer); initId = undefined; publish(); }
      } catch { stop(); return; }
      forward(peer, buffer);
    });
    for (const connection of [peer, backend]) { connection.on('error', stop); connection.on('close', stop); }
  });
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(options.port, '127.0.0.1', resolve); });
  const address = server.address();
  if (!address || typeof address === 'string') throw new BridgeError('PROXY_LISTEN', 'Cannot bind desktop transport.');
  return {
    port: address.port,
    url: `ws://127.0.0.1:${address.port}/desktop`,
    async close() {
      closing = true;
      for (const socket of sockets) socket.terminate();
      wss.close();
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    },
  };
}

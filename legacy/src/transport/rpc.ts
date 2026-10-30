import { randomUUID } from 'node:crypto';
import WebSocket from 'ws';
import { BridgeError } from '../errors.js';

export const MAX_RPC_BYTES = 8 * 1024 * 1024;
export type RpcNotification = { method: string; params?: unknown };
type Pending = { resolve(value: unknown): void; reject(error: Error): void; timer: NodeJS.Timeout };

export function loopbackWebSocketUrl(endpoint: string): URL {
  const url = new URL(endpoint);
  if (url.protocol !== 'ws:' || url.hostname !== '127.0.0.1' || !url.port || url.username || url.password
      || url.pathname !== '/' || url.search || url.hash) {
    throw new BridgeError('RPC_ENDPOINT', 'App-server must use an explicit ws://127.0.0.1:PORT endpoint.');
  }
  return url;
}

/** One connection, independent request IDs, no replay after a timeout or disconnect. */
export class AppServerRpc {
  private readonly pending = new Map<string, Pending>();
  private readonly subscribers = new Set<(event: RpcNotification) => void>();
  private readonly rejectedCallbackSubscribers = new Set<(method: string) => void>();
  private stopped = false;
  private constructor(private readonly socket: WebSocket, private readonly timeoutMs: number) {
    socket.on('message', (data, binary) => {
      try {
        if (binary) throw new Error('Binary RPC frame');
        const value: unknown = JSON.parse(data.toString());
        if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid RPC object');
        const message = value as Record<string, unknown>;
        if (typeof message.method === 'string') {
          if ('id' in message) {
            // This transport never approves tools, executes dynamic tools, or returns account tokens.
            if (typeof message.id !== 'string' && typeof message.id !== 'number') throw new Error('Invalid request ID');
            this.send({ id: message.id, error: { code: -32601, message: 'Client callbacks are disabled.' } });
            for (const subscriber of this.rejectedCallbackSubscribers) subscriber(message.method);
          } else {
            for (const subscriber of this.subscribers) subscriber({ method: message.method, params: message.params });
          }
          return;
        }
        if (typeof message.id !== 'string' && typeof message.id !== 'number') throw new Error('Missing response ID');
        if (('result' in message) === ('error' in message)) throw new Error('Invalid RPC response');
        const pending = this.pending.get(String(message.id));
        if (!pending) return; // A timed-out request may finish late; never submit it again.
        this.pending.delete(String(message.id));
        clearTimeout(pending.timer);
        if ('error' in message) {
          const code = message.error && typeof message.error === 'object' && 'code' in message.error ? message.error.code : 'unknown';
          pending.reject(new BridgeError('RPC_REMOTE', `App-server rejected the request (code ${typeof code === 'number' ? code : 'unknown'}).`));
        } else pending.resolve(message.result);
      } catch { this.fail(new BridgeError('RPC_PROTOCOL', 'Invalid app-server message or event consumer failure.')); }
    });
    socket.on('error', () => this.fail(new BridgeError('RPC_DISCONNECTED', 'App-server connection failed; requests were not replayed.')));
    socket.on('close', () => this.fail(new BridgeError('RPC_DISCONNECTED', 'App-server disconnected; requests were not replayed.')));
  }

  static async connect(endpoint: string, token: string, timeoutMs = 10_000, handshakeTimeoutMs = timeoutMs): Promise<AppServerRpc> {
    const url = loopbackWebSocketUrl(endpoint);
    if (!/^[A-Za-z0-9_-]{32,}$/.test(token)) throw new BridgeError('RPC_CREDENTIAL', 'A high-entropy capability token is required.');
    const socket = new WebSocket(url, {
      headers: { Authorization: `Bearer ${token}` }, handshakeTimeout: handshakeTimeoutMs,
      maxPayload: MAX_RPC_BYTES, perMessageDeflate: false, followRedirects: false,
    });
    // Install message handlers before the open event: notifications can arrive immediately.
    const rpc = new AppServerRpc(socket, timeoutMs);
    await new Promise<void>((resolve, reject) => {
      const failed = () => { cleanup(); rpc.close(); reject(new BridgeError('RPC_CONNECT', 'Cannot establish an authenticated app-server connection.')); };
      const opened = () => { cleanup(); resolve(); };
      const cleanup = () => { socket.off('open', opened); socket.off('error', failed); socket.off('close', failed); };
      socket.once('open', opened); socket.once('error', failed); socket.once('close', failed);
    });
    return rpc;
  }

  async initialize(name = 'codex_responses_bridge'): Promise<unknown> {
    const result = await this.request('initialize', {
      clientInfo: { name, version: '0.1.0' }, capabilities: { experimentalApi: true },
    });
    this.send({ method: 'initialized' });
    return result;
  }

  request(method: string, params: unknown): Promise<unknown> {
    if (this.stopped || this.socket.readyState !== WebSocket.OPEN) return Promise.reject(new BridgeError('RPC_CLOSED', 'App-server is not connected.'));
    if (this.pending.size >= 64) return Promise.reject(new BridgeError('RPC_PENDING_LIMIT', 'Too many outstanding app-server requests.'));
    const id = randomUUID();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new BridgeError('RPC_TIMEOUT', 'Request outcome is unknown; it was not replayed.'));
      }, this.timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try { this.send({ id, method, params }); }
      catch (error) { clearTimeout(timer); this.pending.delete(id); reject(error); }
    });
  }

  subscribe(callback: (event: RpcNotification) => void): () => void {
    this.subscribers.add(callback);
    return () => { this.subscribers.delete(callback); };
  }

  observeRejectedCallbacks(callback: (method: string) => void): () => void {
    this.rejectedCallbackSubscribers.add(callback);
    return () => { this.rejectedCallbackSubscribers.delete(callback); };
  }

  private send(message: unknown): void {
    const data = JSON.stringify(message);
    if (Buffer.byteLength(data) > MAX_RPC_BYTES || this.socket.bufferedAmount > MAX_RPC_BYTES) {
      throw new BridgeError('RPC_SIZE', 'App-server message or send buffer exceeds the limit.');
    }
    this.socket.send(data, error => { if (error) this.fail(new BridgeError('RPC_SEND', 'App-server send failed; request outcome may be unknown.')); });
  }

  private fail(error: Error): void {
    if (this.stopped) return;
    this.stopped = true;
    for (const request of this.pending.values()) { clearTimeout(request.timer); request.reject(error); }
    this.pending.clear(); this.subscribers.clear(); this.rejectedCallbackSubscribers.clear(); this.socket.terminate();
  }

  close(): void { this.fail(new BridgeError('RPC_CLOSED', 'App-server connection closed.')); }
}

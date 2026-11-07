import { createServer, type ServerResponse } from 'node:http';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { z } from 'zod';
import { BridgeError, errorCode } from '../errors.js';
import { TextSseGuard } from './text-stream.js';
import { lanBaseUrl, validateForwardHeaders, type LanProvider } from '../shared/provider.js';

const inputPart = z.object({ type: z.enum(['input_text', 'output_text']), text: z.string() });
const inputMessage = z.object({ type: z.literal('message').optional(), role: z.enum(['system', 'developer', 'user', 'assistant']), content: z.union([z.string(), z.array(inputPart)]) });
const additionalTools = z.object({ type: z.literal('additional_tools'), role: z.literal('developer'), tools: z.array(z.unknown()) });
const requestShape = z.object({
  model: z.string().min(1), stream: z.literal(true),
  input: z.array(z.union([inputMessage, additionalTools])).transform(items => items.filter((item): item is z.infer<typeof inputMessage> => item.type !== 'additional_tools')),
  tools: z.array(z.never()).optional(), previous_response_id: z.null().optional(),
}).passthrough();

export interface GuardRouteStats {
  state: 'unused' | 'active' | 'completed' | 'cancelled' | 'blocked' | 'failed';
  requests: number; upstreamRequests: number; forwardedEvents: number;
  firstForwardedAt: number | null; firstTextForwardedAt: number | null; upstreamEndedAt: number | null;
  completedAt: number | null; failureCode: string | null;
}

interface GuardOptions {
  authorizePeer(peerPort: number, guardPort: number): Promise<boolean>;
  timeoutMs?: number;
}

/** Isolated protocol tests only. Optional header names are for synthetic credentials in a fresh test home. */
export async function startLoopbackTextGuard(options: GuardOptions & { upstreamBaseUrl: string; fixtureCredentialHeaders?: string[] }) {
  const upstream = new URL(options.upstreamBaseUrl);
  if (upstream.protocol !== 'http:' || upstream.hostname !== '127.0.0.1' || !upstream.port
      || upstream.username || upstream.password || upstream.search || upstream.hash
      || upstream.pathname !== '/v1') throw new BridgeError('GUARD_FIXTURE_ENDPOINT', 'This prototype permits only an explicit loopback /v1 fixture. Remote forwarding is not enabled.');
  const fixtureHeaders = validateForwardHeaders(options.fixtureCredentialHeaders ?? []);
  return startTextGuard(options, upstream, fixtureHeaders, fixtureHeaders);
}

/** Real forwarding is pinned to effective, administrator-selected LAN provider metadata. */
export async function startLanTextGuard(options: GuardOptions & { provider: LanProvider }) {
  const upstream = new URL(lanBaseUrl(options.provider.baseUrl));
  const forwardHeaders = validateForwardHeaders(options.provider.forwardHeaders);
  const requiredHeaders = validateForwardHeaders(options.provider.requiredHeaders);
  if (requiredHeaders.some(name => !forwardHeaders.includes(name))) throw new BridgeError('GUARD_AUTH_CONFIG', 'Required headers must be explicitly forwarded.');
  return startTextGuard(options, upstream, forwardHeaders, requiredHeaders);
}

async function startTextGuard(options: GuardOptions, upstream: URL, forwardHeaders: string[], requiredHeaders: string[]) {
  const routes = new Map<string, { model: string; stats: GuardRouteStats }>();
  const controllers = new Set<AbortController>();
  let closing = false;
  const diagnostics = { received: 0, badEnvelope: 0, unknownRoute: 0, rejectedPeer: 0, upgradesRejected: 0, invalidRequestFields: [] as string[] };
  const respond = (response: ServerResponse, status: number, code: string) => {
    if (response.destroyed || response.writableEnded) return;
    response.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' });
    response.end(JSON.stringify({ error: { code, message: 'Text guard rejected the request or upstream response.', type: 'invalid_request_error' } }));
  };
  const server = createServer((request, response) => {
    void (async () => {
      diagnostics.received++;
      const address = server.address();
      if (closing || !address || typeof address === 'string' || request.method !== 'POST'
          || request.headers.host !== `127.0.0.1:${address.port}` || request.headers.origin !== undefined
          || request.socket.remoteAddress !== '127.0.0.1' || !request.socket.remotePort) { diagnostics.badEnvelope++; respond(response, 403, 'GUARD_PEER'); return; }
      const route = routes.get(request.url ?? '');
      if (!route) { diagnostics.unknownRoute++; respond(response, 404, 'GUARD_ROUTE'); return; }
      const abort = new AbortController(); controllers.add(abort);
      const timer = setTimeout(() => abort.abort(new BridgeError('GUARD_TIMEOUT', 'The provider request timed out.')), options.timeoutMs ?? 60_000);
      abort.signal.addEventListener('abort', () => { if (!request.complete) request.destroy(); }, { once: true });
      const disconnected = () => { if (!response.writableEnded) abort.abort(new BridgeError('GUARD_CANCELLED', 'The native downstream closed the request.')); };
      response.once('close', disconnected);
      let claimed = false;
      try {
        const authorized = await Promise.race([
          options.authorizePeer(request.socket.remotePort, address.port),
          new Promise<false>(resolve => abort.signal.addEventListener('abort', () => resolve(false), { once: true })),
        ]);
        if (!authorized || closing || abort.signal.aborted) { diagnostics.rejectedPeer++; respond(response, 403, 'GUARD_PEER'); return; }
        route.stats.requests++;
        if (route.stats.state !== 'unused') { respond(response, 409, 'GUARD_NO_REPLAY'); return; }
        claimed = true; route.stats.state = 'active';
        if (!request.headers['content-type']?.startsWith('application/json') || request.headers['content-encoding']) throw new BridgeError('GUARD_REQUEST', 'Only uncompressed JSON requests are supported.');
        const chunks: Buffer[] = []; let bytes = 0;
        for await (const data of request) {
          if (abort.signal.aborted) throw new BridgeError('GUARD_CANCELLED', 'Guard request stopped.');
          bytes += data.length;
          if (bytes > 8 * 1024 * 1024) throw new BridgeError('GUARD_REQUEST_SIZE', 'Guard request is too large.');
          chunks.push(data);
        }
        const parsed = requestShape.safeParse(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks))));
        if (!parsed.success) {
          diagnostics.invalidRequestFields = parsed.error.issues.map(issue => `${issue.path.join('.')}:${issue.code}`);
          throw new BridgeError('GUARD_REQUEST_SHAPE', 'Unsupported input shape for the text-only guard.');
        }
        const body = parsed.data;
        if (body.model !== route.model) throw new BridgeError('GUARD_MODEL', 'Unexpected model for this route.');
        // Values come only from the PID-verified native backend. Never retain them in route state or logs.
        if (request.headers.cookie || request.headers['chatgpt-account-id'] || (request.headers.authorization && !forwardHeaders.includes('authorization'))) {
          throw new BridgeError('GUARD_CREDENTIALS', 'Unexpected credentials on this provider route.');
        }
        const outboundHeaders: Record<string, string> = { 'content-type': 'application/json', accept: 'text/event-stream', 'accept-encoding': 'identity' };
        for (const name of forwardHeaders) {
          const value = request.headers[name];
          if (value !== undefined) {
            if (typeof value !== 'string' || !value || value.length > 16_384) throw new BridgeError('GUARD_CREDENTIALS', 'Invalid configured provider header.');
            outboundHeaders[name] = value;
          }
        }
        if (requiredHeaders.some(name => !outboundHeaders[name])) throw new BridgeError('GUARD_CREDENTIALS', 'A required native provider header is missing.');
        route.stats.upstreamRequests++;
        const result = await fetch(`${upstream.href}/responses`, {
          method: 'POST', headers: outboundHeaders,
          body: JSON.stringify({ ...body, tools: [], tool_choice: 'none', parallel_tool_calls: false, store: false }),
          redirect: 'error', signal: abort.signal,
        });
        if (result.status !== 200 || !result.headers.get('content-type')?.startsWith('text/event-stream') || !result.body) {
          await result.body?.cancel();
          throw new BridgeError(result.status === 401 || result.status === 403 ? 'GUARD_UPSTREAM_AUTH' : 'GUARD_UPSTREAM', 'The selected provider did not return an accepted SSE response.');
        }
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-store', 'x-accel-buffering': 'no' });
        response.flushHeaders();
        const filter = new TextSseGuard();
        const forward = async (frames: string[]) => {
          for (const frame of frames) {
            route.stats.forwardedEvents++;
            route.stats.firstForwardedAt ??= Date.now();
            if (frame.startsWith('event: response.output_text.delta\n')) route.stats.firstTextForwardedAt ??= Date.now();
            if (!response.write(frame)) await once(response, 'drain', { signal: abort.signal });
          }
        };
        for await (const data of result.body) await forward(filter.push(data));
        route.stats.upstreamEndedAt = Date.now();
        await forward(filter.finish());
        route.stats.state = 'completed'; route.stats.completedAt = Date.now();
        response.end();
      } catch (error) {
        if (claimed) {
          const failure: unknown = abort.signal.aborted ? abort.signal.reason : error;
          route.stats.failureCode = errorCode(failure);
          route.stats.state = route.stats.failureCode === 'GUARD_CANCELLED' ? 'cancelled'
            : failure instanceof BridgeError && failure.code.startsWith('GUARD_') ? 'blocked' : 'failed';
        }
        if (!response.destroyed && !response.writableEnded) {
          if (!response.headersSent) respond(response, 502, 'GUARD_STOPPED');
          else response.end('event: error\ndata: {"type":"error","code":"bridge_guard_blocked","message":"Text-only guard stopped the upstream response."}\n\n');
        }
      } finally {
        clearTimeout(timer); abort.abort(); controllers.delete(abort); response.off('close', disconnected);
      }
    })().catch(() => { if (!response.destroyed) response.destroy(); });
  });
  server.on('upgrade', (_request, socket) => { diagnostics.upgradesRejected++; socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n'); });
  server.on('connect', (_request, socket) => socket.destroy());
  server.headersTimeout = 5000; server.requestTimeout = 15_000;
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  if (!address || typeof address === 'string') throw new BridgeError('GUARD_LISTEN', 'Guard could not bind locally.');
  return {
    diagnostics: () => ({ ...diagnostics }),
    createRoute(model: string) {
      if (closing || routes.size >= 128) throw new BridgeError('GUARD_ROUTE_LIMIT', 'Guard route capacity reached.');
      const routePath = `/${randomBytes(32).toString('base64url')}/v1`;
      const stats: GuardRouteStats = { state: 'unused', requests: 0, upstreamRequests: 0, forwardedEvents: 0,
        firstForwardedAt: null, firstTextForwardedAt: null, upstreamEndedAt: null, completedAt: null, failureCode: null };
      routes.set(`${routePath}/responses`, { model, stats });
      return {
        baseUrl: `http://127.0.0.1:${address.port}${routePath}`, snapshot: (): GuardRouteStats => ({ ...stats }),
        release: () => { if (stats.state === 'active') throw new BridgeError('GUARD_ROUTE_ACTIVE', 'An active route cannot be released.'); routes.delete(`${routePath}/responses`); },
      };
    },
    async close() {
      closing = true;
      for (const abort of controllers) abort.abort();
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
      routes.clear();
    },
  };
}

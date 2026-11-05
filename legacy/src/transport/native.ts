import net from 'node:net';
import { randomUUID } from 'node:crypto';
import { z } from 'zod';
import { BridgeError } from '../errors.js';
import { encodeFrame, FrameDecoder } from './frames.js';

export const nativeToolsSchema = z.object({
  tools: z.array(z.object({
    name: z.string(),
    inputSchema: z.record(z.string(), z.unknown()).optional(),
  })),
});
export type NativeTool = z.infer<typeof nativeToolsSchema>['tools'][number];

// Diagnostics deliberately have no arbitrary method or tools/call escape hatch.
export async function listNativeTools(pipe: string, timeoutMs = 5000): Promise<NativeTool[]> {
  return nativeToolsSchema.parse(await requestToolsList(pipe, timeoutMs)).tools;
}

async function requestToolsList(pipe: string, timeoutMs: number): Promise<unknown> {
  const id = randomUUID();
  const frame = encodeFrame({ jsonrpc: '2.0', id, method: 'tools/list' });
  return new Promise((resolve, reject) => {
    const decoder = new FrameDecoder();
    const socket = net.createConnection(pipe);
    let settled = false;
    let ignoredMessages = 0;
    const finish = (error?: Error, value?: unknown) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      if (error) reject(error);
      else resolve(value);
    };
    const timer = setTimeout(() => finish(new BridgeError('NATIVE_TIMEOUT', 'Native tools discovery timed out.')), timeoutMs);
    socket.once('connect', () => socket.write(frame));
    socket.on('data', (chunk: Buffer) => {
      try {
        for (const raw of decoder.push(chunk)) {
          const response = z.object({
            jsonrpc: z.literal('2.0').optional(),
            id: z.union([z.string(), z.number()]).optional(),
            result: z.unknown().optional(),
            error: z.unknown().optional(),
          }).parse(raw);
          if (response.id !== id) {
            if (++ignoredMessages > 64) throw new BridgeError('NATIVE_PROTOCOL', 'Too many unrelated native messages.');
            continue;
          }
          if (response.error !== undefined) {
            finish(new BridgeError('NATIVE_RPC_ERROR', 'Desktop rejected the read-only tools/list request.'));
          } else if (!Object.hasOwn(response, 'result')) {
            finish(new BridgeError('NATIVE_PROTOCOL', 'Native response has no result.'));
          } else finish(undefined, response.result);
        }
      } catch (error) {
        finish(error instanceof Error ? error : new BridgeError('NATIVE_PROTOCOL', 'Invalid native response.'));
      }
    });
    socket.once('error', () => finish(new BridgeError('NATIVE_CONNECTION', 'Could not connect to the verified desktop pipe.')));
    socket.once('close', () => {
      if (settled) return;
      try { decoder.finish(); }
      catch (error) { finish(error as Error); return; }
      finish(new BridgeError('NATIVE_DISCONNECTED', 'Desktop disconnected before answering tools/list.'));
    });
  });
}

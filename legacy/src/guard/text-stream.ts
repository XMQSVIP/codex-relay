import { z } from 'zod';
import { BridgeError } from '../errors.js';

const id = z.string().min(1).max(256);
const index = z.number().int().min(0).max(4096);
const part = z.discriminatedUnion('type', [
  z.object({ type: z.literal('output_text'), text: z.string(), annotations: z.array(z.unknown()).optional() }).transform(p => ({ type: p.type, text: p.text, annotations: [] })),
  z.object({ type: z.literal('refusal'), refusal: z.string() }),
]);
const message = z.object({
  type: z.literal('message'), id, role: z.literal('assistant'), content: z.array(part),
  status: z.enum(['in_progress', 'completed', 'incomplete']).optional(),
  phase: z.enum(['commentary', 'final_answer']).nullable().optional(),
});
const usage = z.object({
  input_tokens: z.number().int().nonnegative(), output_tokens: z.number().int().nonnegative(), total_tokens: z.number().int().nonnegative(),
  input_tokens_details: z.object({ cached_tokens: z.number().int().nonnegative().optional() }).optional(),
  output_tokens_details: z.object({ reasoning_tokens: z.number().int().nonnegative().optional() }).optional(),
});
const reasonEvents = new Set([
  'response.reasoning_text.delta', 'response.reasoning_text.done',
  'response.reasoning_summary_text.delta', 'response.reasoning_summary_text.done',
  'response.reasoning_summary_part.added', 'response.reasoning_summary_part.done',
]);

/** Reconstruct allowlisted events. No original event object crosses the boundary. */
export class TextEventGuard {
  private readonly items = new Map<string, number>();
  private responseId: string | undefined;
  private terminal = false;
  private poisoned = false;

  accept(value: unknown, eventName?: string): Record<string, unknown> | null {
    if (this.poisoned) throw new BridgeError('GUARD_CLOSED', 'Response guard is closed.');
    try {
      if (this.terminal) throw new Error('Event after completion');
      const base = z.object({ type: z.string(), sequence_number: z.number().int().nonnegative().optional() }).parse(value);
      if (eventName && base.type !== eventName) throw new Error('SSE event mismatch');
      const envelope = { type: base.type, ...(base.sequence_number === undefined ? {} : { sequence_number: base.sequence_number }) };
      if (reasonEvents.has(base.type)) return null;
      if (base.type === 'response.created' || base.type === 'response.in_progress' || base.type === 'response.completed') {
        const parsed = z.object({ response: z.object({
          id, object: z.literal('response').optional(), model: z.string().optional(), created_at: z.number().optional(),
          status: z.enum(['queued', 'in_progress', 'completed']).optional(), output: z.array(z.unknown()).optional(), usage: usage.nullable().optional(),
        }) }).parse(value);
        if (this.responseId !== undefined && this.responseId !== parsed.response.id) throw new Error('Response identity changed');
        this.responseId = parsed.response.id;
        const output = (parsed.response.output ?? []).flatMap(item => {
          const kind = z.object({ type: z.string() }).parse(item);
          if (kind.type === 'reasoning') return [];
          return [message.parse(item)];
        });
        if (base.type === 'response.completed') this.terminal = true;
        return { ...envelope, response: { ...parsed.response, output } };
      }
      if (base.type === 'response.output_item.added' || base.type === 'response.output_item.done') {
        const parsed = z.object({ output_index: index.optional(), item: z.object({ type: z.string() }).passthrough() }).parse(value);
        if (parsed.item.type === 'reasoning') return null;
        const item = message.parse(parsed.item);
        const outputIndex = parsed.output_index ?? 0;
        if (this.items.has(item.id) && this.items.get(item.id) !== outputIndex) throw new Error('Output identity changed');
        this.items.set(item.id, outputIndex);
        return { ...envelope, output_index: outputIndex, item };
      }
      if (base.type === 'response.content_part.added' || base.type === 'response.content_part.done') {
        const parsed = z.object({ item_id: id, output_index: index, content_index: index, part }).parse(value);
        this.assertItem(parsed.item_id, parsed.output_index);
        return { ...envelope, ...parsed };
      }
      if (base.type === 'response.output_text.delta' || base.type === 'response.output_text.done'
          || base.type === 'response.refusal.delta' || base.type === 'response.refusal.done') {
        const delta = base.type.endsWith('.delta');
        const parsed = z.object({ item_id: id, output_index: index, content_index: index,
          delta: z.string().optional(), text: z.string().optional(), refusal: z.string().optional(),
        }).parse(value);
        this.assertItem(parsed.item_id, parsed.output_index);
        const field = delta ? 'delta' : base.type.startsWith('response.refusal.') ? 'refusal' : 'text';
        const text = parsed[field];
        if (text === undefined) throw new Error('Missing text');
        return { ...envelope, item_id: parsed.item_id, output_index: parsed.output_index, content_index: parsed.content_index, [field]: text };
      }
      throw new Error('Unapproved event');
    } catch {
      this.poisoned = true;
      throw new BridgeError('GUARD_EVENT_BLOCKED', 'Upstream returned a tool, unsupported event, or invalid response.');
    }
  }

  private assertItem(itemId: string, outputIndex: number): void {
    if (this.items.get(itemId) !== outputIndex) throw new Error('Text without a matching assistant message');
  }

  finish(): void {
    if (this.poisoned || !this.terminal) throw new BridgeError('GUARD_TRUNCATED', 'Upstream did not complete a validated text response.');
  }
}

/** Streaming SSE decoding, bounded per event and per response, with fatal UTF-8 validation. */
export class TextSseGuard {
  private readonly decoder = new TextDecoder('utf-8', { fatal: true });
  private readonly events = new TextEventGuard();
  private buffer = '';
  private bytes = 0;
  private poisoned = false;
  private completionFrame: string | undefined;
  private finalized = false;
  constructor(private readonly maxEventBytes = 1024 * 1024, private readonly maxResponseBytes = 64 * 1024 * 1024) {}

  push(chunk: Uint8Array): string[] {
    if (this.poisoned || this.finalized) throw new BridgeError('GUARD_CLOSED', 'Response guard is closed.');
    try {
      this.bytes += chunk.byteLength;
      if (this.bytes > this.maxResponseBytes) throw new Error('Response size');
      this.buffer += this.decoder.decode(chunk, { stream: true });
      const output: string[] = [];
      while (true) {
        const separator = /\r?\n\r?\n/.exec(this.buffer);
        if (!separator) break;
        const frame = this.buffer.slice(0, separator.index);
        this.buffer = this.buffer.slice(separator.index + separator[0].length);
        if (Buffer.byteLength(frame) > this.maxEventBytes) throw new Error('Event size');
        const data: string[] = []; let eventName: string | undefined;
        for (const line of frame.split(/\r?\n/)) {
          if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
          else if (line.startsWith('event:')) eventName = line.slice(6).replace(/^ /, '');
          else if (line && !line.startsWith(':') && !line.startsWith('id:') && !line.startsWith('retry:')) throw new Error('SSE field');
        }
        if (!data.length) continue;
        if (data.join('\n') === '[DONE]') { this.events.finish(); continue; }
        const event = this.events.accept(JSON.parse(data.join('\n')), eventName);
        if (event) {
          const encoded = `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
          // Hold success until EOF. A trailing invalid event must not leave a successful turn.
          if (event.type === 'response.completed') this.completionFrame = encoded;
          else output.push(encoded);
        }
      }
      if (Buffer.byteLength(this.buffer) > this.maxEventBytes) throw new Error('Incomplete event size');
      return output;
    } catch (error) {
      this.poisoned = true;
      throw error instanceof BridgeError ? error : new BridgeError('GUARD_SSE', 'Malformed, oversized, or invalidly encoded upstream stream.');
    }
  }

  finish(): string[] {
    try {
      this.buffer += this.decoder.decode();
      if (this.poisoned || this.finalized || this.buffer.trim() || !this.completionFrame) throw new Error('Truncated SSE');
      this.events.finish();
      this.finalized = true;
      return [this.completionFrame];
    } catch {
      this.poisoned = true;
      throw new BridgeError('GUARD_TRUNCATED', 'Upstream did not complete a validated text stream.');
    }
  }
}

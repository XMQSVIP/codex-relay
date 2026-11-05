import { BridgeError } from '../errors.js';

export const MAX_NATIVE_FRAME_BYTES = 8 * 1024 * 1024;
export const MAX_NATIVE_REQUEST_BYTES = 128 * 1024;

export function encodeFrame(value: unknown, maxBytes = MAX_NATIVE_REQUEST_BYTES): Buffer {
  const bytes = Buffer.from(JSON.stringify(value), 'utf8');
  if (bytes.length === 0 || bytes.length > maxBytes) {
    throw new BridgeError('FRAME_SIZE', 'Outgoing native frame exceeds the permitted size.');
  }
  const prefix = Buffer.alloc(4);
  prefix.writeUInt32LE(bytes.length);
  return Buffer.concat([prefix, bytes]);
}

/** Decode byte frames before UTF-8: a pipe read may split any code point or frame. */
export class FrameDecoder {
  private header = Buffer.alloc(4);
  private headerUsed = 0;
  private body: Buffer | undefined;
  private bodyUsed = 0;
  private poisoned = false;

  constructor(private readonly maxBytes = MAX_NATIVE_FRAME_BYTES) {}

  push(chunk: Buffer): unknown[] {
    if (this.poisoned) throw new BridgeError('DECODER_CLOSED', 'Decoder is no longer usable.');
    const values: unknown[] = [];
    let offset = 0;
    try {
      while (offset < chunk.length) {
        if (!this.body) {
          const count = Math.min(4 - this.headerUsed, chunk.length - offset);
          chunk.copy(this.header, this.headerUsed, offset, offset + count);
          this.headerUsed += count;
          offset += count;
          if (this.headerUsed < 4) continue;
          const size = this.header.readUInt32LE();
          if (size === 0 || size > this.maxBytes) {
            throw new BridgeError('FRAME_SIZE', 'Incoming native frame has an invalid length.');
          }
          this.body = Buffer.alloc(size);
        }
        const count = Math.min(this.body.length - this.bodyUsed, chunk.length - offset);
        chunk.copy(this.body, this.bodyUsed, offset, offset + count);
        this.bodyUsed += count;
        offset += count;
        if (this.bodyUsed === this.body.length) {
          try {
            const text = new TextDecoder('utf-8', { fatal: true }).decode(this.body);
            values.push(JSON.parse(text));
          } catch {
            throw new BridgeError('FRAME_ENCODING', 'Native frame is not valid UTF-8 JSON.');
          }
          this.body = undefined;
          this.bodyUsed = 0;
          this.headerUsed = 0;
        }
      }
      return values;
    } catch (error) {
      this.poisoned = true;
      this.body = undefined;
      throw error;
    }
  }

  finish(): void {
    if (this.headerUsed || this.body) {
      this.poisoned = true;
      throw new BridgeError('TRUNCATED_FRAME', 'Native pipe closed before the response frame completed.');
    }
  }
}

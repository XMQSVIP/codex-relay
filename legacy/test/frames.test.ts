import { test } from 'node:test';
import assert from 'node:assert/strict';
import { encodeFrame, FrameDecoder } from '../src/transport/frames.js';

test('native framing preserves Chinese and emoji across every possible split', () => {
  const value = { text: '中文 🛰️\n精确关联', id: 'resp_123' };
  const wire = encodeFrame(value);
  for (let split = 0; split <= wire.length; split++) {
    const decoder = new FrameDecoder();
    assert.deepEqual([...decoder.push(wire.subarray(0, split)), ...decoder.push(wire.subarray(split))], [value]);
    decoder.finish();
  }
});

test('multiple frames and byte-at-a-time delivery', () => {
  const values = [{ id: 1 }, { delta: '🍀' }, { status: 'completed' }];
  const wire = Buffer.concat(values.map(value => encodeFrame(value)));
  const decoder = new FrameDecoder();
  assert.deepEqual([...wire].flatMap(byte => decoder.push(Buffer.from([byte]))), values);
  decoder.finish();
});

test('reject oversize frame before allocating payload and never reuse failed decoder', () => {
  const decoder = new FrameDecoder(10);
  const header = Buffer.alloc(4); header.writeUInt32LE(0xffffffff);
  assert.throws(() => decoder.push(header), { code: 'FRAME_SIZE' });
  assert.throws(() => decoder.push(encodeFrame({})), { code: 'DECODER_CLOSED' });
  assert.throws(() => encodeFrame('long payload', 3), { code: 'FRAME_SIZE' });
});

test('reject zero length, invalid JSON and invalid UTF-8', () => {
  assert.throws(() => new FrameDecoder().push(Buffer.alloc(4)), { code: 'FRAME_SIZE' });
  assert.throws(() => new FrameDecoder().push(Buffer.from([1, 0, 0, 0, 123])), { code: 'FRAME_ENCODING' });
  assert.throws(() => new FrameDecoder().push(Buffer.from([3, 0, 0, 0, 34, 255, 34])), { code: 'FRAME_ENCODING' });
});

test('closed stream with partial header or body is not a successful response', () => {
  for (const bytes of [Buffer.from([4]), encodeFrame({ result: 'ok' }).subarray(0, 7)]) {
    const decoder = new FrameDecoder(); decoder.push(bytes);
    assert.throws(() => decoder.finish(), { code: 'TRUNCATED_FRAME' });
  }
});

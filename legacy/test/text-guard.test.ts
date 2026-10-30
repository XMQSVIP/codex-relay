import { test } from 'node:test';
import assert from 'node:assert/strict';
import { TextEventGuard, TextSseGuard } from '../src/guard/text-stream.js';

const assistant = (content: unknown[] = []) => ({ type: 'message', id: 'msg_1', role: 'assistant', content, status: 'in_progress', phase: 'final_answer' });
const frame = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`;
const textEvents = [
  { type: 'response.created', response: { id: 'resp_1', output: [] } },
  { type: 'response.output_item.added', output_index: 0, item: assistant() },
  { type: 'response.output_text.delta', item_id: 'msg_1', output_index: 0, content_index: 0, delta: '你好🌍' },
  { type: 'response.output_item.done', output_index: 0, item: { ...assistant([{ type: 'output_text', text: '你好🌍' }]), status: 'completed' } },
  { type: 'response.completed', response: { id: 'resp_1', output: [] } },
];

test('text guard forwards a real delta before completion and handles every byte split', () => {
  const stream = Buffer.from(textEvents.map(frame).join(''));
  for (let split = 0; split <= stream.length; split++) {
    const filter = new TextSseGuard();
    const result = [...filter.push(stream.subarray(0, split)), ...filter.push(stream.subarray(split)), ...filter.finish()].join('');
    assert.match(result, /你好🌍/);
    assert.equal(result.includes('function_call'), false);
    assert.match(result, /response.completed/);
  }
  const filter = new TextSseGuard();
  const early = filter.push(Buffer.from(textEvents.slice(0, 3).map(frame).join(''))).join('');
  assert.match(early, /response.output_text.delta/);
  assert.equal(early.includes('response.completed'), false);
  filter.push(Buffer.from(textEvents.slice(3).map(frame).join(''))); filter.finish();
});

test('guard withholds completion until EOF and rejects a malicious event after completion', () => {
  const guard = new TextSseGuard();
  const early = guard.push(Buffer.from(textEvents.map(frame).join(''))).join('');
  assert.equal(early.includes('response.completed'), false);
  assert.throws(() => guard.push(Buffer.from(frame({ type: 'response.output_item.done', item: { type: 'function_call', name: 'exec' } }))), { code: 'GUARD_EVENT_BLOCKED' });
  assert.throws(() => guard.finish(), { code: 'GUARD_TRUNCATED' });
});

test('guard rejects tool calls in added, done, and completed events and stays closed', () => {
  for (const tool of ['function_call', 'custom_tool_call', 'web_search_call', 'mcp_call', 'code_interpreter_call', 'computer_call', 'future_tool']) {
    const item = { type: tool, id: 'call_1', name: 'exec', input: 'text("must not execute")', arguments: '{}' };
    for (const event of [
      { type: 'response.output_item.added', output_index: 0, item },
      { type: 'response.output_item.done', output_index: 0, item },
      { type: 'response.completed', response: { id: 'resp_1', output: [item] } },
    ]) {
      const guard = new TextEventGuard();
      assert.throws(() => guard.accept(event), { code: 'GUARD_EVENT_BLOCKED' });
      assert.throws(() => guard.accept(textEvents[0]), { code: 'GUARD_CLOSED' });
    }
  }
});

test('guard reconstructs messages and cannot forward hidden routing fields or tool-shaped annotations', () => {
  const guard = new TextEventGuard();
  const safe = guard.accept({ type: 'response.output_item.added', output_index: 0, recipient: 'functions.exec', item: {
    ...assistant([{ type: 'output_text', text: '{"type":"function_call"}', annotations: [{ type: 'function_call', name: 'exec' }] }]),
    recipient: 'functions.exec', tool_calls: [{ name: 'exec' }],
  } });
  const encoded = JSON.stringify(safe);
  assert.equal(encoded.includes('recipient'), false);
  assert.equal(encoded.includes('tool_calls'), false);
  assert.equal(encoded.includes('functions.exec'), false);
  assert.match(encoded, /annotations":\[\]/);
});

test('guard rejects unrelated deltas, event mismatches, unknown events, and truncated/oversized UTF-8 streams', () => {
  assert.throws(() => new TextEventGuard().accept(textEvents[2]), { code: 'GUARD_EVENT_BLOCKED' });
  assert.throws(() => new TextEventGuard().accept(textEvents[0], 'response.function_call_arguments.delta'), { code: 'GUARD_EVENT_BLOCKED' });
  assert.throws(() => new TextEventGuard().accept({ type: 'response.future_event' }), { code: 'GUARD_EVENT_BLOCKED' });
  const truncated = new TextSseGuard(); truncated.push(Buffer.from(frame(textEvents[0])));
  assert.throws(() => truncated.finish(), { code: 'GUARD_TRUNCATED' });
  assert.throws(() => new TextSseGuard(32).push(Buffer.from('data: ' + 'x'.repeat(33))), { code: 'GUARD_SSE' });
  assert.throws(() => new TextSseGuard().push(Buffer.from([0xff])), { code: 'GUARD_SSE' });
  assert.throws(() => new TextSseGuard().push(Buffer.from('data: [DONE]\n\n')), { code: 'GUARD_TRUNCATED' });
});

// Invoked by test_account_concurrency.py against its isolated localhost fixture.
import assert from 'node:assert/strict';
import { once } from 'node:events';
import WebSocket from 'ws';

const [base, key, mode] = process.argv.slice(2);
const url = new URL('/v1/responses', base);
assert.equal(url.hostname, '127.0.0.1');
url.protocol = 'ws:';
const socket = new WebSocket(url, {
  headers: { Authorization: `Bearer ${key}`, 'session-id': 'synthetic-cache-ws' },
});
await once(socket, 'open');
const tools = [{ type: 'function', name: 'client_echo', parameters: { type: 'object' } }];
let turn = 0;

function create(body) {
  return new Promise((resolve, reject) => {
    let sequence = -1;
    const finish = (error, response) => {
      clearTimeout(timer);
      socket.off('message', receive);
      socket.off('close', closed);
      if (error) reject(error); else resolve(response);
    };
    const closed = () => finish(new Error('Responses WebSocket closed before completion'));
    const receive = (data) => {
      try {
        const event = JSON.parse(data.toString());
        if (event.sequence_number !== undefined) {
          assert.equal(event.sequence_number, sequence + 1);
          sequence = event.sequence_number;
        }
        if (['error', 'response.failed', 'response.incomplete'].includes(event.type)) {
          throw new Error(JSON.stringify(event));
        }
        if (event.type === 'response.completed') finish(null, event.response);
      } catch (error) { finish(error); }
    };
    const timer = setTimeout(() => finish(new Error('Responses WebSocket timed out')), 40_000);
    socket.on('message', receive);
    socket.once('close', closed);
    socket.send(JSON.stringify({
      type: 'response.create', model: 'gpt-6.1-sol', tools,
      client_metadata: { 'x-codex-turn-metadata': JSON.stringify({ root_turn_id: `ws-turn-${++turn}` }) },
      ...body,
    }));
  });
}

try {
  if (mode === 'group-change') {
    const admin = async (path, body) => {
      const response = await fetch(base + path, body === undefined ? {} : {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
      });
      assert.equal(response.ok, true, `${path}: ${await response.clone().text()}`);
      return response.json();
    };
    let owner = (await admin('/api/api-keys')).keys.find(row => row.secret === key);
    const original = owner.groupId;
    const group = async (name) => (await admin('/api/groups', {
      revision: (await admin('/api/operations')).revision, name, enabled: true,
    })).id;
    const target = await group('authorized destination');
    const forbidden = await group('no account');
    await admin(`/api/accounts/${process.argv[5]}/policy`, {
      revision: (await admin('/api/operations')).revision,
      groupIds: [original, target], enabled: true, maxConcurrency: 2,
    });
    const move = async (groupId, dedicated = false) => {
      owner = await admin(`/api/api-keys/${owner.id}${dedicated ? '/group' : ''}`,
        { revision: owner.revision, groupId });
      // Exercise the parked-turn watchdog after the old access becomes stale.
      await new Promise(resolve => setTimeout(resolve, 750));
    };
    const http = async (history, secret = key) => {
      const response = await fetch(`${base}/v1/responses`, {
        method: 'POST', headers: { Authorization: `Bearer ${secret}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ model: 'gpt-6.1-sol', input: history, reasoning: { effort: 'low' },
          client_metadata: { thread_id: 'same-owned-http-history' } }),
      });
      assert.equal(response.ok, true, await response.clone().text());
      return response.json();
    };
    const records = async () => (await admin('/api/statistics')).records
      .filter(row => row.action === 'responses' && row.nativeThreadId)
      .sort((a, b) => a.startedAt - b.startedAt);
    const history = [{ role: 'user', content: 'concurrency-desktop' }];
    const firstHttp = await http(history);
    history.push(...firstHttp.output, { role: 'user', content: 'concurrency-desktop' });
    await move(target);
    await http(history);
    const foreign = await admin('/api/api-keys', { name: 'other owner', groupId: target });
    await http(history, foreign.secret);
    const httpRows = await records();
    assert.equal(httpRows.length, 3);
    assert.equal(httpRows[0].nativeThreadId, httpRows[1].nativeThreadId);
    assert.notEqual(httpRows[1].nativeThreadId, httpRows[2].nativeThreadId);
    assert.deepEqual(httpRows.map(row => row.groupId), [original, target, target]);

    const first = await create({ input: 'concurrency-tool-C' });
    const call = first.output.find(item => item.type === 'function_call');
    await move(original, true);
    const second = await create({ previous_response_id: first.id,
      input: [{ type: 'function_call_output', call_id: call.call_id, output: 'GROUP_MOVE_RESULT' }] });
    await move(target);
    const third = await create({ previous_response_id: second.id, input: 'concurrency-tool-C' });
    const wsRows = (await records()).slice(3);
    assert.equal(wsRows.length, 3);
    assert.equal(new Set(wsRows.map(row => row.nativeThreadId)).size, 1);
    assert.equal(wsRows[0].nativeTurnId, wsRows[1].nativeTurnId);
    assert.deepEqual(wsRows.map(row => row.groupId), [target, original, target]);
    await move(forbidden);
    await assert.rejects(create({ previous_response_id: third.id, input: 'concurrency-tool-C' }));
    assert.equal(socket.readyState, WebSocket.OPEN);
    await move(target);
    const parked = await create({ input: 'concurrency-tool-D' });
    assert(parked.output.some(item => item.type === 'function_call'));
    await admin(`/api/api-keys/${owner.id}`, { revision: owner.revision, state: 'revoked' });
    await new Promise(resolve => setTimeout(resolve, 750));
    const denied = await fetch(`${base}/v1/responses`, {
      method: 'POST', headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: 'gpt-6.1-sol', input: 'concurrency-desktop' }),
    });
    assert([401, 403].includes(denied.status));
    console.log(JSON.stringify({ httpReuse: true, websocketToolReuse: true, currentGroupAccounting: true,
      foreignKeyIsolated: true, unauthorizedAccountDenied: true, revokedKeyDenied: true }));
  } else if (mode === 'compact-direct') {
    const result = await create({ tools: [], input: [
      { role: 'user', content: 'SYNTHETIC_NO_DISK_COMPACT' }, { type: 'compaction_trigger' },
    ] });
    assert.equal(result.output[0].encrypted_content, 'SYNTHETIC_DIRECT_CIPHER');
    const continued = await create({ tools: [], previous_response_id: result.id, input: [
      { role: 'user', content: 'SYNTHETIC_NO_DISK_COMPACT_SECOND' }, { type: 'compaction_trigger' },
    ] });
    assert.equal(continued.output[0].encrypted_content, 'SYNTHETIC_DIRECT_CIPHER');
    console.log('Remote compaction and previous_response_id passed');
  } else if (mode === 'compaction') {
    const before = new Set((await (await fetch(`${base}/api/statistics`)).json()).records.map(row => row.id));
    let previous;
    for (let index = 0; index < 3; index++) {
      const result = await create({ input: `fixture-compact:websocket:${index}`, tools: [],
        ...(previous ? { previous_response_id: previous } : {}) });
      previous = result.id;
      assert.equal(result.usage.input_tokens_details.cached_tokens, 0);
    }
    const records = (await (await fetch(`${base}/api/statistics`)).json()).records.filter(row => !before.has(row.id));
    assert.equal(records.length, 3);
    assert.equal(new Set(records.map(row => row.nativeThreadId)).size, 1);
    console.log('Responses WebSocket native compaction continuation passed');
  } else if (mode === 'identity') {
    const metadata = (thread, parent) => ({ thread_id: thread, ...(parent ? { parent_thread_id: parent } : {}) });
    const before = new Set((await (await fetch(`${base}/api/statistics`)).json()).records.map(row => row.id));
    const first = await create({ input: 'concurrency-desktop', client_metadata: metadata('ws-root') });
    const second = await create({ previous_response_id: first.id, input: 'concurrency-desktop', client_metadata: metadata('ws-child', 'ws-root') });
    await create({ previous_response_id: second.id, input: 'concurrency-desktop', client_metadata: metadata('ws-child', 'ws-root') });
    const records = (await (await fetch(`${base}/api/statistics`)).json()).records
      .filter(row => !before.has(row.id) && row.action === 'responses').sort((a, b) => a.startedAt - b.startedAt);
    assert.equal(records.length, 3);
    assert.notEqual(records[0].nativeThreadId, records[1].nativeThreadId);
    assert.equal(records[1].nativeThreadId, records[2].nativeThreadId);
    assert.equal(records[0].accountId, records[1].accountId);
    console.log('Responses WebSocket frame identity and independent child thread passed');
  } else {
  const first = await create({ input: 'concurrency-tool-C',
    ...(mode === 'live-reset' ? { reasoning: { effort: 'high', summary: 'detailed' }, service_tier: 'priority' } : {}),
  });
  const call = first.output.find((item) => item.type === 'function_call');
  assert.equal(call.call_id, 'concurrency-tool-C');
  const second = await create({
    previous_response_id: first.id,
    input: [{ type: 'function_call_output', call_id: call.call_id, output: 'WS_CLIENT_RESULT' }],
    ...(mode === 'live-settings' ? { reasoning: { effort: 'high', summary: 'detailed' }, service_tier: 'priority' } : {}),
  });
  assert.match(JSON.stringify(second.output), /concurrency-tool-C-answer/);
  const third = await create({ previous_response_id: second.id, input: 'concurrency-tool-C',
    ...(mode === 'live-reset' ? {} : { reasoning: { effort: 'high' }, service_tier: 'priority' }) });
  assert.match(JSON.stringify(third.output), /concurrency-tool-C-answer/);
  assert.equal(new Set([first.id, second.id, third.id]).size, 3);
  for (const response of [first, second, third]) assert.equal(response.usage.input_tokens, 10);
  assert.equal(second.usage.input_tokens_details.cached_tokens, 8);
  console.log('Responses WebSocket tool continuation and next user message passed');
  }
} finally {
  socket.close();
}

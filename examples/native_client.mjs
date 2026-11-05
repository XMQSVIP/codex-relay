import WebSocket from 'ws';
const url = process.env.BRIDGE_NATIVE_URL ?? 'ws://127.0.0.1:4317/v1/native';
const key = process.env.BRIDGE_API_KEY;
const model = process.env.BRIDGE_MODEL;
if (!key || !model) throw new Error('Set BRIDGE_API_KEY and BRIDGE_MODEL');
const socket = new WebSocket(url, { headers: { Authorization: `Bearer ${key}` } });
const send = value => socket.send(JSON.stringify(value));
socket.on('open', () => send({ id: 1, method: 'thread/start', params: {
  model, dynamicTools: [{ type: 'function', name: 'client_time', description: 'Read the time on the caller computer.', inputSchema: { type: 'object', properties: {}, additionalProperties: false } }]
} }));
socket.on('message', raw => {
  const event = JSON.parse(raw.toString());
  if (event.id === 1 && event.result) send({ id: 2, method: 'turn/start', params: { input: [{ type: 'text', text: '调用 client_time，告诉我客户端的当前时间。', text_elements: [] }] } });
  if (event.method === 'item/tool/call') {
    if (event.params.tool !== 'client_time' || Object.keys(event.params.arguments).length) { socket.close(); throw new Error('Unexpected client tool'); }
    const value = new Date().toISOString();
    console.log('Executed on this client:', value);
    send({ id: event.id, result: { success: true, contentItems: [{ type: 'inputText', text: value }] } });
  }
  if (event.method === 'item/agentMessage/delta') process.stdout.write(event.params.delta);
  if (event.method === 'item/completed' && event.params.item.type === 'agentMessage') console.log('\n', event.params.item.text);
  if (event.method === 'turn/completed') send({ id: 3, method: 'session/close' });
  if (event.method === 'bridge/error' || event.error) { console.error(event); socket.close(); }
});
socket.on('error', error => { console.error(error.message); process.exitCode = 1; });

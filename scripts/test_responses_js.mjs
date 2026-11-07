// Run by test_responses_sdk.py against its isolated synthetic relay.
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
const { default: OpenAI } = await import(pathToFileURL(process.env.BRIDGE_TEST_OPENAI_JS));
const client = new OpenAI({ baseURL: process.env.BRIDGE_TEST_URL, apiKey: process.env.BRIDGE_TEST_KEY, maxRetries: 0 });
const response = await client.responses.create({ model: 'gpt-6.1-sol', input: 'JavaScript direct API', store: false });
assert.equal(response.output_text, 'SDK verified');
const stream = await client.responses.create({ model: 'gpt-6.1-sol', input: 'JavaScript SSE', stream: true, store: false });
let text = '', completed;
for await (const event of stream) {
  if (event.type === 'response.output_text.delta') text += event.delta;
  if (event.type === 'response.completed') completed = event.response;
}
assert.equal(text, 'SDK verified');
assert.equal(completed.status, 'completed');
console.log('Official JavaScript SDK JSON and SSE passed');

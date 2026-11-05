import { test } from 'node:test';
import assert from 'node:assert/strict';
import { guardedProviderOverrides, lanBaseUrl, selectLanProvider } from '../src/shared/provider.js';

const nativeConfig = () => ({ config: {
  model_provider: 'OpenAI', model: 'configured-model', mcp_servers: { local: {} },
  model_providers: { OpenAI: { base_url: 'http://192.168.1.10:48760/v1', wire_api: 'responses', requires_openai_auth: false,
    experimental_bearer_token: 'PRIVATE_FIXTURE_BEARER', http_headers: { 'X-OpenAI-Actor-Authorization': 'PRIVATE_FIXTURE_ACTOR' },
  } },
} });

test('effective provider metadata and per-thread overrides never contain credential values', () => {
  const config = nativeConfig();
  const original = structuredClone(config);
  const provider = selectLanProvider(config);
  assert.equal(provider.id, 'OpenAI'); assert.equal(provider.model, 'configured-model');
  assert.deepEqual(provider.requiredHeaders, ['x-openai-actor-authorization', 'authorization']);
  assert.equal(provider.auth, 'native-bearer');
  const overrides = guardedProviderOverrides(provider, 'http://127.0.0.1:3000/capability/v1');
  assert.equal(overrides['model_providers.OpenAI.base_url'], 'http://127.0.0.1:3000/capability/v1');
  assert.equal(JSON.stringify({ provider, overrides }).includes('PRIVATE_FIXTURE'), false);
  assert.deepEqual(config, original);
  assert.equal(Object.keys(overrides).some(key => /bearer|auth|headers/.test(key)), false);
});

test('LAN selection rejects public, URL credentials, query, builtin provider and unsupported auth', () => {
  for (const url of ['https://example.com/v1', 'http://127.0.0.1/v1', 'http://169.254.169.254/v1', 'http://192.168.1.1/v1?key=secret', 'http://secret@192.168.1.1/v1', 'http://192.168.1.1/not-v1']) {
    assert.throws(() => lanBaseUrl(url), { code: 'PROVIDER_ENDPOINT' });
  }
  assert.equal(lanBaseUrl('http://10.1.2.3/v1/'), 'http://10.1.2.3/v1');
  assert.throws(() => selectLanProvider({ config: { ...nativeConfig().config, model_provider: 'openai' } }), { code: 'PROVIDER_RESERVED' });
  const config = nativeConfig(); config.config.model_providers.OpenAI.requires_openai_auth = true;
  assert.throws(() => selectLanProvider(config), { code: 'PROVIDER_AUTH_UNSUPPORTED' });
  config.config.model_providers.OpenAI.requires_openai_auth = false;
  Object.assign(config.config.model_providers.OpenAI.http_headers, { Host: 'evil.example' });
  assert.throws(() => selectLanProvider(config), { code: 'PROVIDER_HEADERS' });
});

test('native environment auth is resolved by presence only and missing variables fail closed', () => {
  const config = nativeConfig(); Object.assign(config.config.model_providers.OpenAI, { experimental_bearer_token: null, env_key: 'LAN_TEST_KEY', env_http_headers: { 'X-Test-Auth': 'LAN_HEADER_KEY' } });
  assert.throws(() => selectLanProvider(config, {}), { code: 'PROVIDER_AUTH_MISSING' });
  const provider = selectLanProvider(config, { LAN_TEST_KEY: 'PRIVATE_FIXTURE_KEY', LAN_HEADER_KEY: 'PRIVATE_FIXTURE_HEADER' });
  assert.equal(provider.forwardHeaders.includes('x-test-auth'), true);
  assert.equal(JSON.stringify(provider).includes('PRIVATE_FIXTURE'), false);
});

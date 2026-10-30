import { z } from 'zod';
import { BridgeError } from '../errors.js';
import type { AppServerRpc } from '../transport/rpc.js';
import { cleanChildEnvironment } from './backend.js';

export interface LanProvider {
  id: string;
  model: string;
  baseUrl: string;
  auth: 'native-bearer' | 'native-headers' | 'none';
  forwardHeaders: string[];
  requiredHeaders: string[];
  mcpServers: string[];
}

const record = z.record(z.string(), z.unknown());
const headers = z.record(z.string(), z.string()).nullish();
const providerShape = z.object({
  base_url: z.string(), wire_api: z.literal('responses'), requires_openai_auth: z.boolean().nullish(),
  env_key: z.string().nullish(), experimental_bearer_token: z.string().nullish(),
  http_headers: headers, env_http_headers: headers, query_params: headers,
  auth: z.unknown().optional(), gateway_oauth: z.unknown().optional(), aws: z.unknown().optional(),
});

/** Numeric private IPv4 only: an administrator-selected LAN endpoint, never caller-provided DNS. */
export function lanBaseUrl(value: string): string {
  let url: URL;
  try { url = new URL(value); } catch { throw new BridgeError('PROVIDER_ENDPOINT', 'Invalid configured LAN endpoint.'); }
  const parts = url.hostname.split('.').map(Number);
  const privateAddress = parts.length === 4 && parts.every(p => Number.isInteger(p) && p >= 0 && p <= 255)
    && (parts[0] === 10 || (parts[0] === 192 && parts[1] === 168) || (parts[0] === 172 && parts[1]! >= 16 && parts[1]! <= 31));
  if (!['http:', 'https:'].includes(url.protocol) || !privateAddress || url.username || url.password || url.search || url.hash
      || !['/v1', '/v1/'].includes(url.pathname)) throw new BridgeError('PROVIDER_ENDPOINT', 'The selected provider must use a fixed private IPv4 /v1 endpoint without URL credentials or query parameters.');
  url.pathname = '/v1';
  return url.href;
}

export function validateForwardHeaders(names: string[]): string[] {
  const result = [...new Set(names.map(name => name.toLowerCase()))];
  if (result.some(name => !/^[a-z0-9!#$%&'*+.^_`|~-]{1,128}$/.test(name)
      || /^(host|connection|keep-alive|te|trailer|transfer-encoding|upgrade|proxy-.*|content-.*|accept.*|origin|referer|cookie|set-cookie|chatgpt-account-id|forwarded|x-forwarded-.*|sec-.*)$/i.test(name))) {
    throw new BridgeError('PROVIDER_HEADERS', 'The provider contains an unsupported transport or browser header.');
  }
  return result;
}

/** Consume effective config in memory. Return only routing metadata, never credentials or raw config. */
export function selectLanProvider(value: unknown, env: NodeJS.ProcessEnv = process.env): LanProvider {
  const result = z.object({ config: z.object({
    model_provider: z.string().regex(/^[A-Za-z0-9_-]{1,128}$/), model: z.string().min(1).max(256),
    model_providers: z.record(z.string(), record), mcp_servers: z.record(z.string(), z.unknown()).nullish(),
  }) }).safeParse(value);
  if (!result.success) throw new BridgeError('PROVIDER_CONFIG', 'Effective provider configuration is incomplete or unsupported.');
  const config = result.data.config;
  if (['openai', 'ollama', 'lmstudio'].includes(config.model_provider)) throw new BridgeError('PROVIDER_RESERVED', 'A custom LAN provider is required for per-thread routing.');
  const parsed = providerShape.safeParse(config.model_providers[config.model_provider]);
  if (!parsed.success) throw new BridgeError('PROVIDER_CONFIG', 'The selected provider is not a supported Responses provider.');
  const provider = parsed.data;
  if (provider.requires_openai_auth || provider.auth || provider.gateway_oauth || provider.aws || Object.keys(provider.query_params ?? {}).length) {
    throw new BridgeError('PROVIDER_AUTH_UNSUPPORTED', 'This LAN adapter supports native static/environment bearer tokens and configured headers only.');
  }
  const readEnv = (name: string) => Object.entries(env).find(([key]) => key.toUpperCase() === name.toUpperCase())?.[1];
  if (provider.env_key && !readEnv(provider.env_key)) throw new BridgeError('PROVIDER_AUTH_MISSING', 'The selected provider credential environment variable is missing.');
  const hasBearer = !!(provider.env_key || provider.experimental_bearer_token);
  const staticHeaders = Object.entries(provider.http_headers ?? {}).filter(([, value]) => !!value).map(([key]) => key);
  const envHeaders = Object.entries(provider.env_http_headers ?? {}).filter(([, name]) => !!readEnv(name)).map(([key]) => key);
  const forwardHeaders = validateForwardHeaders([...staticHeaders, ...envHeaders, ...(hasBearer ? ['authorization'] : [])]);
  const baseUrl = lanBaseUrl(provider.base_url);
  return { id: config.model_provider, model: config.model, baseUrl,
    auth: hasBearer ? 'native-bearer' : forwardHeaders.length ? 'native-headers' : 'none',
    forwardHeaders, requiredHeaders: [...forwardHeaders], mcpServers: Object.keys(config.mcp_servers ?? {}),
  };
}

export async function readLanProvider(rpc: AppServerRpc, cwd: string): Promise<LanProvider> {
  return selectLanProvider(await rpc.request('config/read', { includeLayers: false, cwd }), cleanChildEnvironment());
}

/** Only non-secret routing fields are overridden. Codex resolves the original provider's credentials. */
export function guardedProviderOverrides(provider: LanProvider, routeBaseUrl: string): Record<string, unknown> {
  const prefix = `model_providers.${provider.id}`;
  return {
    [`${prefix}.base_url`]: routeBaseUrl,
    [`${prefix}.supports_websockets`]: false,
    [`${prefix}.request_max_retries`]: 0,
    [`${prefix}.stream_max_retries`]: 0,
    'features.enable_request_compression': false,
  };
}

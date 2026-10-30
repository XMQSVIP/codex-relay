import { accountGroupIds } from './accountMembership';
import type { ClientKey, Operations } from './api';

export interface CodexConfigInput {
  secret: string;
  baseUrl: string;
  model: string;
  websocketEnabled: boolean;
}

export const CODEX_DEFAULT_MODEL = 'gpt-6.1-sol';

// JSON string escapes are also TOML basic-string escapes; TOML additionally forbids literal DEL.
const tomlString = (value: string) => JSON.stringify(value).replace(/\u007f/g, '\\u007f');

export function buildCodexConfig({ secret, baseUrl, model, websocketEnabled }: CodexConfigInput): string {
  return `model_provider = "OpenAI"
model = ${tomlString(model)}
review_model = ${tomlString(model)}
model_reasoning_effort = "max"
service_tier = "default"

[model_providers.OpenAI]
name = "OpenAI"
base_url = ${tomlString(baseUrl.replace(/\/+$/, ''))}
wire_api = "responses"
supports_websockets = ${websocketEnabled}
requires_openai_auth = false
# 代理密钥仅用于网关鉴权，真实账号登录状态由服务端管理
experimental_bearer_token = ${tomlString(secret)}

[model_providers.OpenAI.capabilities]
# Codex 0.162.0+ 显式启用 V2 远程压缩，不依赖 Provider 名称识别
remote_compaction = "v2"

[model_providers.OpenAI.http_headers]
# 声明服务端托管认证，让官方客户端启用原生生图，该标记不是密钥
X-OpenAI-Actor-Authorization = "proxy-managed"

[features]
image_generation = true
goals = true
`;
}

export const validCodexModel = (model: string) => /^[a-zA-Z0-9._:/-]{1,128}$/.test(model);

export function keyModelChoices(key: ClientKey, operations: Operations) {
  const groupEnabled = operations.groups.some(group => group.id === key.groupId && group.enabled);
  const routeModels = groupEnabled ? [...new Set(operations.accounts
    .filter(account => accountGroupIds(account).includes(key.groupId) && account.enabled && !account.deleting)
    .flatMap(account => account.route ? [account.route.model] : []).filter(validCodexModel))] : [];
  const choices = key.models.length ? [...new Set(key.models)] : [...new Set([CODEX_DEFAULT_MODEL, ...routeModels])];
  const preferred = choices.includes(CODEX_DEFAULT_MODEL) ? CODEX_DEFAULT_MODEL : routeModels.find(model => choices.includes(model)) ?? choices[0] ?? '';
  return { choices, preferred };
}

export function keyUsageUnavailable(key: ClientKey | undefined, now = Date.now()): string {
  if (!key || key.state === 'revoked') return '密钥已删除或撤销，请选择其他密钥。';
  if (key.state === 'expired' || (key.expiresAt !== null && key.expiresAt <= now)) return '密钥已过期，请创建新密钥。';
  if (key.state === 'paused') return '密钥已暂停，请先启用密钥。';
  if (!key.secret) return '未取得完整密钥，无法生成配置，请刷新密钥列表。';
  return '';
}

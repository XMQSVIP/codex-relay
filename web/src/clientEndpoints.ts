import type { ClientKeys } from './api';

export function clientEndpoints(data: ClientKeys | null, allowPublicAccess: boolean, origin: string) {
  const consoleUrl = `${origin}/v1`;
  const choices = new Map<string, string>();
  for (const endpoint of data?.networkEndpoints ?? []) choices.set(endpoint.baseUrl, endpoint.transport === 'reverse-proxy' ? 'HTTPS · 反向代理' : endpoint.transport.toUpperCase());
  for (const url of data?.lanBaseUrls ?? []) if (!choices.has(url)) choices.set(url, allowPublicAccess ? '局域网' : '局域网（需开启外部访问）');
  if (data && allowPublicAccess && !choices.has(consoleUrl)) choices.set(consoleUrl, '当前后台地址');
  if (data) choices.set(data.localBaseUrl, '本机');
  const preferred = !data ? '' : allowPublicAccess ? data.networkEndpoints[0]?.baseUrl ?? data.lanBaseUrls?.[0] ?? consoleUrl : data.localBaseUrl;
  return { choices, preferred };
}

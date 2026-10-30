import type { AccountBackup, Group } from './api';

export const accountBackupMaxBytes = 4 * 1024 * 1024;
export type ImportAccount = {
  name: string;
  source: 'isolated' | 'native-config' | 'api-key';
  document?: Record<string, unknown>;
  upstream?: NonNullable<AccountBackup['items'][number]['upstream']>;
  groupIds: string[];
  maxConcurrency: number;
};
export type BackupPreview = {
  items: ImportAccount[];
};
const object = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === 'object' && !Array.isArray(value);
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// Only import fields accepted by the server. Backup descriptions and reconnect
// hints must never be forwarded to its deny_unknown_fields request structs.
export function parseAccountBackup(text: string): BackupPreview {
  if (new Blob([text]).size > accountBackupMaxBytes) throw new Error('账户备份不能超过 4 MiB。');
  let value: unknown;
  try {
    value = JSON.parse(text.replace(/^\uFEFF/, ''));
  } catch {
    throw new Error('JSON 格式无效，请检查账户备份内容。');
  }
  if (
    !object(value) ||
    value.format !== 'codex-relay-accounts' ||
    value.version !== 1 ||
    !Array.isArray(value.items)
  ) {
    throw new Error('请选择版本为 1 的账户备份。单账户凭据请通过“添加账户 → 导入凭据”导入。');
  }
  if (!value.items.length || value.items.length > 32) throw new Error('每批支持 1–32 个账户。');
  const items: ImportAccount[] = [];
  for (const [index, item] of value.items.entries()) {
    const invalid = () => new Error(`第 ${index + 1} 项的名称、来源、分组或并发配置无效。`);
    if (
      !object(item) ||
      typeof item.name !== 'string' ||
      !item.name.trim() ||
      [...item.name].length > 60 ||
      /[\u0000-\u001f\u007f-\u009f]/u.test(item.name)
    )
      throw invalid();
    if (item.source !== 'isolated' && item.source !== 'native-config' && item.source !== 'api-key')
      throw invalid();
    if (
      !Number.isInteger(item.maxConcurrency) ||
      Number(item.maxConcurrency) < 1 ||
      Number(item.maxConcurrency) > 256
    )
      throw invalid();
    if ('groupId' in item) throw invalid();
    const groupIds = item.groupIds;
    if (
      !Array.isArray(groupIds) ||
      !groupIds.length ||
      groupIds.length > 64 ||
      !groupIds.every((id) => typeof id === 'string' && uuid.test(id))
    )
      throw invalid();
    const document = item.document;
    if (document != null && (!object(document) || item.source !== 'isolated')) throw invalid();
    if (document && new Blob([JSON.stringify(document)]).size > 256 * 1024)
      throw new Error(`第 ${index + 1} 项的凭据超过 256 KiB。`);
    let upstream: ImportAccount['upstream'];
    if (item.source === 'api-key') {
      if (
        !object(item.upstream) ||
        typeof item.upstream.baseUrl !== 'string' ||
        typeof item.upstream.apiKey !== 'string' ||
        !item.upstream.apiKey.trim() ||
        item.upstream.apiKey.length > 8192 ||
        /[\u0000-\u001f\u007f]/.test(item.upstream.apiKey) ||
        (item.upstream.transport !== 'http-sse' && item.upstream.transport !== 'ws-first')
      )
        throw invalid();
      try {
        const url = new URL(item.upstream.baseUrl);
        if (
          !['http:', 'https:'].includes(url.protocol) ||
          url.username ||
          url.password ||
          url.search ||
          url.hash
        )
          throw invalid();
      } catch {
        throw invalid();
      }
      upstream = {
        baseUrl: item.upstream.baseUrl,
        apiKey: item.upstream.apiKey,
        transport: item.upstream.transport as 'http-sse' | 'ws-first',
      };
    } else if (item.upstream != null) throw invalid();
    items.push({
      name: item.name,
      source: item.source,
      maxConcurrency: Number(item.maxConcurrency),
      groupIds: [...new Set(groupIds as string[])],
      ...(object(document) ? { document } : {}),
      ...(upstream ? { upstream } : {}),
    });
  }
  return { items };
}

export function missingBackupGroups(items: ImportAccount[], groups: Group[]): boolean {
  return items.some((item) => item.groupIds.some((id) => !groups.some((group) => group.id === id)));
}

export function downloadAccountJson(value: unknown, filename: string) {
  const url = URL.createObjectURL(
    new Blob([JSON.stringify(value, null, 2)], { type: 'application/json' }),
  );
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

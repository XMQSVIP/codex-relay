import type { Page } from '@playwright/test';
import type { Account, AccountPolicy, ClientKey, NativeCapabilities, Overview, RequestRecord, Statistics, UsageRanking } from '../src/api';

export const nativeCapabilities = (): NativeCapabilities => ({
  installation: 'fixture', provider: 'fixture', checkedAt: Date.now(), stale: false,
  models: [{ id: 'test-model', model: 'test-model', inputModalities: ['text'], supportedReasoningEfforts: [{ reasoningEffort: 'high', description: 'High' }], serviceTiers: [{ id: 'ultrafast', name: 'Ultrafast', description: 'Synthetic tier' }], defaultServiceTier: null }],
  webSearch: null, imageGeneration: null, errorCode: null,
});

export const groupId = '00000000-0000-0000-0000-000000000000';
export const nativeId = '10000000-0000-0000-0000-000000000001';
export const oauthId = '10000000-0000-0000-0000-000000000002';
export function overview(count = 3): Overview {
  const accounts: Account[] = Array.from({ length: count }, (_, i) => ({
    id: `10000000-0000-0000-0000-${String(i + 1).padStart(12, '0')}`, name: i === 0 ? '本机 Codex' : i === 1 ? '独立测试账户' : `生产账户 ${i + 1} / 长名称回归验证`,
    source: i === 0 ? 'native-config' : 'isolated', credentialSource: i === 0 ? null : 'managed', createdAt: Date.now(), state: 'stopped', pid: null,
    account: i === 0 ? null : { type: 'chatgpt', email: `operator-${i}@example.test`, planType: 'plus' },
  }));
  const policies: AccountPolicy[] = accounts.map(a => ({ id: a.id, name: a.name, groupIds: [groupId], enabled: true, maxConcurrency: 5, active: 0, queued: 0, route: { id: 'openai', model: 'test-model', baseUrl: 'https://model.example.test', auth: 'chatgpt' } }));
  return {
    implementation: 'rust', version: '0.2.0', inferenceEnabled: !!count, busy: false,
    automaticProxy: { state: 'direct', http: null, https: null },
    installation: { manuallyConfigured: false, desktopExecutable: 'C:\\Program Files\\Codex\\Codex.exe', archive: 'C:\\Program Files\\Codex\\resources\\app.asar', appServerVersion: 'fixture-version', appServerExecutable: 'C:\\Program Files\\Codex\\resources\\codex.exe', appServerSha256: 'fixture' },
    desktop: { state: 'stopped' }, accounts, jobs: [], reports: {},
    operations: { revision: 1, globalLimit: count * 5, globalLimitAuto: true, manualGlobalLimit: 5, accountCapacity: count * 5, active: 0, queued: 1, queueLimit: 64, queueTimeoutSeconds: 30, nativeAffinityTtlHours: 24, storageHealthy: true, draining: false, retentionDays: 30, groups: [{ id: groupId, name: '默认分组', enabled: true, active: 0, queued: 1, memberCount: count }, { id: '20000000-0000-0000-0000-000000000001', name: '开发测试', enabled: true, active: 0, queued: 0, memberCount: 0 }], accounts: policies },
  };
}

function usageRankings(records: RequestRecord[], dimension: 'account' | 'key' | 'model'): UsageRanking[] {
  const rows = new Map<string, UsageRanking>();
  for (const record of [...records].sort((a, b) => a.startedAt - b.startedAt)) {
    if (dimension === 'model' && record.kind !== 'inference') continue;
    const id = dimension === 'model' ? (record.model ?? '') : dimension === 'account' ? record.accountId : record.keyId;
    if (id == null) continue;
    const name = dimension === 'model' ? record.model : dimension === 'account' ? record.accountName : record.keyName;
    const row = rows.get(id) ?? { id, name: null, requestCount: 0, inputTokens: null, outputTokens: null, totalTokens: null, usageKnownRequests: 0,
      failedRequests: 0, averageFirstResponseMs: null, averageModelFirstResponseMs: null, averageTotalMs: null, cachedInputTokens: null, cacheKnownInputTokens: null, cacheKnownRequests: 0, cacheHitRate: null };
    if (name != null) row.name = name;
    row.requestCount++;
    if (record.status === 'failed') row.failedRequests++;
    if (record.kind === 'inference' && record.cachedInputTokens != null) {
      row.cachedInputTokens = (row.cachedInputTokens ?? 0) + record.cachedInputTokens;
      if (record.inputTokens != null) row.cacheKnownInputTokens = (row.cacheKnownInputTokens ?? 0) + record.inputTokens;
      row.cacheKnownRequests++;
    }
    if (record.inputTokens != null) row.inputTokens = (row.inputTokens ?? 0) + record.inputTokens;
    if (record.outputTokens != null) row.outputTokens = (row.outputTokens ?? 0) + record.outputTokens;
    if (record.inputTokens != null && record.outputTokens != null) {
      row.totalTokens = (row.totalTokens ?? 0) + record.inputTokens + record.outputTokens;
      row.usageKnownRequests++;
    }
    rows.set(id, row);
  }
  return [...rows.values()].map(row => {
    const samples = records.filter(record => record.kind === 'inference' && (dimension === 'model' ? (record.model ?? '') : dimension === 'account' ? record.accountId : record.keyId) === row.id);
    const first = samples.filter(record => record.firstDeltaMs != null).map(record => record.firstDeltaMs! + (record.waitMs ?? 0));
    const modelFirst = samples.filter(record => record.firstDeltaMs != null).map(record => record.firstDeltaMs!);
    const total = samples.filter(record => record.totalMs != null).map(record => record.totalMs!);
    row.averageFirstResponseMs = first.length ? first.reduce((sum, value) => sum + value, 0) / first.length : null;
    row.averageModelFirstResponseMs = modelFirst.length ? modelFirst.reduce((sum, value) => sum + value, 0) / modelFirst.length : null;
    row.averageTotalMs = total.length ? total.reduce((sum, value) => sum + value, 0) / total.length : null;
    row.cacheHitRate = row.cachedInputTokens != null && row.cacheKnownInputTokens ? row.cachedInputTokens / row.cacheKnownInputTokens : null;
    return row;
  }).sort((a, b) => (b.totalTokens ?? -1) - (a.totalTokens ?? -1) || a.id.localeCompare(b.id));
}

function statisticsSummary(records: RequestRecord[]): Statistics['summary'] {
  const models = records.filter(r => r.kind === 'inference');
  const known = models.filter(r => r.inputTokens != null && r.outputTokens != null);
  const cacheKnown = models.filter(r => r.cachedInputTokens != null);
  const sum = (items: RequestRecord[], field: 'inputTokens' | 'outputTokens' | 'cachedInputTokens') =>
    items.some(r => r[field] != null) ? items.reduce((n, r) => n + (r[field] ?? 0), 0) : null;
  const average = (values: (number | null | undefined)[]) => {
    const known = values.filter((v): v is number => v != null);
    return known.length ? known.reduce((n, v) => n + v, 0) / known.length : null;
  };
  const cachedInputTokens = sum(cacheKnown, 'cachedInputTokens'), cacheKnownInputTokens = sum(cacheKnown, 'inputTokens');
  const count = (status: string) => records.filter(r => r.status === status).length;
  return { total: records.length, succeeded: count('succeeded'), failed: count('failed'), rejected: count('rejected'),
    cancelled: count('cancelled') + count('interrupted'), running: count('running'), queued: count('queued'), timedOut: count('timed_out'),
    queueFull: records.filter(r => r.errorCode === 'QUEUE_FULL').length, averageWaitMs: average(records.map(r => r.waitMs)),
    averageMs: average(records.filter(r => ['succeeded', 'failed'].includes(r.status)).map(r => r.durationMs)),
    averageFirstResponseMs: average(models.map(r => r.firstDeltaMs == null ? null : r.firstDeltaMs + (r.waitMs ?? 0))),
    averageModelFirstResponseMs: average(models.map(r => r.firstDeltaMs)),
    averageTotalMs: average(models.map(r => r.totalMs)), inferenceRequests: models.length,
    inferenceSucceeded: models.filter(r => r.status === 'succeeded').length, inferenceFailed: models.filter(r => r.status === 'failed').length,
    inputTokens: sum(models, 'inputTokens'), outputTokens: sum(models, 'outputTokens'), usageKnownRequests: known.length,
    totalTokens: known.length ? known.reduce((n, r) => n + r.inputTokens! + r.outputTokens!, 0) : null,
    cachedInputTokens, cacheKnownInputTokens, cacheKnownRequests: cacheKnown.length,
    cacheHitRate: cachedInputTokens != null && cacheKnownInputTokens ? cachedInputTokens / cacheKnownInputTokens : null };
}

export async function mockConsole(page: Page, options: { count?: number; emptyRecords?: boolean; emptyKeys?: boolean; longNames?: boolean } = {}) {
  const logins = new Map<string, unknown>();
  const data = overview(options.count ?? 3);
  if (options.longNames) data.accounts[0].name = '超长账户名称'.repeat(8);
  const key: ClientKey = { id: '30000000-0000-0000-0000-000000000001', name: '开发客户端', prefix: 'cr_fixture', secret: 'cr_test_secret_only_for_fixture', groupId, models: [], state: 'active', createdAt: Date.now(), expiresAt: null, lastAuthenticatedAt: null, revision: 1, maxConcurrency: 0 };
  const keys = options.emptyKeys ? [] : [key];
  const resetAutomation = new Map<string, { enabled: boolean; revision: number; pendingRequest: null }>();
  const records: RequestRecord[] = options.emptyRecords ? [] : [{ id: '40000000-0000-0000-0000-000000000001', startedAt: Date.now(), kind: 'inference', action: 'responses', accountId: nativeId, accountName: '本机 Codex', groupId, groupName: '默认分组', status: 'succeeded', durationMs: 1500, totalMs: 1500, waitMs: 50, errorCode: null, responseServiceTier: null, endpoint: '/v1/responses', clientTransport: 'sse', upstreamTransport: 'native', model: 'test-model', inputTokens: 100, outputTokens: 25, cachedInputTokens: 50, firstResponseMs: 250, firstDeltaMs: 200, streaming: true, keyId: key.id, keyName: key.name, reasoningEffort: 'default', userAgent: 'fixture-sdk/1.0' }];
  const state = { debug: { enabled: false, revision: 0 }, data, keys, records, queue: true, writes: [] as { path: string; body: any }[], offline: false, overviewCalls: 0, statisticsQueries: [] as URLSearchParams[], expired: false, conflict: false };
  await page.route('**/api/**', async route => {
    const request = route.request(), url = new URL(request.url()), path = url.pathname.slice(4), method = request.method();
    const body = request.postDataJSON();
    const reply = (json: unknown, status = 200) => {
      data.operations.accountCapacity = data.operations.accounts.reduce((sum, account) => sum + account.maxConcurrency, 0);
      data.operations.globalLimit = data.operations.globalLimitAuto ? data.operations.accountCapacity : data.operations.manualGlobalLimit;
      return route.fulfill({ json, status });
    };
    if (path === '/session') return reply({ requiresToken: false, passwordEnabled: state.expired, allowPublicAccess: true, authenticated: !state.expired });
    if (path === '/session/login') { state.expired = false; return reply({ ok: true }); }
    if (state.expired) return reply({ error: { code: 'ADMIN_LOGIN_REQUIRED', message: '请重新登录管理后台。' } }, 401);
    if (path === '/overview') { state.overviewCalls++; if (state.offline) return route.abort('failed'); return reply(data); }
    if (path.endsWith('/quota')) return reply({ status: 'unknown', stale: true, blocked: false, windows: [] });
    if (path === '/accounts/metrics') {
      const from = Number(url.searchParams.get('from')), to = Number(url.searchParams.get('to')), id = url.searchParams.get('accountId');
      const inference = records.filter(r => r.kind === 'inference' && (!id || r.accountId === id));
      const list = inference.filter(r => r.startedAt >= from && r.startedAt <= to);
      const aggregate = (items: RequestRecord[], id: string | null) => {
        const succeeded = items.filter(r => r.status === 'succeeded').length, failed = items.filter(r => r.status === 'failed').length;
        const sum = (key: 'inputTokens' | 'outputTokens' | 'cachedInputTokens') => items.some(r => r[key] != null) ? items.reduce((n, r) => n + (r[key] ?? 0), 0) : null;
        const known = items.filter(r => r.inputTokens != null && r.outputTokens != null);
        return { id, requestCount: items.length, succeeded, failed, successRate: succeeded + failed ? succeeded / (succeeded + failed) : null, inputTokens: sum('inputTokens'), outputTokens: sum('outputTokens'), cachedInputTokens: sum('cachedInputTokens'), totalTokens: known.length ? known.reduce((n, r) => n + r.inputTokens! + r.outputTokens!, 0) : null, usageKnownRequests: known.length };
      };
      const accounts = [...new Set(inference.map(r => r.accountId))].map(id => ({ ...aggregate(list.filter(r => r.accountId === id), id), lastUsedAt: Math.max(...inference.filter(r => r.accountId === id).map(r => r.startedAt)) }));
      const models = id ? [...new Set(list.map(r => r.model ?? null))].map(id => aggregate(list.filter(r => (r.model ?? null) === id), id)) : [];
      return reply({ from, to, updatedAt: Date.now(), accounts, models });
    }
    if (method !== 'GET') state.writes.push({ path, body });
    if (path.endsWith('/quota/reset/automation')) {
      const current = resetAutomation.get(path) ?? { enabled: false, revision: 1, pendingRequest: null };
      if (method === 'POST') { current.enabled = body.enabled; current.revision++; }
      resetAutomation.set(path, current);
      return reply(current);
    }
    if (path === '/api-keys' && method === 'GET') return reply({ keys, baseUrl: 'http://127.0.0.1:4317/v1', localBaseUrl: 'http://127.0.0.1:4317/v1', networkEndpoints: [], inferenceEnabled: !!data.accounts.length });
    if (path === '/api-keys' && method === 'POST') { const created = { ...key, ...body, id: crypto.randomUUID() }; keys.unshift(created); return reply({ key: created, secret: created.secret }, 201); }
    if (path.startsWith('/api-keys/')) { const item = keys.find(item => path.endsWith(item.id)); if (!item) return reply({}, 404); if (method === 'DELETE') keys.splice(keys.indexOf(item), 1); else Object.assign(item, body, { revision: item.revision + 1 }); return reply(item); }
    if (path === '/statistics') {
      state.statisticsQueries.push(url.searchParams);
      const from = Number(url.searchParams.get('from') ?? Date.now() - 86400000), to = Number(url.searchParams.get('to') ?? Date.now());
      const list = records.filter(record => record.startedAt >= from && record.startedAt <= to && (['keyName', 'accountName', 'groupName'] as const).every(field => !url.searchParams.get(field) || record[field]?.includes(url.searchParams.get(field)!)) && (['groupId', 'accountId', 'kind', 'status', 'keyId', 'model', 'errorCode'] as const).every(field => !url.searchParams.get(field) || record[field] === url.searchParams.get(field)));
      const summary = statisticsSummary(list);
      const pageSize = Number(url.searchParams.get('pageSize') ?? 20);
      const pageNumber = Math.min(Number(url.searchParams.get('page') ?? 1), Math.max(1, Math.ceil(list.length / pageSize)));
      const bucketMs = Math.ceil((to - from + 1) / 24);
      const trend = Array.from({ length: 24 }, (_, i) => {
        const items = list.filter(r => Math.min(23, Math.floor((r.startedAt - from) / bucketMs)) === i);
        const bucket = statisticsSummary(items);
        return { startedAt: Math.min(to, from + bucketMs * i), count: items.length, succeeded: bucket.succeeded, rejected: bucket.rejected,
          inferenceRequests: bucket.inferenceRequests, inputTokens: bucket.inputTokens, outputTokens: bucket.outputTokens,
          totalTokens: bucket.totalTokens, usageKnownRequests: bucket.usageKnownRequests,
          cachedInputTokens: bucket.cachedInputTokens, cacheKnownInputTokens: bucket.cacheKnownInputTokens,
          cacheKnownRequests: bucket.cacheKnownRequests, cacheHitRate: bucket.cacheHitRate };
      });
      return reply({ from, to, hours: (to - from + 1) / 3600000, page: pageNumber, pageSize, summary, trend, records: list.slice((pageNumber - 1) * pageSize, pageNumber * pageSize), accountUsage: usageRankings(list, 'account'), keyUsage: usageRankings(list, 'key'), modelUsage: usageRankings(list, 'model') });
    }
    if (path === '/queue') return reply({ page: 1, pageSize: 50, total: state.queue ? 1 : 0, totalQueued: state.queue ? 1 : 0, records: state.queue ? [{ id: 'queue-fixture', position: 1, startedAt: Date.now(), kind: 'inference', action: 'responses', accountId: null, accountName: null, groupId, groupName: '默认分组', waitMs: 1000, remainingMs: 29000 }] : [] });
    if (path.endsWith('/cancel')) { state.queue = false; data.operations.queued = 0; return reply({ ok: true }); }
    if (path === '/local-connection/inspect') return reply({ id: 'inspection-fixture', provider: 'openai', auth: 'chatgpt', model: 'test-model', endpoint: null });
    if (path === '/local-connection/connect') { const account: Account = { id: body.requestId, name: body.name, source: 'native-config', createdAt: Date.now(), state: 'stopped', pid: null, account: null }; data.accounts.push(account); data.operations.accounts.push({ ...data.operations.accounts[0], id: account.id, name: account.name, groupIds: [body.groupId] }); return reply({ status: 'enabled', account }); }
    if (path === '/accounts' && method === 'POST') { const account: Account = { id: crypto.randomUUID(), name: body.name, source: body.source === 'api-key' ? 'api-key' : 'isolated', ...(body.source === 'api-key' ? { credentialSource: 'api-key', upstream: { baseUrl: body.upstream.baseUrl, transport: body.upstream.transport, hasApiKey: true } } : {}), createdAt: Date.now(), state: 'stopped', pid: null, account: null }; data.accounts.push(account); data.operations.accounts.push({ id: account.id, name: account.name, groupIds: [groupId], enabled: body.source !== 'api-key', maxConcurrency: 5, active: 0, queued: 0 }); return reply(account, 201); }
    if (path.startsWith('/accounts/')) {
      const id = path.split('/')[2], account = data.accounts.find(a => a.id === id), policy = data.operations.accounts.find(a => a.id === id);
      if (!account || !policy) return reply({}, 404);
      if (path.endsWith('/upstream')) { account.upstream = { baseUrl: body.baseUrl, transport: body.transport, hasApiKey: true }; data.operations.revision++; return reply(account.upstream); }
      if (path.endsWith('/features/verify')) return reply({ verified: true, enabled: !policy.disabledFeatures?.includes(body.feature) });
      if (path.endsWith('/features')) policy.disabledFeatures = body.disabledFeatures;
      if (path.endsWith('/activation')) {
        policy.enabled = body.enabled;
        if (body.enabled && !policy.route) policy.route = { id: 'openai', model: 'test-model', baseUrl: 'https://model.example.test' };
        if (!body.enabled && !policy.active) { account.pid = null; account.state = 'stopped'; }
      }
      if (path.endsWith('/start') || path.endsWith('/restart')) { account.pid = 12345; account.state = 'ready'; }
      if (path.endsWith('/stop')) { account.pid = null; account.state = 'stopped'; policy.active = 0; }
      if (path.endsWith('/route')) { policy.route = body.enabled ? { id: 'openai', model: 'test-model', baseUrl: 'https://model.example.test' } : undefined; }
      if (path.endsWith('/policy')) { Object.assign(policy, body); for (const group of data.operations.groups) group.memberCount = data.operations.accounts.filter(account => account.groupIds.includes(group.id)).length; }
      if (path.endsWith('/proxy') && state.conflict) { state.conflict = false; return reply({ error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请刷新后重试。' } }, 409); }
      if (path.endsWith('/proxy')) { policy.proxy = body.config; policy.proxyStatus = { state: policy.active ? 'switching' : 'applied' }; }
      if (path.endsWith('/credentials')) account.credentialSource = 'managed';
      if (path.endsWith('/login')) {
        if (method !== 'GET') logins.set(id, { type: body.kind, loginId: 'login-fixture', status: 'waiting', startedAt: Date.now(), expiresAt: Date.now() + 600000, manualCallbackAvailable: body.kind === 'chatgpt', authUrl: 'https://auth.example.test', userCode: 'TEST-1234' });
        return reply(logins.get(id) ?? null);
      }
      data.operations.revision++; return reply(data.operations);
    }
    if (path === '/groups' || path.startsWith('/groups/')) {
      if (state.conflict) { state.conflict = false; return reply({ error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请重新打开编辑后重试。' } }, 409); }
      if (path === '/groups') data.operations.groups.push({ id: crypto.randomUUID(), ...body, active: 0, queued: 0, memberCount: 0 });
      else { const group = data.operations.groups.find(g => path.endsWith(g.id)); if (group) Object.assign(group, body); }
      data.operations.revision++; return reply(data.operations);
    }
    if (path === '/settings/debug') {
      if (method === 'POST') {
        if (body.revision !== state.debug.revision) return reply({ error: { message: '调试设置已变化' } }, 409);
        state.debug = { enabled: body.enabled, revision: state.debug.revision + 1 };
      }
      return reply(state.debug);
    }
    if (/^\/requests\/[^/]+\/content$/.test(path)) return reply({ id: path.split('/')[2], request: null, response: null });
    if (path === '/settings/access') return reply({ passwordEnabled: method === 'GET' ? false : body.passwordEnabled, allowPublicAccess: method === 'GET' ? true : body.allowPublicAccess });
    if (path === '/settings/concurrency') { data.operations.globalLimitAuto = body.globalLimitAuto === true; if (!data.operations.globalLimitAuto) data.operations.manualGlobalLimit = body.globalLimit; }
    if (path === '/settings/queue') Object.assign(data.operations, body);
    if (path === '/settings/native-affinity') {
      if (state.conflict) { state.conflict = false; return reply({ error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请刷新后重试。' } }, 409); }
      data.operations.nativeAffinityTtlHours = body.nativeAffinityTtlHours;
    }
    if (path.startsWith('/settings/')) { data.operations.revision++; return reply(data.operations); }
    if (path === '/jobs' || path.startsWith('/desktop/') || path === '/service/stop') return reply({ ok: true });
    return reply({ error: { code: 'FIXTURE_UNKNOWN', message: `Unmocked endpoint ${method} ${path}` } }, 404);
  });
  return state;
}

import { expect, test } from '@playwright/test';
import {
  accountSchema,
  accountQuotaSchema,
  statisticsSchema,
  nativeAffinitySettingsSchema,
  accountMembershipSchema,
  accountBackupSchema,
  nativeCapabilitiesSchema,
} from '../contracts/admin';
import { mockConsole, nativeCapabilities, nativeId, overview } from './fixtures';

test('native service tiers preserve unknown metadata and validate advertised tiers', () => {
  const capabilities = nativeCapabilities();
  expect(nativeCapabilitiesSchema.parse(capabilities).models[0].serviceTiers?.[0].id).toBe(
    'ultrafast',
  );
  capabilities.models[0].serviceTiers = null;
  expect(nativeCapabilitiesSchema.parse(capabilities).models[0].serviceTiers).toBeNull();
  expect(
    nativeCapabilitiesSchema.safeParse({
      ...capabilities,
      models: [{ ...capabilities.models[0], serviceTiers: [{ id: 'ultrafast' }] }],
    }).success,
  ).toBe(false);
});

test('API account backups require the upstream configuration and credential', () => {
  const item = {
    name: 'API',
    source: 'api-key',
    groupIds: ['group'],
    groupNames: ['default'],
    maxConcurrency: 3,
    requiresReconnect: false,
    document: null,
  };
  const backup = { format: 'codex-relay-accounts', version: 1, items: [item] };
  expect(accountBackupSchema.safeParse(backup).success).toBe(false);
  expect(
    accountBackupSchema.safeParse({
      ...backup,
      items: [{ ...item, upstream: { baseUrl: 'https://example.test/v1', transport: 'ws-first' } }],
    }).success,
  ).toBe(false);
  expect(
    accountBackupSchema.safeParse({
      ...backup,
      items: [
        {
          ...item,
          upstream: {
            baseUrl: 'https://example.test/v1',
            apiKey: 'synthetic-api-key',
            transport: 'ws-first',
          },
        },
      ],
    }).success,
  ).toBe(true);
});

test('console mocks obey the same account, quota and statistics contracts as Rust responses', async ({
  page,
}) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  state.records.push(
    {
      ...record,
      id: 'unknown-usage',
      inputTokens: null,
      outputTokens: null,
      cachedInputTokens: null,
    },
    { ...record, id: 'zero-usage', inputTokens: 0, outputTokens: 0, cachedInputTokens: 0 },
  );
  await page.goto('/?view=accounts');
  const responses = await page.evaluate(async (id) => {
    const paths = [
      '/api/overview',
      '/api/accounts/' + id + '/quota',
      '/api/statistics?from=0&to=' + Date.now(),
    ];
    return Promise.all(
      paths.map(async (path) => {
        const response = await fetch(path);
        if (!response.ok) throw new Error(path + ': ' + response.status);
        return response.json();
      }),
    );
  }, nativeId);
  const accounts = responses[0].accounts as unknown[];
  expect(nativeAffinitySettingsSchema.parse(responses[0].operations).nativeAffinityTtlHours).toBe(
    24,
  );
  expect(accounts).toHaveLength(3);
  for (const account of accounts) accountSchema.parse(account);
  for (const policy of responses[0].operations.accounts) accountMembershipSchema.parse(policy);
  expect(accountQuotaSchema.parse(responses[1]).status).toBe('unknown');
  const statistics = statisticsSchema.parse(responses[2]);
  expect(statistics.records.some((row) => row.inputTokens === null)).toBe(true);
  expect(statistics.records.some((row) => row.inputTokens === 0)).toBe(true);
  expect(
    statisticsSchema.safeParse({
      ...statistics,
      summary: { ...statistics.summary, inputTokens: undefined },
    }).success,
  ).toBe(false);
  expect(
    statisticsSchema.safeParse({
      ...statistics,
      records: [{ ...statistics.records[0], inputTokens: '100' }],
    }).success,
  ).toBe(false);
  for (const field of ['clientTransport', 'upstreamTransport']) {
    for (const invalid of [undefined, 'ws-first', true, 1]) {
      expect(
        statisticsSchema.safeParse({
          ...statistics,
          records: [{ ...statistics.records[0], [field]: invalid }],
        }).success,
      ).toBe(false);
    }
  }
  for (const invalid of [undefined, true, 1]) {
    expect(
      statisticsSchema.safeParse({
        ...statistics,
        records: [{ ...statistics.records[0], responseServiceTier: invalid }],
      }).success,
    ).toBe(false);
  }
});

test('mock empty statistics keep nullable usage rather than fabricating numeric totals', async ({
  page,
}) => {
  await mockConsole(page, { emptyRecords: true });
  await page.goto('/?view=statistics');
  const response = await page.evaluate(async () => (await fetch('/api/statistics')).json());
  const statistics = statisticsSchema.parse(response);
  expect(statistics.records).toEqual([]);
  expect(statistics.summary.inputTokens).toBeNull();
});

test('contracts reject missing fields, invalid states and numeric strings', () => {
  expect(accountMembershipSchema.safeParse({ groupId: 'old-group' }).success).toBe(false);
  expect(accountMembershipSchema.safeParse({ groupIds: [] }).success).toBe(false);
  expect(
    accountMembershipSchema.safeParse({
      groupIds: overview().operations.accounts[0].groupIds,
      groupId: 'old-group',
    }).success,
  ).toBe(false);
  for (const value of [undefined, '24', 0, 1.5, 721]) {
    expect(nativeAffinitySettingsSchema.safeParse({ nativeAffinityTtlHours: value }).success).toBe(
      false,
    );
  }
  const account = overview().accounts[0];
  expect(accountSchema.safeParse({ ...account, pid: undefined }).success).toBe(false);
  expect(accountSchema.safeParse({ ...account, source: 'future-invalid-source' }).success).toBe(
    false,
  );
  const quota = { status: 'unknown', stale: true, blocked: false, windows: [] };
  expect(accountQuotaSchema.safeParse({ ...quota, status: 'ready' }).success).toBe(false);
  expect(accountQuotaSchema.safeParse({ ...quota, checkedAt: '1000' }).success).toBe(false);
  expect(accountQuotaSchema.safeParse({ ...quota, windows: null }).success).toBe(false);
});

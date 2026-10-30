import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { mockConsole, groupId, oauthId, nativeId } from './fixtures';
import { accountBackupSchema, accountBatchResultSchema } from '../contracts/admin';

const credential = {
  tokens: {
    access_token: 'synthetic-access',
    id_token: 'synthetic-id',
    refresh_token: 'synthetic-refresh',
  },
};
const entry = {
  name: '迁移账户',
  source: 'isolated',
  groupIds: [groupId],
  groupNames: ['默认分组'],
  maxConcurrency: 3,
  requiresReconnect: false,
  document: credential,
};
const backup = { format: 'codex-relay-accounts', version: 1, items: [entry] };

test('single-account export downloads credentials directly and can import the file unchanged', async ({
  page,
}, testInfo) => {
  await mockConsole(page);
  let calls = 0;
  await page.route('**/api/accounts/export', (route) => {
    calls++;
    expect(route.request().postDataJSON()).toEqual({ accountIds: [oauthId] });
    accountBackupSchema.parse(backup);
    return route.fulfill({ json: backup });
  });
  let imported: unknown;
  await page.route(`**/api/accounts/${oauthId}/credentials`, (route) => {
    imported = route.request().postDataJSON();
    return route.fulfill({ json: { ok: true } });
  });
  await page.goto(`/?view=accounts&account=${oauthId}&section=login`);
  await page.getByRole('button', { name: '导出凭据', exact: true }).click();
  expect(calls).toBe(0);
  const dialog = page.getByRole('dialog', { name: '导出单账户凭据' });
  await expect(dialog).toContainText('不包含名称、分组或并发配置');
  const downloadEvent = page.waitForEvent('download');
  await dialog.getByRole('button', { name: '确认下载凭据 JSON' }).click();
  const download = await downloadEvent;
  const path = testInfo.outputPath('credentials.json');
  await download.saveAs(path);
  expect(JSON.parse(await readFile(path, 'utf8'))).toEqual(credential);
  await page.getByLabel('选择凭据文件').setInputFiles(path);
  await page.getByRole('button', { name: '导入到独立实例' }).click();
  await expect.poll(() => imported).toEqual({ document: credential });
  expect(await page.evaluate(() => JSON.stringify([localStorage, sessionStorage]))).not.toContain(
    'synthetic-refresh',
  );
});

test('batch export restores only supported import fields and displays partial results', async ({
  page,
}, testInfo) => {
  await mockConsole(page);
  const exported = {
    ...backup,
    items: [
      entry,
      {
        ...entry,
        name: '本机配置',
        source: 'native-config',
        document: null,
        requiresReconnect: true,
      },
      {
        ...entry,
        name: 'API 账户',
        source: 'api-key',
        document: null,
        requiresReconnect: false,
        upstream: {
          baseUrl: 'https://upstream.example/v1',
          apiKey: 'synthetic-upstream-secret',
          transport: 'ws-first',
        },
      },
    ],
  };
  await page.route('**/api/accounts/export', (route) => route.fulfill({ json: exported }));
  let submitted: unknown;
  await page.route('**/api/accounts/batch', (route) => {
    submitted = route.request().postDataJSON();
    const result = {
      action: 'import',
      status: 'partial',
      items: [
        { status: 'skipped', code: 'DUPLICATE_ACCOUNT' },
        { accountId: nativeId, status: 'succeeded' },
        { accountId: '10000000-0000-0000-0000-000000000004', status: 'succeeded' },
      ],
    };
    accountBatchResultSchema.parse(result);
    return route.fulfill({ json: result });
  });
  await page.goto('/?view=accounts');
  await page.getByRole('checkbox', { name: '选择当前页全部账号' }).check();
  await page.getByRole('button', { name: '导出账户备份', exact: true }).click();
  const downloadEvent = page.waitForEvent('download');
  await page.getByRole('button', { name: '确认下载账户备份' }).click();
  const download = await downloadEvent;
  const path = testInfo.outputPath('accounts.json');
  await download.saveAs(path);
  expect(JSON.parse(await readFile(path, 'utf8'))).toEqual(exported);
  await page.getByRole('button', { name: '导入账户备份', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('账户备份文件', { exact: true }).setInputFiles(path);
  await expect(dialog.getByLabel('备份预览')).toContainText('可导入 3 个账户');
  await expect(dialog).toContainText('API Key 配置 · 包含上游密钥');
  await expect(dialog.getByLabel('备份预览')).not.toContainText('synthetic-upstream-secret');
  await dialog.getByRole('button', { name: '确认导入账户' }).click();
  await expect(dialog.getByLabel('导入结果')).toContainText('账户身份已存在，已跳过');
  await expect(dialog.getByLabel('导入结果')).toContainText('需要重新连接');
  await expect(dialog.getByLabel('导入结果')).toContainText('已导入上游配置和密钥，可直接启用');
  expect(submitted).toEqual({
    revision: 1,
    action: 'import',
    items: [
      {
        name: entry.name,
        source: 'isolated',
        document: credential,
        groupIds: [groupId],
        maxConcurrency: 3,
      },
      { name: '本机配置', source: 'native-config', groupIds: [groupId], maxConcurrency: 3 },
      {
        name: 'API 账户',
        source: 'api-key',
        groupIds: [groupId],
        maxConcurrency: 3,
        upstream: {
          baseUrl: 'https://upstream.example/v1',
          apiKey: 'synthetic-upstream-secret',
          transport: 'ws-first',
        },
      },
    ],
  });
  await expect(dialog.getByRole('textbox')).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: '确认导入账户' })).toHaveCount(0);
  expect(await page.evaluate(() => JSON.stringify([localStorage, sessionStorage]))).not.toContain(
    'synthetic-upstream-secret',
  );
});

for (const width of [375, 1440])
  test(`backup migration requires a destination group and fits ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 900 });
    await mockConsole(page, { count: 0 });
    let submitted: any;
    await page.route('**/api/accounts/batch', (route) => {
      submitted = route.request().postDataJSON();
      return route.fulfill({
        json: {
          action: 'import',
          status: 'completed',
          items: [{ status: 'succeeded', accountId: oauthId }],
        },
      });
    });
    await page.goto('/?view=accounts');
    const trigger = page.getByRole('button', { name: '导入账户备份', exact: true });
    await trigger.click();
    const dialog = page.getByRole('dialog');
    const migrated = {
      ...backup,
      items: [{ ...entry, groupIds: ['99999999-0000-0000-0000-000000000000'] }],
    };
    await dialog.getByLabel('或粘贴账户备份 JSON').fill(JSON.stringify(migrated));
    await dialog.getByRole('button', { name: '预览备份' }).click();
    await expect(dialog.getByRole('button', { name: '确认导入账户' })).toBeDisabled();
    await expect(dialog).toContainText('原分组不存在');
    await dialog.getByLabel('导入分组', { exact: true }).selectOption(groupId);
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    ).toBeTruthy();
    expect(await dialog.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBeTruthy();
    await page.screenshot({
      path: testInfo.outputPath(`account-import-${width}.png`),
      fullPage: true,
    });
    await dialog.getByRole('button', { name: '确认导入账户' }).click();
    await expect(dialog.getByLabel('导入结果')).toContainText('账户保持停用');
    expect(submitted.groupIds).toEqual([groupId]);
    expect(submitted.enabled).toBeUndefined();
    await dialog.getByRole('button', { name: '完成并关闭' }).click();
    await expect(trigger).toBeFocused();
    await trigger.click();
    await expect(dialog.getByLabel('或粘贴账户备份 JSON')).toHaveValue('');
  });

test('invalid, oversized and credential-only files are rejected before any request', async ({
  page,
}) => {
  await mockConsole(page);
  let calls = 0;
  await page.route('**/api/accounts/batch', (route) => {
    calls++;
    return route.fulfill({ json: {} });
  });
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '导入账户备份', exact: true }).click();
  const dialog = page.getByRole('dialog');
  for (const [value, message] of [
    ['{broken', 'JSON 格式无效'],
    [JSON.stringify(credential), '单账户凭据'],
    [JSON.stringify({ ...backup, version: 2 }), '版本为 1'],
    [JSON.stringify({ ...backup, items: [{ ...entry, groupIds: undefined, groupId }] }), '分组'],
    [JSON.stringify({ ...backup, items: [{ ...entry, groupIds: undefined }] }), '分组'],
    [JSON.stringify({ ...backup, items: Array(33).fill(entry) }), '1–32'],
    [
      JSON.stringify({
        ...backup,
        items: [
          {
            ...entry,
            source: 'api-key',
            document: null,
            upstream: {
              baseUrl: 'https://example.test/v1',
              apiKey: 'synthetic-key',
              transport: ['ws-first'],
            },
          },
        ],
      }),
      '无效',
    ],
    ...[undefined, '', 'synthetic-key\n'].map((apiKey) => [
      JSON.stringify({
        ...backup,
        items: [
          {
            ...entry,
            source: 'api-key',
            document: null,
            upstream: { baseUrl: 'https://example.test/v1', apiKey, transport: 'http-sse' },
          },
        ],
      }),
      '无效',
    ]),
    [
      JSON.stringify({
        ...backup,
        items: [{ ...entry, document: { access_token: 'x'.repeat(262144) } }],
      }),
      '256 KiB',
    ],
  ]) {
    await dialog.getByLabel('或粘贴账户备份 JSON').fill(value);
    await dialog.getByRole('button', { name: '预览备份' }).click();
    await expect(dialog.getByRole('alert')).toContainText(message);
    await expect(dialog.getByRole('alert')).toBeFocused();
    await expect(dialog.getByRole('button', { name: '确认导入账户' })).toBeDisabled();
  }
  await dialog.getByLabel('账户备份文件', { exact: true }).setInputFiles({
    name: 'large.json',
    mimeType: 'application/json',
    buffer: Buffer.alloc(4 * 1024 * 1024 + 1),
  });
  await expect(dialog.getByRole('alert')).toContainText('4 MiB');
  expect(calls).toBe(0);
});

test('network interruption clears credentials and prevents accidental replay', async ({ page }) => {
  await mockConsole(page);
  let calls = 0;
  await page.route('**/api/accounts/batch', (route) => {
    calls++;
    return route.abort();
  });
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '导入账户备份', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('或粘贴账户备份 JSON').fill(JSON.stringify(backup));
  await dialog.getByRole('button', { name: '预览备份' }).click();
  await dialog.getByRole('button', { name: '确认导入账户' }).click();
  await expect(dialog.getByRole('alert')).toContainText('导入可能仍在执行');
  await expect(dialog.getByRole('textbox')).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: '确认导入账户' })).toHaveCount(0);
  expect(calls).toBe(1);
});

test('export respects unavailable credentials and queued-account restrictions', async ({
  page,
}) => {
  const state = await mockConsole(page);
  state.data.accounts[1].credentialSource = 'desktop';
  await page.goto(`/?view=accounts&account=${oauthId}`);
  await expect(page.getByRole('heading', { name: '凭据管理' })).toBeVisible();
  await expect(page.getByRole('button', { name: '导出凭据', exact: true })).toHaveCount(0);
  state.data.accounts[1].credentialSource = 'external';
  state.data.operations.accounts[1].queued = 1;
  await page.reload();
  await expect(page.getByRole('button', { name: '导出凭据', exact: true })).toBeDisabled();
  state.data.operations.accounts[1].queued = 0;
  await page.reload();
  await page.route('**/api/accounts/export', (route) =>
    route.fulfill({
      status: 409,
      json: { error: { code: 'BUSY', message: '账户忙碌，请稍后重试。' } },
    }),
  );
  await page.getByRole('button', { name: '导出凭据', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('仅有 Access Token');
  await dialog.getByRole('button', { name: '确认下载凭据 JSON' }).click();
  await expect(dialog.getByRole('alert')).toContainText('账户忙碌');
});

import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

const downloadUrl = 'https://registry.npmjs.org/@openai/codex/-/codex-0.166.0-linux-x64.tgz';
async function managed(page: Parameters<typeof mockConsole>[0]) {
  const state = await mockConsole(page);
  state.data.managedCodex = {
    supported: true,
    architecture: 'x64',
    latestVersion: '0.166.0',
    activeVersion: '0.166.0',
    downloadUrl,
    checkedAt: Date.now(),
    lastAttemptAt: Date.now(),
    operation: null,
    error: null,
    versions: [
      { version: '0.166.0', installedAt: 1, inUse: true },
      { version: '0.165.0', installedAt: 1, inUse: false },
      { version: '0.164.0', installedAt: 1, inUse: true },
    ],
  };
  return state;
}

test('Linux download link can be copied and standalone paths never submit downloads', async ({
  page,
}) => {
  const state = await managed(page);
  state.data.installation = null;
  state.data.managedCodex.activeVersion = null;
  let copied = '';
  await page.exposeFunction('recordCopy', (value: string) => {
    copied = value;
  });
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText: (value: string) => (window as any).recordCopy(value) },
    });
  });
  const downloads: unknown[] = [];
  await page.route('**/api/native-runtime/actions', async (route) => {
    downloads.push(route.request().postDataJSON());
    await route.fulfill({ status: 202, json: { id: 'download' } });
  });
  let prepared: unknown;
  await page.route('**/api/jobs', async (route) => {
    prepared = route.request().postDataJSON();
    await route.fulfill({ status: 202, json: { id: 'path' } });
  });
  await page.goto('/?view=settings&tab=diagnostics');
  await expect(page.getByRole('link', { name: downloadUrl })).toHaveAttribute('href', downloadUrl);
  await page.getByRole('button', { name: '复制下载链接' }).click();
  await expect(page.getByText('下载链接已复制。')).toBeVisible();
  expect(copied).toBe(downloadUrl);
  await page.getByLabel('运行模式').selectOption('standalone');
  await page.getByLabel('Codex 路径', { exact: true }).fill('/opt/codex/bin/codex');
  await page.getByRole('button', { name: '校验并保存路径' }).click();
  await expect
    .poll(() => prepared)
    .toEqual({ kind: 'prepare', paths: { appServerExecutable: '/opt/codex/bin/codex' } });
  expect(downloads).toEqual([]);
  await expect(page.getByRole('link', { name: downloadUrl })).toBeVisible();
});

test('any old version can be activated; current and in-use versions cannot be deleted', async ({
  page,
}) => {
  const state = await managed(page);
  const actions: any[] = [];
  await page.route('**/api/native-runtime/actions', async (route) => {
    const action = route.request().postDataJSON();
    actions.push(action);
    if (action.action === 'activate') state.data.managedCodex.activeVersion = action.version;
    if (action.action === 'remove')
      state.data.managedCodex.versions = state.data.managedCodex.versions.filter(
        (v) => v.version !== action.version,
      );
    await route.fulfill({ status: 202, json: { id: 'action' } });
  });
  await page.goto('/?view=settings&tab=diagnostics');
  await expect(
    page.getByRole('button', { name: '删除 Codex 0.166.0', exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole('button', { name: '删除 Codex 0.164.0', exact: true }),
  ).toBeDisabled();
  await page.getByRole('button', { name: '启用 Codex 0.164.0' }).click();
  await expect(page.getByRole('button', { name: '启用 Codex 0.164.0' })).toBeDisabled();
  expect(actions[0]).toEqual({ action: 'activate', version: '0.164.0' });
  await page.getByRole('button', { name: '删除 Codex 0.165.0', exact: true }).click();
  expect(actions).toHaveLength(1);
  await page.getByRole('button', { name: '确认删除 Codex 0.165.0' }).click();
  await expect(page.getByText('0.165.0', { exact: true })).toHaveCount(0);
  expect(actions[1]).toEqual({ action: 'remove', version: '0.165.0' });
});

test('download progress, cancellation and failure recover without changing current selection', async ({
  page,
}) => {
  const state = await managed(page);
  state.data.managedCodex.latestVersion = '0.167.0';
  let action: unknown;
  await page.route('**/api/native-runtime/actions', async (route) => {
    action = route.request().postDataJSON();
    state.data.busy = true;
    state.data.managedCodex.operation = {
      id: 'download',
      action: 'install',
      phase: 'downloading',
      downloadedBytes: 1024,
      totalBytes: 4096,
      startedAt: Date.now(),
      finishedAt: null,
      message: '正在下载官方 Codex…',
    };
    await route.fulfill({ status: 202, json: { id: 'download' } });
  });
  await page.route('**/api/native-runtime/cancel', async (route) => {
    expect(route.request().postDataJSON()).toEqual({ id: 'download' });
    state.data.busy = false;
    Object.assign(state.data.managedCodex.operation!, {
      phase: 'cancelled',
      finishedAt: Date.now(),
      message: '操作已取消，当前运行版本未改变。',
    });
    await route.fulfill({ json: { id: 'download' } });
  });
  await page.goto('/?view=settings&tab=diagnostics');
  await page.getByRole('button', { name: '下载并启用新版' }).click();
  await expect(page.getByRole('progressbar', { name: 'Codex 下载进度' })).toHaveAttribute(
    'value',
    '1024',
  );
  expect(action).toEqual({ action: 'install', version: '0.167.0' });
  await expect(page.getByRole('button', { name: '检查更新', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '取消操作', exact: true }).click();
  await expect(page.getByText('操作已取消，当前运行版本未改变。')).toBeVisible();
  await expect(page.getByRole('button', { name: '下载并启用新版' })).toBeEnabled();
  state.data.managedCodex.error = {
    code: 'CODEX_INTEGRITY',
    message: '下载文件校验失败，已丢弃下载内容；请重试。',
  };
  await expect(page.getByRole('alert')).toContainText('下载文件校验失败');
  expect(state.data.managedCodex.activeVersion).toBe('0.166.0');
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await expect(page.getByRole('alert')).toContainText('下载文件校验失败');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  await page
    .getByRole('region', { name: '原生运行环境' })
    .screenshot({ path: '../.cache/managed-codex-mobile.png' });
});

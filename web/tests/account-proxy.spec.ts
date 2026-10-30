import { test, expect } from '@playwright/test';
import { mockConsole, nativeId, oauthId } from './fixtures';

test('account list shows automatic proxy endpoints and refreshes them without affecting other modes', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.automaticProxy = { state: 'proxy', http: 'http://127.0.0.1:7890', https: 'http://127.0.0.1:7890' };
  state.data.operations.accounts[1].proxy = { mode: 'direct', url: '' };
  state.data.operations.accounts[2].proxy = { mode: 'custom', url: 'http://localhost:7897' };
  await page.goto('/?view=accounts');
  const link = page.getByRole('link', { name: '本机 Codex的代理设置', exact: true });
  await expect(link).toHaveText('代理：自动 · http://127.0.0.1:7890');
  await expect(page.getByRole('link', { name: '独立测试账户的代理设置', exact: true })).toHaveText('代理：直连');
  await expect(page.locator('.account-proxy-summary').nth(2)).toHaveText('代理：指定代理 · http://localhost:7897');
  state.data.automaticProxy = { state: 'proxy', http: 'http://proxy.example:8080', https: 'socks5h://[::1]:1080' };
  await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
  await expect(link).toHaveText('代理：自动 · HTTP http://proxy.example:8080 · HTTPS socks5h://[::1]:1080');
  await page.setViewportSize({ width: 375, height: 844 });
  await link.screenshot({ path: '../.cache/automatic-proxy-mobile.png' });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  state.data.automaticProxy = { state: 'error', http: null, https: null };
  await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
  await expect(link).toHaveText('代理：自动 · 代理解析失败');
  state.data.automaticProxy = { state: 'direct', http: null, https: null };
  await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
  await expect(link).toHaveText('代理：自动 · 直连');
  expect(state.writes).toHaveLength(0);
});

test('accounts default to auto and save independent proxy modes', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=accounts');
  const link = page.getByRole('link', { name: '本机 Codex的代理设置', exact: true });
  await expect(link).toHaveText('代理：自动 · 直连');
  await link.click();
  await expect(page.getByLabel('连接方式', { exact: true })).toHaveValue('auto');
  await expect(page.getByText('优先使用启动环境的代理变量；未指定时读取 Windows / macOS 系统手动 HTTP/HTTPS 代理。系统代理变更后，请重启已运行的账户实例。', { exact: true })).toBeVisible();
  await page.getByLabel('连接方式', { exact: true }).selectOption('custom');
  await page.getByLabel('代理地址', { exact: true }).fill('http://127.0.0.1:7897');
  await page.getByRole('button', { name: '保存代理', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存代理', exact: true })).toBeDisabled();
  expect(state.data.operations.accounts[0].proxy).toEqual({ mode: 'custom', url: 'http://127.0.0.1:7897' });
  await page.goto(`/?view=accounts&account=${oauthId}&section=access`);
  await expect(page.getByLabel('连接方式', { exact: true })).toHaveValue('auto');
  await page.getByLabel('连接方式', { exact: true }).selectOption('direct');
  await page.getByRole('button', { name: '保存代理', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存代理', exact: true })).toBeDisabled();
  expect(state.data.operations.accounts[1].proxy).toEqual({ mode: 'direct', url: '' });
  expect(state.data.operations.accounts[0].proxy?.url).toBe('http://127.0.0.1:7897');
  await page.reload();
  await expect(page.getByLabel('连接方式', { exact: true })).toHaveValue('direct');
  await page.goto('/?view=settings');
  await expect(page.getByLabel('连接方式', { exact: true })).toHaveCount(0);
  await expect(page.getByText('app-server 代理', { exact: true })).toHaveCount(0);
  expect(state.writes.every(item => item.path !== '/settings/app-server-proxy')).toBe(true);
});

test('invalid proxy addresses never submit and conflicts preserve the draft', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  await page.getByLabel('连接方式', { exact: true }).selectOption('custom');
  for (const url of ['socks5://localhost:1080', 'http://user:password@localhost:8080', 'http://localhost/path', 'http://localhost:0']) {
    await page.getByLabel('代理地址', { exact: true }).fill(url);
    await page.getByRole('button', { name: '保存代理', exact: true }).click();
    await expect(page.getByText('请输入有效的 HTTP/HTTPS 代理地址，不包含账号密码、路径或查询参数。', { exact: true })).toBeVisible();
  }
  expect(state.writes).toHaveLength(0);
  await page.getByLabel('代理地址', { exact: true }).fill('http://127.0.0.1:7897');
  state.conflict = true;
  await page.getByRole('button', { name: '保存代理', exact: true }).click();
  await expect(page.getByText('配置已更新，请刷新后重试。', { exact: true })).toBeVisible();
  await expect(page.getByLabel('代理地址', { exact: true })).toHaveValue('http://127.0.0.1:7897');
  await page.getByRole('button', { name: '还原代理', exact: true }).click();
  await expect(page.getByLabel('连接方式', { exact: true })).toHaveValue('auto');
});

test('polling keeps unsaved edits and their revision while busy saves show immediate restart status', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].active = 1;
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  await page.getByLabel('连接方式', { exact: true }).selectOption('custom');
  await page.getByLabel('代理地址', { exact: true }).fill('http://127.0.0.1:7897');
  const before = state.overviewCalls;
  state.data.operations.revision = 10;
  await expect.poll(() => state.overviewCalls, { timeout: 15000 }).toBeGreaterThan(before);
  await expect(page.getByLabel('代理地址', { exact: true })).toHaveValue('http://127.0.0.1:7897');
  await page.getByRole('button', { name: '保存代理', exact: true }).click();
  await expect(page.locator('#account-proxy').getByRole('status')).toContainText('正在立即重启');
  expect(state.writes.find(item => item.path.endsWith('/proxy'))?.body.revision).toBe(1);
  expect(state.data.operations.accounts[0].active).toBe(1);
});

test('failed switch retries the saved configuration without discarding draft changes', async ({ page }) => {
  const state = await mockConsole(page);
  const policy = state.data.operations.accounts[0];
  policy.proxy = { mode: 'custom', url: 'http://127.0.0.1:7897' };
  policy.proxyStatus = { state: 'failed', error: { code: 'IO_ERROR', message: '实例启动失败。' } };
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  await expect(page.locator('#account-proxy').getByRole('alert')).toContainText('实例启动失败。');
  await page.getByLabel('连接方式', { exact: true }).selectOption('direct');
  await page.getByRole('button', { name: '重试切换', exact: true }).click();
  await expect(page.locator('#account-proxy').getByRole('status')).toHaveText('已生效');
  expect(state.writes.find(item => item.path.endsWith('/proxy'))?.body.config).toEqual(policy.proxy);
  await expect(page.getByLabel('连接方式', { exact: true })).toHaveValue('direct');
  await page.getByRole('button', { name: '保存代理', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存代理', exact: true })).toBeDisabled();
  expect(state.writes.filter(item => item.path.endsWith('/proxy'))[1].body.revision).toBe(2);
});

test('proxy settings fit desktop and mobile viewports', async ({ page }, testInfo) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].location = { enabled: true, source: 'proxy', custom: { timezone: '', country: '', region: '', city: '' } };
  state.data.operations.accounts[0].locationStatus = { state: 'applied', effective: { timezone: 'Asia/Shanghai', country: 'CN', region: '', city: '' } };
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  await page.getByLabel('连接方式', { exact: true }).selectOption('custom');
  await page.getByLabel('代理地址', { exact: true }).fill('http://127.0.0.1:7897');
  for (const width of [2531, 1440, 1024, 390]) {
    await page.setViewportSize({ width, height: 1000 });
    await expect(page.getByRole('button', { name: '保存代理', exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    if (width === 390) await expect(page.locator('.sidebar')).toBeHidden();
    await page.screenshot({ path: testInfo.outputPath(`account-proxy-${width}.png`), fullPage: true, animations: 'disabled' });
  }
  await page.setViewportSize({ width: 2531, height: 1252 });
  await page.getByLabel('连接方式', { exact: true }).selectOption('auto');
  await page.getByRole('heading', { name: '时区与搜索位置', exact: true }).click();
  await page.locator('.account-detail-section').screenshot({ path: testInfo.outputPath('account-settings-light.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await page.locator('.account-detail-section').screenshot({ path: testInfo.outputPath('account-settings-dark.png'), animations: 'disabled' });
  await page.screenshot({ path: testInfo.outputPath('account-settings-wide-dark.png'), fullPage: true, animations: 'disabled' });
});

test('native reinspection identifies the account so it uses that account proxy', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].proxy = { mode: 'custom', url: 'http://127.0.0.1:7897' };
  await page.goto(`/?view=accounts&account=${nativeId}&section=login`);
  await page.getByRole('button', { name: '重新检测并更新', exact: true }).click();
  await expect.poll(() => state.writes.find(item => item.path === '/local-connection/inspect')?.body.accountId).toBe(nativeId);
});

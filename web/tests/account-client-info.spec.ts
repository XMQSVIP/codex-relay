import { test, expect } from '@playwright/test';
import { mockConsole, nativeId, oauthId } from './fixtures';

test('account management shows actual runtime client identities and multiple versions', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.accounts[0].state = 'generating';
  state.data.accounts[0].clients = [
    { pid: 1234, clientInfo: { name: 'codex_desktop', title: 'Codex Desktop', version: '26.1021.31345' }, userAgent: 'runtime-agent/1.0' },
    { pid: 5678, clientInfo: { name: 'codex_desktop', title: 'Codex Desktop', version: '26.1025.11240' }, userAgent: null },
  ];
  await page.goto('/?view=accounts');
  await expect(page.locator(`#account-${nativeId}`).getByText('codex_desktop 26.1021.31345 / codex_desktop 26.1025.11240', { exact: true })).toBeVisible();
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  const info = page.getByRole('region', { name: '当前 clientInfo', exact: true });
  const runtime = page.getByRole('region', { name: '账户运行状态', exact: true });
  await expect(info).toHaveCount(0);
  await expect(runtime).toHaveCount(0);
  await expect(page.getByLabel('账户状态：可调度', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '测试对话：本机 Codex', exact: true })).toHaveCount(0);
  const tabs = page.getByRole('navigation', { name: '账户配置分类' });
  await tabs.getByRole('link', { name: '运行信息', exact: true }).click();
  await expect(page).toHaveURL(/section=runtime/);
  await expect(tabs.getByRole('link', { name: '运行信息', exact: true })).toHaveAttribute('aria-current', 'page');
  await page.reload();
  await expect(runtime.getByText('实例运行', { exact: true })).toBeVisible();
  await expect(runtime.getByRole('button', { name: '测试对话：本机 Codex', exact: true })).toBeVisible();
  await expect(info.getByText('26.1021.31345', { exact: true })).toBeVisible();
  await expect(info.getByText('26.1025.11240', { exact: true })).toBeVisible();
  await expect(info.getByText('runtime-agent/1.0', { exact: true })).toBeVisible();
  await info.getByText('User-Agent', { exact: true }).click();
  await expect(info.getByText('runtime-agent/1.0', { exact: true })).toBeHidden();
  await info.getByText('User-Agent', { exact: true }).click();
  await expect(info.getByText('runtime-agent/1.0', { exact: true })).toBeVisible();
  await page.setViewportSize({ width: 375, height: 844 });
  await info.screenshot({ path: '../.cache/account-client-info-mobile.png' });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.goto(`/?view=accounts&account=${oauthId}&section=client`);
  await expect(tabs.getByRole('link', { name: '运行信息', exact: true })).toHaveAttribute('aria-current', 'page');
  await expect(info).toContainText('当前没有可读取身份的运行实例');
  await expect(info.getByText('26.1021.31345', { exact: true })).toHaveCount(0);
  await tabs.getByRole('link', { name: '登录与凭据', exact: true }).click();
  await expect(info).toHaveCount(0);
  await expect(runtime).toHaveCount(0);
  expect(state.writes).toHaveLength(0);
});

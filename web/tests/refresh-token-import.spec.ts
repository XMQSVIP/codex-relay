import { expect, test } from '@playwright/test';
import { mockConsole, oauthId } from './fixtures';

test('RT import submits the selected type, blocks duplicate submits and clears on success', async ({ page }) => {
  const state = await mockConsole(page);
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/accounts/*/credentials', async route => {
    await pending;
    await route.fallback();
  });
  await page.goto(`/?view=accounts&account=${oauthId}&section=credentials`);
  await page.getByLabel('纯文本令牌类型').selectOption('refreshToken');
  const input = page.getByLabel('或粘贴 JSON / Refresh Token');
  await input.fill('  synthetic-rt  ');
  await page.getByRole('button', { name: '导入到独立实例', exact: true }).click();
  await expect(page.getByRole('button', { name: '正在导入…', exact: true })).toBeDisabled();
  await expect(input).toBeDisabled();
  await expect(page.getByLabel('纯文本令牌类型')).toBeDisabled();
  release();
  await expect(page.getByText('凭据已保存。请在“接入与调度”中启用账户。')).toBeVisible();
  await expect(input).toHaveValue('');
  const imports = state.writes.filter(item => item.path.endsWith('/credentials'));
  expect(imports).toHaveLength(1);
  expect(imports[0].body).toEqual({ document: { refreshToken: 'synthetic-rt' } });
  await page.screenshot({ path: '../.cache/rt-import-desktop.png', fullPage: true });
});

test('RT JSON works in default mode and a failed exchange retains the draft', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const state = await mockConsole(page);
  let rejected = true;
  await page.route('**/api/accounts/*/credentials', route => {
    if (!rejected) return route.fallback();
    rejected = false;
    return route.fulfill({ status: 400, json: { error: { code: 'OAUTH_IMPORT_REJECTED', message: 'Refresh Token 已失效，请重新登录。' } } });
  });
  await page.goto(`/?view=accounts&account=${oauthId}&section=credentials`);
  const input = page.getByLabel('或粘贴 JSON / Access Token');
  const draft = JSON.stringify({ tokens: { refresh_token: 'synthetic-rt' } });
  await input.fill(draft);
  await page.getByRole('button', { name: '导入到独立实例', exact: true }).click();
  await expect(page.getByRole('alert')).toHaveText('Refresh Token 已失效，请重新登录。');
  await expect(input).toHaveValue(draft);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: '../.cache/rt-import-mobile.png', fullPage: true });
  await expect(page.getByRole('button', { name: '导入到独立实例', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '导入到独立实例', exact: true }).click();
  await expect(input).toHaveValue('');
  expect(state.writes.find(item => item.path.endsWith('/credentials'))?.body).toEqual({ document: { tokens: { refresh_token: 'synthetic-rt' } } });
});

import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const mode of ['login', 'import'] as const) {
  test(`${mode} creation waits for refreshed accounts before navigation`, async ({ page }) => {
    await page.clock.install();
    await mockConsole(page);
    await page.goto('/?view=accounts');
    await page.getByRole('button', { name: '添加账户', exact: true }).click();
    await page.getByRole('radio', { name: mode === 'login' ? '登录其他 ChatGPT 账户' : '导入凭据', exact: true }).check();
    await page.getByLabel('账户名称', { exact: true }).fill('新建账户');
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    let refreshing = false;
    await page.route('**/api/overview', async route => { refreshing = true; await gate; await route.fallback(); });
    await page.getByRole('button', { name: mode === 'login' ? '创建并继续登录' : '创建并导入凭据', exact: true }).click();
    await expect.poll(() => refreshing).toBe(true);
    await expect(page.getByRole('dialog')).toBeVisible();
    expect(new URL(page.url()).searchParams.has('account')).toBe(false);
    await expect(page.getByRole('heading', { name: '账户不存在' })).toHaveCount(0);
    release();
    await expect(page.locator('.detail-heading h2')).toHaveText('新建账户');
    await expect(page.getByRole('heading', { name: '账户不存在' })).toHaveCount(0);
  });
}

test('creation refresh failure retries without creating a second account', async ({ page }) => {
  await page.clock.install();
  const state = await mockConsole(page);
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '添加账户', exact: true }).click();
  await page.getByRole('radio', { name: '登录其他 ChatGPT 账户', exact: true }).check();
  await page.getByLabel('账户名称', { exact: true }).fill('只创建一次');
  state.offline = true;
  await page.getByRole('button', { name: '创建并继续登录' }).click();
  await expect(page.getByRole('alert')).toContainText('账户已创建，但列表刷新失败');
  expect(new URL(page.url()).searchParams.has('account')).toBe(false);
  state.offline = false;
  await page.getByRole('button', { name: '重试刷新账户' }).click();
  await expect(page.locator('.detail-heading h2')).toHaveText('只创建一次');
  expect(state.writes.filter(item => item.path === '/accounts')).toHaveLength(1);
});

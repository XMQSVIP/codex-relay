import { test, expect } from '@playwright/test';
import { mockConsole, nativeId, oauthId } from './fixtures';

test('a running account can be disabled without interrupting its task and resumed', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.accounts[0].state = 'generating';
  state.data.operations.accounts[0].active = 1;
  await page.goto('/');
  const toggle = page.getByRole('switch', { name: '启用账户：本机 Codex', exact: true });
  await expect(toggle).toBeEnabled();
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  await expect(page.locator(`#account-${nativeId}`)).toContainText('当前任务完成后停止实例');
  expect(state.data.operations.accounts[0].active).toBe(1);
  expect(state.data.operations.accounts[0].route?.id).toBe('openai');
  expect(state.writes.map(item => item.path)).toEqual([`/accounts/${nativeId}/activation`]);
  await toggle.click();
  await expect(toggle).toBeChecked();
  expect(state.data.operations.accounts[0].active).toBe(1);
});

test('activation conflicts preserve the switch and a pending request cannot repeat', async ({ page }) => {
  const state = await mockConsole(page);
  let requests = 0;
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/accounts/*/activation', async route => {
    requests++;
    if (requests === 1) return route.fulfill({ status: 409, json: { error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请刷新后重试。' } } });
    await gate;
    return route.fallback();
  });
  await page.goto('/');
  const toggle = page.getByRole('switch', { name: '启用账户：独立测试账户', exact: true });
  await toggle.click();
  await expect(page.getByRole('alert')).toContainText('配置已更新');
  await expect(toggle).toBeChecked();
  await toggle.click();
  await expect(toggle).toBeDisabled();
  await expect(toggle).toHaveAttribute('aria-busy', 'true');
  await expect(toggle).toHaveText('停用中…');
  await page.keyboard.press('Space');
  expect(requests).toBe(2);
  release();
  await expect(toggle).not.toBeChecked();
  await expect(toggle).toBeEnabled();
  expect(state.data.operations.accounts[1].route?.id).toBe('openai');
});

test('the first activation configures a route and disabled accounts support manual debugging', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].route = undefined;
  await page.goto('/');
  const toggle = page.getByRole('switch', { name: '启用账户：本机 Codex', exact: true });
  await expect(toggle).not.toBeChecked();
  await toggle.click();
  await expect(toggle).toBeChecked();
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  const actions = page.locator(`#account-${nativeId} .account-row-actions`);
  await expect(actions.locator('button,a')).toHaveCount(4);
  await expect(actions.getByRole('button', { name: '展开详情：本机 Codex' })).toBeVisible();
  await expect(actions.getByRole('link', { name: '账户设置：本机 Codex', exact: true })).toBeVisible();
  await expect(actions.getByRole('button', { name: /^删除账户/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /启动实例|停止实例|重启实例/ })).toHaveCount(0);
  await page.goto(`/?view=accounts&account=${nativeId}&section=advanced`);
  await page.getByRole('button', { name: '启动实例', exact: true }).click();
  await expect(page.getByRole('button', { name: '停止实例', exact: true })).toBeVisible();
  expect(state.data.operations.accounts[0].enabled).toBe(false);
  await page.getByRole('button', { name: '重启实例', exact: true }).click();
  await expect.poll(() => state.writes.some(item => item.path.endsWith('/restart'))).toBe(true);
  expect(state.data.operations.accounts[0].enabled).toBe(false);
});

test('group pause does not change the account switch and credentials are required for first activation', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.groups[0].enabled = false;
  state.data.operations.accounts[1].route = undefined;
  state.data.operations.accounts[1].enabled = false;
  state.data.accounts[1].credentialSource = null;
  state.data.accounts[1].account = null;
  await page.goto('/');
  const native = page.getByRole('switch', { name: '启用账户：本机 Codex', exact: true });
  await expect(native).toBeChecked();
  await expect(page.locator(`#account-${nativeId}`)).toContainText('在“分组管理”恢复分组');
  await native.click();
  await expect(native).not.toBeChecked();
  await native.click();
  await expect(native).toBeChecked();
  await expect(page.getByRole('switch', { name: '启用账户：独立测试账户', exact: true })).toBeDisabled();
  await expect(page.locator(`#account-${oauthId}`).getByRole('link', { name: '登录或导入凭据', exact: true })).toBeVisible();
  await page.goto(`/?view=accounts&account=${oauthId}&section=access`);
  await expect(page.getByRole('switch', { name: '启用账户：独立测试账户', exact: true })).toBeDisabled();
  await expect(page.getByRole('link', { name: '登录或导入凭据', exact: true })).toBeVisible();
  await expect(page.getByLabel('参与调度')).toHaveCount(0);
});

for (const view of ['list', 'details'] as const) {
  test(`restored credentials can activate from ${view} before the first installation detection`, async ({ page }) => {
    const state = await mockConsole(page);
    state.data.installation = null;
    state.data.accounts[1].account = null;
    state.data.operations.accounts[1].route = undefined;
    state.data.operations.accounts[1].enabled = false;
    let requests = 0;
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    await page.route(`**/api/accounts/${oauthId}/activation`, async route => {
      requests++;
      await gate;
      return route.fallback();
    });
    await page.goto(view === 'list' ? '/?view=accounts' : `/?view=accounts&account=${oauthId}&section=access`);
    await page.clock.install();
    const toggle = page.getByRole('switch', { name: '启用账户：独立测试账户', exact: true });
    await expect(toggle).toBeEnabled();
    await expect(toggle).not.toBeChecked();
    const help = view === 'list' ? page.locator(`#account-${oauthId}`) : page.locator('.account-group').filter({ has: toggle });
    const status = help.locator(view === 'list' ? '.account-runtime-cell .badge' : '.settings-state');
    await expect(help).toContainText('启用时将自动检测原生安装');
    await toggle.click();
    try {
      await expect(toggle).toHaveText('启用中…');
      await expect(toggle).toBeDisabled();
      await expect(toggle).toHaveAttribute('aria-busy', 'true');
      await expect(toggle).not.toBeChecked();
      await expect(status).toHaveText('启用中');
      await page.keyboard.press('Space');
      expect(requests).toBe(1);
      // A poll may see the persisted activation before its POST response arrives.
      state.data.operations.accounts[1].enabled = true;
      state.data.operations.accounts[1].route = { id: 'openai', model: 'test-model', baseUrl: 'https://model.example.test' };
      await page.clock.fastForward(4100);
      await expect(toggle).toBeChecked();
      await expect(toggle).toHaveText('启用中…');
      await expect(status).toHaveText('启用中');
    } finally { release(); }
    await expect(toggle).toBeChecked();
    await expect(toggle).toBeEnabled();
    await expect(toggle).toHaveText('启用账户');
    await expect(toggle).toHaveAttribute('aria-busy', 'false');
    await expect(status).toHaveText(view === 'list' ? '可调度' : '已启用');
    expect(state.writes.map(item => item.path)).toEqual([`/accounts/${oauthId}/activation`]);
  });
}

for (const view of ['list', 'details'] as const) test(`automatic installation detection failure keeps the imported account disabled and retryable in ${view}`, async ({ page }) => {
  const state = await mockConsole(page);
  state.data.installation = null;
  state.data.accounts[1].account = null;
  state.data.operations.accounts[1].route = undefined;
  state.data.operations.accounts[1].enabled = false;
  let failed = true;
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  await page.route(`**/api/accounts/${oauthId}/activation`, async route => {
    if (!failed) return route.fallback();
    await gate;
    return route.fulfill({ status: 422, json: { error: { code: 'DESKTOP_SELECTION', message: '请保持官方 Codex 桌面运行后重试。' } } });
  });
  await page.goto(view === 'list' ? '/?view=accounts' : `/?view=accounts&account=${oauthId}&section=access`);
  const toggle = page.getByRole('switch', { name: '启用账户：独立测试账户', exact: true });
  await toggle.click();
  try {
    await expect(toggle).toHaveText('启用中…');
    await expect(toggle).toBeDisabled();
  } finally { release(); }
  await expect(page.getByRole('alert')).toContainText('请保持官方 Codex 桌面运行后重试');
  await expect(toggle).not.toBeChecked();
  await expect(toggle).toBeEnabled();
  await expect(toggle).toHaveText('启用账户');
  await expect(toggle).toHaveAttribute('aria-busy', 'false');
  failed = false;
  await toggle.click();
  await expect(toggle).toBeChecked();
});

test('restarting a busy instance in advanced settings requires confirmation', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.accounts[0].state = 'generating';
  state.data.operations.accounts[0].active = 1;
  await page.goto(`/?view=accounts&account=${nativeId}&section=advanced`);
  const restart = page.getByRole('button', { name: '重启实例', exact: true });
  await restart.click();
  const dialog = page.getByRole('dialog', { name: '重启“本机 Codex”？', exact: true });
  await expect(dialog).toBeVisible();
  expect(state.writes.some(item => item.path.endsWith('/restart'))).toBe(false);
  await dialog.getByRole('button', { name: '取消', exact: true }).click();
  await expect(restart).toBeFocused();
  await restart.click();
  await dialog.getByRole('button', { name: '中断当前任务并重启', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  expect(state.data.operations.accounts[0].enabled).toBe(true);
  expect(state.writes.filter(item => item.path.endsWith('/restart'))).toHaveLength(1);
});

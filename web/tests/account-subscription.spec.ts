import { test, expect } from '@playwright/test';
import { mockConsole, oauthId } from './fixtures';

test('newly authenticated accounts show subscription progress and dates arrive through polling', async ({ page }) => {
  await page.clock.install();
  const state = await mockConsole(page, { count: 2 });
  const account = state.data.accounts[1];
  account.subscription = { status: 'unknown', planType: 'plus' };
  account.account = null;
  state.data.operations.accounts[1].route!.auth = 'native-chatgpt:managed:new-login';
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${oauthId} .account-subscription-cell`);
  await expect(cell).toContainText('正在查询');
  account.subscription = { status: 'error', planType: 'plus', lastAttemptAt: Date.now(), errorCode: null };
  await page.clock.fastForward(4_000);
  await expect(cell).toContainText('正在查询');
  await expect(cell).not.toContainText('稍后重试');
  account.subscription = { status: 'error', planType: 'plus', lastAttemptAt: Date.now(), nextAttemptAt: Date.now() + 60_000, errorCode: 'SUBSCRIPTION_TIMEOUT' };
  await page.clock.fastForward(4_000);
  await expect(cell).toContainText('稍后重试');
  await cell.getByRole('button').click();
  await expect(page.getByRole('dialog', { name: '订阅详情' })).toContainText('自动重试');
  await expect(page.getByRole('dialog', { name: '订阅详情' })).not.toContainText('一天后');
  await page.keyboard.press('Escape');
  account.subscription = { status: 'current', planType: 'plus', expiresAt: Date.UTC(2030, 11, 20, 12), willRenew: true };
  await page.clock.fastForward(4_000);
  await expect(cell.locator('.subscription-date > span')).toHaveText(['到期', '2030-12-20']);
  expect(state.writes).toHaveLength(0);
});

test('subscription uses live metadata then saved credentials and does not guess unknown plans', async ({ page }) => {
  const { data, writes } = await mockConsole(page, { count: 9 });
  data.operations.accounts[0].route!.auth = 'native-api-key';
  const values = ['pro', 'pro_lite', 'plus', 'free', 'future_plan', null, null, 'constructor'];
  for (const [index, plan] of values.entries()) {
    data.accounts[index + 1].account = null;
    data.accounts[index + 1].planType = plan;
  }
  data.accounts[7].account = { type: 'chatgpt', email: null, planType: 'promax' };
  data.accounts[7].planType = 'plus';
  await page.goto('/?view=accounts');
  const badges = page.locator('.account-subscription-cell .badge');
  await expect(badges).toHaveText(['API', 'Pro', 'ProLite', 'Plus', 'Free', 'future_plan', '未知', 'ProMax', 'constructor']);
  data.accounts[1].planType = null;
  await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
  await expect(badges.nth(1)).toHaveText('未知');
  expect(writes).toEqual([]);
});

test('subscription column fits desktop and card layouts in both themes', async ({ page }, testInfo) => {
  const { data } = await mockConsole(page, { count: 3 });
  data.accounts[0].planType = 'pro';
  data.accounts[1].account!.planType = 'prolite';
  data.accounts[2].account!.planType = 'plus';
  for (const account of data.accounts) account.subscription = { status: 'current', expiresAt: Date.UTC(2030, 10, 20, 12), willRenew: true };
  await page.goto('/?view=accounts');
  for (const width of [1440, 1100, 375]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const theme of ['light', 'dark'] as const) {
      await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
      await expect(page.locator('.subscription-badge').first()).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      const cells = await page.locator('.account-console-row').first().locator(':scope > div').evaluateAll(nodes => nodes.map(node => {
        const { x, y, width, height } = node.getBoundingClientRect();
        return { x, y, width, height };
      }));
      for (let i = 0; i < cells.length; i++) for (let j = i + 1; j < cells.length; j++) {
        const a = cells[i], b = cells[j];
        expect(a.x + a.width <= b.x + 1 || b.x + b.width <= a.x + 1 || a.y + a.height <= b.y + 1 || b.y + b.height <= a.y + 1).toBe(true);
      }
      await page.screenshot({ path: testInfo.outputPath(`subscriptions-${width}-${theme}.png`), fullPage: true });
    }
  }
});

test('subscription dates are cached, accessible and refreshed only on demand', async ({ page }, testInfo) => {
  const { data } = await mockConsole(page, { count: 3 });
  const account = data.accounts[0];
  account.subscription = { status: 'stale', planType: 'pro', expiresAt: Date.UTC(2030, 10, 20, 12), willRenew: true, lastAttemptAt: Date.now() };
  data.operations.accounts[1].route!.auth = 'native-api-key';
  let refreshes = 0;
  await page.route(`**/api/accounts/${account.id}/subscription/refresh`, async route => {
    refreshes++;
    account.subscription = { ...account.subscription!, status: 'current', expiresAt: Date.UTC(2030, 11, 20, 12) };
    await route.fulfill({ json: account.subscription });
  });
  await page.goto('/?view=accounts');
  const cell = page.locator('.account-subscription-cell').first();
  await expect(cell.locator('.subscription-date > span')).toHaveText(['到期', '2030-11-20', '待确认']);
  await expect(cell).toContainText('待确认');
  await expect(page.locator('.account-subscription-cell').nth(1).locator('.subscription-date')).toHaveCount(0);
  await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
  expect(refreshes).toBe(0);
  const trigger = page.getByRole('button', { name: '本机 Codex的订阅详情' });
  await trigger.focus(); await page.keyboard.press('Enter');
  await expect(page.getByRole('dialog', { name: '订阅详情' })).toBeVisible();
  await expect(page.getByRole('dialog')).toContainText('自动续费');
  await page.screenshot({ path: testInfo.outputPath('subscription-details.png'), fullPage: true });
  await page.getByRole('button', { name: '刷新订阅', exact: true }).click();
  await expect(cell.locator('.subscription-date > span')).toHaveText(['到期', '2030-12-20']);
  expect(refreshes).toBe(1);
  await page.keyboard.press('Escape');
  await expect(trigger).toBeFocused();
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

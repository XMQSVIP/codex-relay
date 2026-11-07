import { test, expect } from '@playwright/test';
import { mockConsole, nativeId, oauthId } from './fixtures';

test('account filters, cooldown details and expanded navigation preserve state', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[1].health = { state: 'cooling', retryAt: Date.now() - 1000, consecutiveFailures: 3, reason: 'RPC_CLOSED', sharedUpstream: false, lastVerifiedAt: null };
  state.data.operations.accounts[2].enabled = false;
  await page.goto('/?view=accounts');
  await page.getByRole('combobox', { name: '筛选账户状态', exact: true }).selectOption('available');
  await expect(page.locator('.account-console-row')).toHaveCount(1);
  await page.getByRole('button', { name: '展开详情：本机 Codex' }).click();
  await expect(page.getByRole('region', { name: '本机 Codex的详情' })).toContainText('125');
  await page.getByRole('link', { name: '账户设置：本机 Codex' }).click();
  await page.getByRole('link', { name: '返回账户列表' }).click();
  await expect(page.getByRole('button', { name: '展开详情：本机 Codex' })).toHaveAttribute('aria-expanded', 'true');
  await page.getByRole('combobox', { name: '筛选账户状态', exact: true }).selectOption('limited');
  await expect(page.locator('.account-console-row')).toHaveCount(1);
  await page.getByRole('button', { name: '展开详情：独立测试账户' }).click();
  await expect(page.locator('.account-insights')).toContainText('等待恢复检查');
  await expect(page.locator('.account-insights')).toContainText('RPC_CLOSED');
  expect(state.writes).toHaveLength(0);
  expect(new URL(page.url()).searchParams.get('expanded')).toBe(oauthId);
});

for (const width of [375, 768, 1440, 2560]) for (const theme of ['dark', 'light'] as const) {
  test(`expanded accounts ${width} ${theme}`, async ({ page }, info) => {
    const state = await mockConsole(page, { longNames: true });
    state.data.operations.accounts[0].quota = { status: 'supported', checkedAt: Date.now(), stale: false, blocked: false, windows: [{ bucket: 'codex', window: 'primary', usedPercent: 25, durationMins: 300, resetsAt: Date.now() + 3600000, reached: false }, { bucket: 'codex', window: 'secondary', usedPercent: 70, durationMins: 10080, resetsAt: Date.now() + 86400000, reached: false }] };
    await page.setViewportSize({ width, height: 1100 });
    await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
    await page.goto(`/?view=accounts&expanded=${nativeId}`);
    await expect(page.locator('.account-insights')).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath('accounts.png'), fullPage: true });
  });
}

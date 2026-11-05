import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';
import type { AccountQuota } from '../src/api';

function quota(): AccountQuota {
  const now = Date.now();
  return { status: 'supported', checkedAt: now, stale: false, blocked: false, windows: [
    { bucket: 'codex', window: 'primary', durationMins: 300, usedPercent: 42, resetsAt: now + 3_600_000, reached: false, usage: { from: now - 14_400_000, to: now, totalTokens: 4_000_000, requestCount: 4, usageKnownRequests: 4 } },
    { bucket: 'codex', window: 'secondary', durationMins: 10080, usedPercent: 48, resetsAt: now + 86_400_000, reached: false, usage: { from: now - 518_400_000, to: now, totalTokens: 13_900_000, requestCount: 10, usageKnownRequests: 10 } },
  ] };
}

test('API accounts without usable quota show only unsupported while supported data stays visible', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].route!.auth = 'native-api-key';
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  for (const status of ['unknown', 'unsupported', 'error', 'supported'] as const) {
    state.data.operations.accounts[0].quota = { status, stale: true, blocked: false, windows: [] };
    await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
    await expect(cell).toHaveText('周期用量不支持');
    await expect(cell.getByRole('button')).toHaveCount(0);
  }
  state.data.operations.accounts[0].quota = quota();
  await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
  await expect(cell).toContainText('13.9M');
  await expect(cell).not.toContainText('不支持');
  state.data.operations.accounts[0].route!.auth = 'chatgpt';
  state.data.operations.accounts[0].quota = undefined;
  await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
  await expect(cell).toContainText('周期用量未知');
});

test('current cycle summary uses the longest window and hover exposes each window without adding them', async ({ page }, testInfo) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = quota();
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  await expect(cell).toContainText('13.9M');
  await expect(cell).not.toContainText('17.9M');
  await expect(cell.locator('.quota-summary-meta')).toContainText('周额度');
  await expect(cell.locator('.quota-summary-meta .quota-rate')).toHaveText('使用率 48.0%');
  await expect(page.getByRole('tooltip')).toHaveCount(0);
  await cell.getByRole('button', { name: '查看当前周期用量详情' }).hover();
  const tooltip = page.getByRole('tooltip');
  await expect(tooltip).toBeVisible();
  await expect(tooltip).toContainText('4M Token');
  await expect(tooltip).toContainText('13.9M Token');
  await expect(tooltip.getByRole('progressbar', { name: '5 小时额度使用率' })).toHaveAttribute('value', '42');
  await expect(tooltip).toContainText('重置时间');
  await tooltip.hover();
  await expect(tooltip).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('quota-usage-light.png'), animations: 'disabled' });
  await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
  await page.screenshot({ path: testInfo.outputPath('quota-usage-dark.png'), animations: 'disabled' });
  await page.keyboard.press('Escape');
  await expect(tooltip).toHaveCount(0);
  await cell.getByRole('button', { name: '查看当前周期用量详情' }).focus();
  await expect(tooltip).toBeVisible();
  await page.keyboard.press('Tab');
  await expect(tooltip).toHaveCount(0);
});

test('expired periods are not carried forward and missing usage is not displayed as zero', async ({ page }) => {
  const state = await mockConsole(page);
  const data = quota();
  data.windows[1].resetsAt = Date.now() - 1;
  data.windows[1].usedPercent = 95;
  data.windows[0].usage = null;
  data.stale = true;
  state.data.operations.accounts[0].quota = data;
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  await expect(cell.locator('.quota-summary-meta')).toContainText('5 小时额度');
  await expect(cell.locator('.quota-summary-meta .quota-rate')).toHaveText('使用率 42.0%');
  await expect(cell.locator('.quota-summary-total')).toHaveText('未知Token');
  await expect(cell.locator('.quota-mini-track').nth(1)).toHaveClass('quota-mini-track neutral');
  await cell.getByRole('button', { name: '查看当前周期用量详情' }).click();
  await expect(page.getByRole('tooltip')).toContainText('窗口已结束，等待刷新');
  await expect(page.getByRole('tooltip')).not.toContainText('13.9M');
  await expect(page.getByRole('tooltip').locator('.quota-tooltip-window').nth(1)).toHaveClass('quota-tooltip-window neutral');
});

for (const width of [375, 1440]) for (const theme of ['light', 'dark'] as const) {
  test(`low remaining quota warns for either current window in the list and details (${width} ${theme})`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    const state = await mockConsole(page);
    const data = quota();
    data.windows[0].usedPercent = 95;
    data.windows[1].usedPercent = 90;
    state.data.operations.accounts[0].quota = data;
    await page.goto('/?view=accounts');
    await page.evaluate(value => { document.documentElement.dataset.theme = value; }, theme);
    const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
    const warning = theme === 'light' ? 'rgb(135, 91, 22)' : 'rgb(237, 199, 133)';
    const error = theme === 'light' ? 'rgb(178, 54, 54)' : 'rgb(255, 175, 169)';
    await expect(cell.locator('.quota-summary-meta .quota-rate')).toHaveClass('quota-rate success');
    await expect(cell).not.toContainText('额度不足');
    await expect(cell.locator('.quota-mini-track > span').first()).toHaveCSS('background-color', warning);
    await cell.getByRole('button', { name: '查看当前周期用量详情' }).focus();
    const tooltip = page.getByRole('tooltip');
    await expect(tooltip.locator('.quota-rate').first()).toHaveCSS('color', warning);
    await expect(tooltip.getByRole('progressbar').first()).toHaveCSS('accent-color', warning);
    await page.screenshot({ path: info.outputPath(`low-quota-tooltip-${theme}.png`) });
    await page.keyboard.press('Escape');
    await page.getByRole('button', { name: '展开详情：本机 Codex' }).click();
    const panel = page.locator('.account-insights .account-quota-panel');
    await expect(panel.locator('> p.quota-rate')).toHaveText('剩余 5.0%');
    await expect(panel.locator('> p.quota-rate')).toHaveCSS('color', warning);
    await expect(panel.locator('.quota-window .quota-rate').first()).toHaveText('已用 95.0%');
    await expect(panel.locator('.quota-window .quota-rate').first()).toHaveCSS('color', warning);
    await expect(panel.getByRole('progressbar').first()).toHaveCSS('accent-color', warning);
    await expect(panel.locator('.quota-window').nth(1)).toHaveClass('quota-window success');
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath(`low-quota-details-${theme}.png`), fullPage: true });

    data.windows[0].usedPercent = 100;
    await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
    await expect(cell).not.toContainText('额度已耗尽');
    await expect(cell.locator('.quota-mini-track > span').first()).toHaveCSS('background-color', error);
    await expect(panel.getByRole('progressbar').first()).toHaveCSS('accent-color', error);

    data.windows[0].usedPercent = 90.01;
    await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
    await expect(cell.locator('.quota-mini-track > span').first()).toHaveCSS('background-color', warning);
    await expect(panel.getByRole('progressbar').first()).toHaveCSS('accent-color', warning);

    data.windows[0].usedPercent = null;
    await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
    await expect(cell.locator('.quota-mini-track').first()).toHaveClass('quota-mini-track neutral');
    await expect(panel.locator('.quota-window').first()).toHaveClass('quota-window neutral');
  });
}

test('keyboard focus replaces another account hover tooltip', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = quota();
  await page.goto('/?view=accounts');
  const triggers = page.getByRole('button', { name: '查看当前周期用量详情', exact: true });
  const tooltip = page.getByRole('tooltip');
  await triggers.nth(0).hover();
  await expect(tooltip).toContainText('13.9M Token');
  await triggers.nth(1).focus();
  await expect(tooltip).toHaveCount(1);
  await expect(tooltip).toContainText('额度未知');
  await expect(tooltip).not.toContainText('13.9M Token');
  await triggers.nth(0).focus();
  await expect(tooltip).toHaveCount(1);
  await expect(tooltip).toContainText('13.9M Token');
  await page.keyboard.press('Tab');
  await expect(tooltip).toHaveCount(0);
});

test('partial totals are identified and a zero total remains visible on mobile', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 375, height: 812 });
  const state = await mockConsole(page);
  const data = quota();
  data.windows[1].usage = { ...data.windows[1].usage!, totalTokens: 0, usageKnownRequests: 8 };
  state.data.operations.accounts[0].quota = data;
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  await expect(cell.locator('.quota-summary-total')).toHaveText('0Token（部分）');
  await cell.getByRole('button', { name: '查看当前周期用量详情' }).click();
  const tooltip = page.getByRole('tooltip');
  await expect(tooltip).toContainText('2 个请求尚无完整 Token 用量');
  await page.evaluate(() => window.scrollBy(0, 12));
  await expect(tooltip).toBeVisible();
  const bounds = await tooltip.boundingBox();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(375);
  expect(bounds!.y).toBeGreaterThanOrEqual(0);
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(812);
  await page.screenshot({ path: testInfo.outputPath('quota-usage-mobile.png'), animations: 'disabled' });
  await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
  await expect(tooltip).toHaveCount(0);
});

import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';
import type { AccountQuota } from '../src/api';

const failedQuota = (): AccountQuota => ({ status: 'error', errorCode: 'RPC_TIMEOUT', lastAttemptAt: Date.now(), stale: true, blocked: false, windows: [] });

test('quota timeout returned with HTTP 200 remains a visible failure and can recover', async ({ page }, info) => {
  await page.setViewportSize({ width: 375, height: 812 });
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = failedQuota();
  let recovered = false;
  await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => route.fulfill({ json: recovered
    ? { ...failedQuota(), status: 'supported', errorCode: null, stale: false, checkedAt: Date.now() }
    : failedQuota() }));
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  await cell.getByRole('button', { name: '查看当前周期用量详情' }).click();
  await expect(page.getByRole('tooltip')).toContainText('RPC_TIMEOUT');
  await page.keyboard.press('Escape');
  await cell.getByRole('button', { name: '刷新失败 · 查看原因', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '额度刷新详情', exact: true });
  const detail = dialog.locator('.account-quota-panel > [role="status"]');
  await expect(detail).toContainText('原生后端响应超时');
  await dialog.getByRole('button', { name: '刷新额度', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('RPC_TIMEOUT');
  await expect(dialog).not.toContainText('上游额度已刷新');
  await expect(dialog).toContainText('最近尝试：');
  await dialog.getByRole('button', { name: '关闭操作提示' }).click();
  await expect(detail).toContainText('RPC_TIMEOUT');
  await dialog.screenshot({ path: info.outputPath('quota-timeout-mobile.png') });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  recovered = true;
  await dialog.getByRole('button', { name: '刷新额度', exact: true }).click();
  await expect(dialog.getByRole('status').filter({ hasText: '上游额度已刷新' })).toBeVisible();
  await expect(detail).toHaveCount(0);
});

test('failed quota refresh identifies retained usage and opens details without another request', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = {
    ...failedQuota(), checkedAt: Date.now() - 600_000,
    windows: [{ bucket: 'codex', window: 'secondary', durationMins: 10080, usedPercent: 45, resetsAt: Date.now() + 86_400_000, reached: false }],
  };
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  await expect(cell).toContainText('使用率 45.0%');
  await expect(cell).toContainText('显示上次数据');
  await cell.getByRole('button', { name: '刷新失败 · 查看原因', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '额度刷新详情', exact: true });
  await expect(dialog).toContainText('RPC_TIMEOUT');
  await expect(dialog).toContainText('当前显示上次成功读取的数据');
  await expect(dialog.getByRole('button', { name: '刷新额度', exact: true })).toBeEnabled();
  expect(state.writes.some(write => write.path.endsWith('/quota/refresh'))).toBe(false);
  await page.keyboard.press('Escape');
  await expect(cell.getByRole('button', { name: '刷新失败 · 查看原因', exact: true })).toBeFocused();
});

test('failed refresh preserves the API error message and code after the toast is dismissed', async ({ page }) => {
  await mockConsole(page);
  await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => route.fulfill({ status: 502, json: { error: { code: 'RPC_TIMEOUT', message: '原生 getAuthStatus 请求超时，结果未知；未重发。' } } }));
  await page.goto('/?view=accounts');
  await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：未知' }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  await dialog.getByRole('button', { name: '刷新额度', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('getAuthStatus');
  await dialog.getByRole('button', { name: '关闭操作提示' }).click();
  await expect(dialog.locator('.account-quota-panel > [role="status"]')).toContainText('RPC_TIMEOUT');
  await expect(dialog).not.toContainText('上游额度已刷新');
});

for (const status of ['unsupported', 'unknown', 'supported'] as const) {
  test(`refresh does not report success for ${status} stale data`, async ({ page }) => {
    await mockConsole(page);
    await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => route.fulfill({ json: {
      ...failedQuota(), status, errorCode: status === 'unsupported' ? 'QUOTA_UNSUPPORTED' : null,
    } }));
    await page.goto('/?view=accounts');
    await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：未知' }).click();
    const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
    await dialog.getByRole('button', { name: '刷新额度', exact: true }).click();
    await expect(dialog).toContainText(status === 'unsupported' ? 'QUOTA_UNSUPPORTED' : '尚未取得新的额度数据');
    await expect(dialog).not.toContainText('上游额度已刷新');
  });
}

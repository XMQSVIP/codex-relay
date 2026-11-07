import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';
import type { AccountQuota } from '../src/api';

const failedQuota = (): AccountQuota => ({ status: 'error', errorCode: 'RPC_TIMEOUT', lastAttemptAt: Date.now(), stale: true, blocked: false, windows: [] });

for (const source of ['cell', 'expanded'] as const) {
  test(`opening reset cards from the ${source} refreshes once per opening and shares the new inventory`, async ({ page }) => {
    const state = await mockConsole(page);
    const checkedAt = Date.now();
    const initial: AccountQuota = {
      status: 'supported', checkedAt, lastAttemptAt: checkedAt, stale: false, blocked: false,
      windows: [{ bucket: 'codex', window: 'secondary', durationMins: 10080, usedPercent: 45, resetsAt: checkedAt + 86_400_000, reached: false }],
      resetContext: 'a'.repeat(64), resetCredits: { availableCount: 2, checkedAt, credits: [
        { id: 'old-card', resetType: 'codexRateLimits', status: 'available', expiresAt: checkedAt + 86_400_000 },
      ] },
    };
    state.data.operations.accounts[0].quota = initial;
    let refreshRequests = 0;
    let finishRefresh = () => {};
    await page.route(`**/api/accounts/${nativeId}/quota/refresh`, async route => {
      refreshRequests++;
      await new Promise<void>(resolve => { finishRefresh = resolve; });
      await route.fulfill({ json: {
        ...initial, checkedAt: checkedAt + refreshRequests * 100, lastAttemptAt: checkedAt + refreshRequests * 100,
        windows: [{ ...initial.windows[0], usedPercent: 25 }],
        resetCredits: { availableCount: 1, checkedAt, credits: [
          { id: 'new-card', resetType: 'codexRateLimits', status: 'available', expiresAt: checkedAt + 172_800_000 },
        ] },
      } });
    });
    await page.goto(`/?view=accounts&expanded=${nativeId}`);
    const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
    const panel = page.locator(`#insights-${nativeId} > .account-insights > .account-quota-panel`);
    await expect(panel.getByRole('button', { name: '刷新额度', exact: true })).toBeEnabled();
    expect(refreshRequests).toBe(0);
    await page.route('**/api/overview', async () => {});
    const trigger = (source === 'cell' ? cell : panel).getByRole('button', { name: /^重置卡：/ });
    const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
    for (const opening of [1, 2]) {
      await trigger.click();
      await expect.poll(() => refreshRequests).toBe(opening);
      await expect(dialog.getByRole('button', { name: '正在读取…', exact: true })).toBeDisabled();
      await expect(dialog.locator('.account-quota-panel')).toHaveAttribute('aria-busy', 'true');
      await expect(dialog.getByRole('button', { name: '使用', exact: true })).toBeDisabled();
      finishRefresh();
      await expect(dialog.getByRole('button', { name: '刷新额度', exact: true })).toBeEnabled();
      await expect(dialog.getByRole('button', { name: '使用', exact: true })).toBeEnabled();
      await expect(dialog.locator('.quota-reset-card')).toHaveCount(1);
      await expect(dialog).toContainText('重置卡：1 张');
      await expect(cell).toContainText('重置卡：1 张');
      await expect(cell).toContainText('使用率 25.0%');
      await expect(panel).toContainText('重置卡：1 张');
      await expect(panel).toContainText('已用 25.0%');
      expect(refreshRequests).toBe(opening);
      await page.keyboard.press('Escape');
      await expect(dialog).toHaveCount(0);
    }
    expect(state.writes.some(write => write.path.endsWith('/quota/reset'))).toBe(false);
  });
}

test('opening reset cards without a configured route does not refresh', async ({ page }) => {
  const state = await mockConsole(page);
  delete state.data.operations.accounts[0].route;
  await page.goto('/?view=accounts');
  await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：未知' }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  await expect(dialog.getByRole('button', { name: '刷新额度', exact: true })).toBeDisabled();
  await expect(dialog.getByRole('combobox', { name: '重置卡使用方式' })).toBeEnabled();
  expect(state.writes.some(write => write.path.endsWith('/quota/refresh'))).toBe(false);
});

test('removing the route during an opening refresh cancels the read and releases the dialog', async ({ page }) => {
  const state = await mockConsole(page);
  let finishRefresh = () => {};
  let refreshRequests = 0;
  await page.route(`**/api/accounts/${nativeId}/quota/refresh`, async route => {
    refreshRequests++;
    await new Promise<void>(resolve => { finishRefresh = resolve; });
    await route.fulfill({ json: failedQuota() });
  });
  await page.goto('/?view=accounts');
  await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：未知' }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  await expect.poll(() => refreshRequests).toBe(1);
  await expect(dialog.getByRole('button', { name: '正在读取…', exact: true })).toBeDisabled();
  const canceled = page.waitForEvent('requestfailed', request => request.url().endsWith(`/accounts/${nativeId}/quota/refresh`));
  delete state.data.operations.accounts[0].route;
  // The next overview poll delivers the configuration change while the read is pending.
  await canceled;
  await expect(dialog.getByRole('button', { name: '刷新额度', exact: true })).toBeDisabled();
  await expect(dialog.locator('.account-quota-panel')).toHaveAttribute('aria-busy', 'false');
  finishRefresh();
  await dialog.getByRole('button', { name: '关闭窗口' }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole('alert')).toHaveCount(0);
  expect(refreshRequests).toBe(1);
});

test('count-only refresh explains missing details and recovered cards survive reopening', async ({ page }) => {
  const state = await mockConsole(page);
  const checkedAt = Date.now();
  const initial: AccountQuota = {
    status: 'supported', checkedAt, lastAttemptAt: checkedAt, stale: false, blocked: false,
    windows: [{ bucket: 'codex', window: 'secondary', durationMins: 10080, usedPercent: 45, resetsAt: checkedAt + 86_400_000, reached: false }],
    resetContext: 'a'.repeat(64), resetCredits: { availableCount: 2, checkedAt, credits: null },
  };
  state.data.operations.accounts[0].quota = initial;
  let recovered = false;
  let refreshRequests = 0;
  await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => {
    refreshRequests++;
    return route.fulfill({ json: {
      ...initial, checkedAt: checkedAt + refreshRequests * 100, lastAttemptAt: checkedAt + refreshRequests * 100,
      windows: [{ ...initial.windows[0], usedPercent: recovered ? 47 : 46 }],
      resetCredits: { ...initial.resetCredits, credits: recovered ? [
        { id: 'card-one', resetType: 'codexRateLimits', status: 'available', expiresAt: checkedAt + 86_400_000 },
        { id: 'card-two', resetType: 'codexRateLimits', status: 'available', expiresAt: checkedAt + 172_800_000 },
      ] : null },
    } });
  });
  await page.goto(`/?view=accounts&expanded=${nativeId}`);
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  const panel = page.locator(`#insights-${nativeId} > .account-insights > .account-quota-panel`);
  await expect(cell).toContainText('使用率 45.0%');
  // Shared state must update from the refresh response without an overview poll.
  await page.route('**/api/overview', async () => {});
  await cell.getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  await expect(dialog.getByRole('status')).toContainText('暂未读到重置卡明细');
  await expect(dialog).toContainText('至少间隔 30 秒');
  await expect(dialog).not.toContainText('上游未提供');
  await expect(dialog).not.toContainText('上游额度已刷新');
  await dialog.getByRole('button', { name: '关闭操作提示' }).click();
  await expect(dialog).toContainText('暂未读到卡片明细');
  await page.keyboard.press('Escape');
  await expect(cell).toContainText('使用率 46.0%');
  await expect(panel).toContainText('已用 46.0%');

  recovered = true;
  await cell.getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  await expect(dialog.getByRole('status')).toContainText('上游额度已刷新');
  await expect(dialog.locator('.quota-reset-card')).toHaveCount(2);
  await expect(dialog).not.toContainText('暂未读到卡片明细');
  await page.keyboard.press('Escape');
  await expect(cell).toContainText('使用率 47.0%');
  await expect(panel).toContainText('已用 47.0%');
  await cell.getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  await expect(dialog.getByRole('button', { name: '刷新额度', exact: true })).toBeEnabled();
  await expect(dialog.locator('.quota-reset-card')).toHaveCount(2);
  expect(refreshRequests).toBe(3);
  expect(state.writes.some(write => write.path.endsWith('/quota/reset'))).toBe(false);
});

for (const credits of [null, []]) {
  test(`zero reset cards does not warn about missing details (${credits === null ? 'count only' : 'empty inventory'})`, async ({ page }) => {
    await mockConsole(page);
    await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => route.fulfill({ json: {
      status: 'supported', checkedAt: Date.now(), stale: false, blocked: false, windows: [],
      resetCredits: { availableCount: 0, checkedAt: Date.now(), credits },
    } }));
    await page.goto('/?view=accounts');
    await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：未知' }).click();
    const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
    await expect(dialog.getByRole('status')).toContainText('上游额度已刷新');
    await expect(dialog).toContainText('暂无可用重置卡。');
    await expect(dialog).not.toContainText('暂未读到');
  });
}

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

test('failed opening refresh retains cached cards and the API error until a manual retry recovers', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = {
    status: 'supported', checkedAt: Date.now(), stale: false, blocked: false, windows: [],
    resetCredits: { availableCount: 2, checkedAt: Date.now(), credits: null },
  };
  let refreshRequests = 0;
  await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => {
    refreshRequests++;
    return refreshRequests === 1
      ? route.fulfill({ status: 502, json: { error: { code: 'RPC_TIMEOUT', message: '原生 getAuthStatus 请求超时，结果未知；未重发。' } } })
      : route.fulfill({ json: { status: 'supported', checkedAt: Date.now(), stale: false, blocked: false, windows: [], resetCredits: { availableCount: 0, checkedAt: Date.now(), credits: [] } } });
  });
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  await cell.getByRole('button', { name: '重置卡：2 张' }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  await expect(dialog.getByRole('alert')).toContainText('getAuthStatus');
  await dialog.getByRole('button', { name: '关闭操作提示' }).click();
  await expect(dialog.locator('.account-quota-panel > [role="status"]')).toContainText('RPC_TIMEOUT');
  await expect(dialog).toContainText('重置卡：2 张');
  await expect(dialog).toContainText('当前显示上次成功读取的数据');
  await expect(dialog).not.toContainText('上游额度已刷新');
  await dialog.getByRole('button', { name: '刷新额度', exact: true }).click();
  await expect(dialog).toContainText('暂无可用重置卡。');
  await expect(dialog.locator('.account-quota-panel > [role="status"]')).toHaveCount(0);
  await expect(cell).toContainText('重置卡：0 张');
  expect(refreshRequests).toBe(2);
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
    await expect(dialog).toContainText(status === 'unsupported' ? 'QUOTA_UNSUPPORTED' : '尚未取得新的额度数据');
    await expect(dialog).not.toContainText('上游额度已刷新');
  });
}

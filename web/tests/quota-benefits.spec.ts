import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';
import type { AccountQuota } from '../src/api';
const quota = (): AccountQuota => ({ status: 'supported', checkedAt: Date.now(), lastAttemptAt: Date.now(), stale: false, blocked: false, windows: [], resetContext: 'a'.repeat(64), credits: { balance: '9007199254740993.123456789', hasCredits: true, unlimited: false, checkedAt: Date.now() }, resetCredits: { availableCount: 2, checkedAt: Date.now(), credits: [{ id: 'card-1', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() + 86400000 }] } });

for (const width of [390, 1440]) {
  test(`reset cards default to manual and auto mode persists without hiding manual use ${width}`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    const state = await mockConsole(page);
    state.data.operations.accounts[0].quota = quota();
    await page.goto(`/?view=accounts&expanded=${nativeId}`);
    await expect(page.getByRole('combobox', { name: '重置卡使用方式' })).toHaveCount(0);
    await page.locator(`#account-${nativeId} .account-quota-cell`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
    const mode = page.getByRole('combobox', { name: '重置卡使用方式' });
    await expect(mode).toBeEnabled();
    await expect(mode).toHaveValue('manual');
    await mode.selectOption('auto');
    await expect(mode).toHaveValue('auto');
    await expect(page.getByRole('button', { name: '使用', exact: true })).toBeEnabled();
    await page.reload();
    await page.locator(`#account-${nativeId} .account-quota-cell`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
    await expect(mode).toHaveValue('auto');
    await page.getByRole('dialog', { name: '重置卡', exact: true }).screenshot({ path: info.outputPath('reset-automation.png') });
    await mode.selectOption('manual');
    await expect(mode).toHaveValue('manual');
    expect(state.writes.filter(w => w.path.endsWith('/quota/reset'))).toHaveLength(0);
    expect(state.writes.filter(w => w.path.endsWith('/automation')).map(w => w.body.enabled)).toEqual([true, false]);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
}

test('balances and reset cards are visible in the collapsed second column', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = quota();
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} > div`).nth(1);
  await expect(cell).toContainText('9007199254740993.123456789 点');
  await expect(cell).toContainText('重置卡：2 张');
  await expect(page.locator('.account-insights')).toHaveCount(0);
});

test('reset requires confirmation and uncertain retries reuse the same key after reopening', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = quota();
  const requests: { context: string; idempotencyKey: string }[] = [];
  await page.route(`**/api/accounts/${nativeId}/quota/reset`, route => {
    requests.push(route.request().postDataJSON());
    if (requests.length === 1) return route.abort('failed');
    return route.fulfill({ json: { outcome: 'alreadyRedeemed', refreshed: true, quota: { ...quota(), checkedAt: Date.now() + 100, lastAttemptAt: Date.now() + 100, resetCredits: { availableCount: 1, checkedAt: Date.now(), credits: null } } } });
  });
  await page.goto(`/?view=accounts&expanded=${nativeId}`);
  await page.locator(`#account-${nativeId} .account-quota-cell`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  await page.getByRole('button', { name: '使用', exact: true }).click();
  expect(requests).toHaveLength(0);
  await page.getByRole('button', { name: '确认使用一张' }).click();
  await expect(page.getByRole('dialog').getByRole('alert')).toBeVisible();
  await page.keyboard.press('Escape');
  await page.reload();
  await page.locator(`#account-${nativeId} .account-quota-cell`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  await page.getByRole('button', { name: '核验上次重置', exact: true }).click();
  await page.getByRole('button', { name: '重试同一次重置' }).click();
  await expect(page.getByRole('status').filter({ hasText: '没有重复消耗' })).toBeVisible();
  await expect(page.getByRole('dialog', { name: '重置卡', exact: true })).toContainText('重置卡：1 张');
  expect(requests).toHaveLength(2);
  expect(requests[1]).toEqual(requests[0]);
  expect(await page.evaluate(id => sessionStorage.getItem(`quota-reset:${id}`), nativeId)).toBeNull();
});

test('zero and unknown differ, stale cards cannot start a new reset', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = { ...quota(), stale: true, credits: { balance: '0', hasCredits: false, unlimited: false, checkedAt: Date.now() } };
  await page.goto(`/?view=accounts&expanded=${nativeId}`);
  await expect(page.locator('.account-quota-panel')).toContainText('点数：0 点');
  await page.locator(`#account-${nativeId} .account-quota-cell`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  await expect(page.getByRole('button', { name: '使用', exact: true })).toBeDisabled();
  await expect(page.locator(`#account-${state.data.accounts[1].id} .account-quota-cell`)).toContainText('重置卡：未知');
});


test('each card has an action and only the chosen card is redeemed', async ({ page }, info) => {
  const state = await mockConsole(page);
  const value = quota();
  value.resetCredits!.credits = [
    { id: 'card-later', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() + 86_400_000 },
    { id: 'card-soon', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() + 120_000 },
    { id: 'card-used', resetType: 'codexRateLimits', status: 'redeemed', expiresAt: Date.now() + 86_400_000 },
    { id: 'card-expired', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() - 1000 },
    { id: 'card-other', resetType: 'other', status: 'available', expiresAt: Date.now() + 86_400_000 },
  ];
  state.data.operations.accounts[0].quota = value;
  const requests: { creditId: string; idempotencyKey: string }[] = [];
  await page.route(`**/api/accounts/${nativeId}/quota/reset`, route => {
    requests.push(route.request().postDataJSON());
    return route.fulfill({ json: { outcome: 'reset', refreshed: true, quota: { ...value, lastAttemptAt: Date.now() + 100, resetCredits: { ...value.resetCredits, availableCount: 1, credits: value.resetCredits!.credits!.filter(c => c.id !== 'card-later') } } } });
  });
  await page.goto('/?view=accounts');
  await expect(page.getByRole('combobox', { name: '重置卡使用方式' })).toHaveCount(0);
  await page.locator(`#account-${nativeId} .account-quota-cell`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  const first = dialog.getByRole('group', { name: 'Codex 重置卡 1', exact: true });
  const second = dialog.getByRole('group', { name: 'Codex 重置卡 2', exact: true });
  await expect(dialog.getByRole('radio')).toHaveCount(0);
  await expect(dialog.locator('.quota-reset-card')).toHaveCount(2);
  await expect(first.getByRole('button', { name: '使用', exact: true })).toBeEnabled();
  await expect(second.getByRole('button', { name: '使用', exact: true })).toBeEnabled();
  await expect(dialog).not.toContainText('不可用');
  await expect(dialog).not.toContainText('已过期');
  await expect(dialog).not.toContainText('其他类型重置卡');
  await expect(dialog).not.toContainText('card-later');
  await first.getByRole('button', { name: '使用', exact: true }).click();
  await expect(first.getByRole('button', { name: '确认使用一张' })).toBeVisible();
  await expect(second.getByRole('button', { name: '确认使用一张' })).toHaveCount(0);
  await dialog.screenshot({ path: info.outputPath('multiple-reset-cards.png') });
  await dialog.getByRole('button', { name: '确认使用一张', exact: true }).click();
  await expect(dialog).toContainText('重置卡：1 张');
  await expect(dialog.locator('.quota-reset-card')).toHaveCount(1);
  expect(requests).toHaveLength(1);
  expect(requests[0].creditId).toBe('card-later');
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole('combobox', { name: '重置卡使用方式' })).toHaveCount(0);
});

for (const width of [375, 1200]) {
  test(`card actions hide upstream IDs and preserve the failed attempt at ${width}px`, async ({ page }, info) => {
    const state = await mockConsole(page);
    const value = quota();
    const privateId = 'RateLimitResetCredit_d03029eb9e70819194ebbabc9cd0120b';
    value.resetCredits!.credits = [
      { id: privateId, resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() + 86400000 },
      { id: 'second-card', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() + 86400000 },
    ];
    state.data.operations.accounts[0].quota = value;
    const requests: unknown[] = [];
    await page.route(`**/api/accounts/${nativeId}/quota/reset`, route => {
      requests.push(route.request().postDataJSON());
      return requests.length === 1 ? route.abort() : route.fulfill({ json: { outcome: 'alreadyRedeemed', refreshed: true, quota: { ...value, lastAttemptAt: Date.now() + 100, resetCredits: { ...value.resetCredits, availableCount: 1, credits: value.resetCredits!.credits!.slice(1) } } } });
    });
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/?view=accounts');
    await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
    const first = dialog.getByRole('group', { name: 'Codex 重置卡 1', exact: true });
    const second = dialog.getByRole('group', { name: 'Codex 重置卡 2', exact: true });
    await expect(dialog).not.toContainText(privateId);
    await first.getByRole('button', { name: '使用', exact: true }).click();
    await first.getByRole('button', { name: '确认使用一张' }).click();
    await expect(first.getByRole('button', { name: '重试同一次重置' })).toBeEnabled();
    await expect(second.getByRole('button', { name: '使用', exact: true })).toBeDisabled();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
    await dialog.screenshot({ path: info.outputPath('card-actions.png') });
    await first.getByRole('button', { name: '重试同一次重置' }).click();
    await expect(dialog).toContainText('重置卡：1 张');
    expect(requests).toHaveLength(2);
    expect(requests[1]).toEqual(requests[0]);
    expect((requests[0] as { creditId: string }).creditId).toBe(privateId);
  });
}

test('when card details disappear a fresh request lets the upstream choose the next card', async ({ page }) => {
  const state = await mockConsole(page);
  const value = quota();
  state.data.operations.accounts[0].quota = value;
  const requests: { creditId?: string; idempotencyKey: string }[] = [];
  await page.route(`**/api/accounts/${nativeId}/quota/reset`, route => {
    requests.push(route.request().postDataJSON());
    return route.fulfill({ json: { outcome: 'reset', refreshed: true, quota: { ...value, lastAttemptAt: Date.now() + requests.length * 100, resetCredits: { ...value.resetCredits, credits: null, availableCount: 1 } } } });
  });
  await page.goto('/?view=accounts');
  await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  await dialog.getByRole('button', { name: '使用', exact: true }).click();
  await dialog.getByRole('button', { name: '确认使用一张', exact: true }).click();
  await expect(dialog).toContainText('暂未读到卡片明细');
  await dialog.getByRole('button', { name: '使用', exact: true }).click();
  await dialog.getByRole('button', { name: '确认使用一张', exact: true }).click();
  await expect.poll(() => requests.length).toBe(2);
  expect(requests[0].creditId).toBe('card-1');
  expect(requests[1].creditId).toBeUndefined();
  expect(requests[1].idempotencyKey).not.toBe(requests[0].idempotencyKey);
});

for (const empty of [false, true]) {
  test(`no available cards shows an empty state without a use action (empty inventory: ${empty})`, async ({ page }) => {
    const state = await mockConsole(page);
    const value = quota();
    value.resetCredits = { availableCount: 0, checkedAt: Date.now(), credits: empty ? [] : [
      { id: 'used', resetType: 'codexRateLimits', status: 'redeemed', expiresAt: null },
      { id: 'expired', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() - 1000 },
      { id: 'other', resetType: 'other', status: 'available', expiresAt: null },
    ] };
    state.data.operations.accounts[0].quota = value;
    await page.goto('/?view=accounts');
    await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：0 张', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
    await expect(dialog).toContainText('暂无可用重置卡。');
    await expect(dialog.locator('.quota-reset-card')).toHaveCount(0);
    await expect(dialog.getByRole('button', { name: '使用', exact: true })).toHaveCount(0);
    await expect(dialog).not.toContainText('暂未读到卡片明细');
  });
}

test('a hidden redeemed card still allows verification with the original request', async ({ page }) => {
  const state = await mockConsole(page);
  const value = quota();
  value.resetCredits = { availableCount: 0, checkedAt: Date.now(), credits: [
    { id: 'used', resetType: 'codexRateLimits', status: 'redeemed', expiresAt: null },
  ] };
  state.data.operations.accounts[0].quota = value;
  const attempt = { context: value.resetContext, idempotencyKey: 'same-request', creditId: 'used' };
  await page.addInitScript(({ id, attempt }) => sessionStorage.setItem(`quota-reset:${id}`, JSON.stringify(attempt)), { id: nativeId, attempt });
  const requests: unknown[] = [];
  await page.route(`**/api/accounts/${nativeId}/quota/reset`, route => {
    requests.push(route.request().postDataJSON());
    return route.fulfill({ json: { outcome: 'alreadyRedeemed', refreshed: true, quota: value } });
  });
  await page.goto('/?view=accounts');
  await page.locator(`#account-${nativeId}`).getByRole('button', { name: '重置卡：0 张', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
  await expect(dialog.locator('.quota-reset-card')).toHaveCount(0);
  await dialog.getByRole('button', { name: '核验上次重置', exact: true }).click();
  await dialog.getByRole('button', { name: '重试同一次重置', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: '没有重复消耗' })).toBeVisible();
  expect(requests).toEqual([attempt]);
  await expect(dialog.getByRole('button', { name: '核验上次重置', exact: true })).toHaveCount(0);
});

for (const source of ['cell', 'expanded'] as const) {
  test(`reset immediately updates the quota in the ${source} dialog, account row and expanded panel`, async ({ page }) => {
    const state = await mockConsole(page);
    const value = quota();
    value.windows = [{ bucket: 'codex', window: 'secondary', durationMins: 10080, usedPercent: 100, resetsAt: Date.now() + 86_400_000, reached: true }];
    state.data.operations.accounts[0].quota = value;
    const refreshed = { ...value, checkedAt: Date.now() + 100, lastAttemptAt: Date.now() + 100,
      windows: [{ ...value.windows[0], usedPercent: 0, reached: false }],
      resetCredits: { availableCount: 0, checkedAt: Date.now() + 100, credits: [] },
    };
    let resetRequests = 0;
    let refreshRequests = 0;
    await page.route(`**/api/accounts/${nativeId}/quota/reset`, route => {
      resetRequests++;
      return route.fulfill({ json: { outcome: 'reset', refreshed: true, quota: refreshed } });
    });
    await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => {
      refreshRequests++;
      return route.fulfill({ json: value });
    });
    await page.goto(`/?view=accounts&expanded=${nativeId}`);
    const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
    const panel = page.locator(`#insights-${nativeId} > .account-insights > .account-quota-panel`);
    await expect(cell).toContainText('使用率 100.0%');
    await expect(panel).toContainText('已用 100.0%');
    // No overview response after the initial load can update the shared quota.
    await page.route('**/api/overview', async () => {});
    await (source === 'cell' ? cell : panel).getByRole('button', { name: '重置卡：2 张', exact: true }).click();
    const dialog = page.getByRole('dialog', { name: '重置卡', exact: true });
    await dialog.getByRole('button', { name: '使用', exact: true }).click();
    await dialog.getByRole('button', { name: '确认使用一张', exact: true }).click();
    await expect(dialog).toContainText('已用 0.0%');
    await expect(dialog).toContainText('暂无可用重置卡。');
    await expect(cell).toContainText('使用率 0.0%');
    await expect(cell).toContainText('重置卡：0 张');
    await expect(panel).toContainText('已用 0.0%');
    await expect(panel).toContainText('重置卡：0 张');
    expect(resetRequests).toBe(1);
    // Opening refreshes once; the reset response supplies the post-reset quota reading.
    expect(refreshRequests).toBe(1);
    await page.keyboard.press('Escape');
    await expect(cell).toContainText('使用率 0.0%');
  });
}

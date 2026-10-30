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
    await expect(page.getByRole('button', { name: '使用重置卡', exact: true })).toBeEnabled();
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
  await page.getByRole('button', { name: '使用重置卡', exact: true }).click();
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
  await expect(page.getByRole('button', { name: '使用重置卡', exact: true })).toBeDisabled();
  await expect(page.locator(`#account-${state.data.accounts[1].id} .account-quota-cell`)).toContainText('重置卡：未知');
});


test('multiple cards are selectable in the dialog and only the chosen card is redeemed', async ({ page }, info) => {
  const state = await mockConsole(page);
  const value = quota();
  value.resetCredits!.credits = [
    { id: 'card-later', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() + 86_400_000 },
    { id: 'card-soon', resetType: 'codexRateLimits', status: 'available', expiresAt: Date.now() + 120_000 },
    { id: 'card-used', resetType: 'codexRateLimits', status: 'redeemed', expiresAt: Date.now() + 86_400_000 },
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
  await expect(dialog.getByRole('radio', { name: '选择重置卡 card-soon', exact: true })).toBeChecked();
  await expect(dialog.getByRole('radio', { name: '选择重置卡 card-used', exact: true })).toBeDisabled();
  await dialog.getByRole('radio', { name: '选择重置卡 card-later', exact: true }).check();
  await dialog.screenshot({ path: info.outputPath('multiple-reset-cards.png') });
  await dialog.getByRole('button', { name: '使用重置卡', exact: true }).click();
  await expect(dialog).toContainText('选中的重置卡（card-later）');
  await dialog.getByRole('button', { name: '确认使用一张', exact: true }).click();
  await expect(dialog).toContainText('重置卡：1 张');
  expect(requests).toHaveLength(1);
  expect(requests[0].creditId).toBe('card-later');
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole('combobox', { name: '重置卡使用方式' })).toHaveCount(0);
});

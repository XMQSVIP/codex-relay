import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';

test('selection persists across pages and clears when filters change', async ({ page }) => {
  await mockConsole(page, { count: 25 });
  await page.goto('/?view=accounts');
  await expect(page.getByRole('link', { name: '管理分组', exact: true })).toHaveCount(0);
  await page.locator('.account-column-headings').getByRole('checkbox', { name: '选择当前页全部账号' }).check();
  await expect(page.getByText('已选 20 个', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '下一页' }).click();
  await expect(page.getByText('已选 20 个', { exact: true })).toBeVisible();
  await page.locator('.account-column-headings').getByRole('checkbox', { name: '选择当前页全部账号' }).check();
  await expect(page.getByText('已选 25 个', { exact: true })).toBeVisible();
  await page.getByRole('combobox', { name: '筛选账户状态', exact: true }).selectOption('available');
  await expect(page.getByRole('button', { name: '更多', exact: true })).toHaveCount(0);
});

test('batch configuration submits only modified fields and renders direct results', async ({ page }) => {
  await mockConsole(page); let payload: any;
  await page.route('**/api/accounts/batch', async route => {
    expect(route.request().method()).toBe('POST');
    payload = route.request().postDataJSON();
    return route.fulfill({ json: { action: 'configure', status: 'completed', items: [{ accountId: nativeId, status: 'succeeded' }] } });
  });
  await page.goto('/?view=accounts');
  await page.getByRole('checkbox', { name: '选择账号：本机 Codex', exact: true }).check();
  await expect(page.getByRole('checkbox', { name: '选择当前页全部账号' })).toBeChecked({ indeterminate: true });
  const more = page.getByRole('button', { name: '更多', exact: true });
  await expect(page.getByRole('button', { name: '批量配置', exact: true })).toHaveCount(0);
  await more.focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('button', { name: '批量删除', exact: true })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(more).toBeFocused();
  await expect(more).toHaveAttribute('aria-expanded', 'false');
  await more.click();
  await page.getByRole('button', { name: '批量配置', exact: true }).click();
  await page.getByRole('spinbutton', { name: '账户并发上限', exact: true }).fill('7');
  await page.getByRole('button', { name: '提交配置' }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.locator('.batch-result')).toContainText('成功 1，失败 0，跳过 0');
  expect(payload).toEqual({ revision: 1, action: 'configure', accountIds: [nativeId], maxConcurrency: 7 });
});

test('quota is separate from token usage and unknown errors retain old data', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = { status: 'supported', checkedAt: Date.now(), stale: false, blocked: true, retryAt: Date.now() + 60_000, windows: [{ bucket: 'codex', window: 'primary', durationMins: 300, usedPercent: 100, resetsAt: Date.now() + 60_000, reached: true }] };
  await page.goto(`/?view=accounts&expanded=${nativeId}`);
  await page.getByRole('combobox', { name: '筛选账户状态', exact: true }).selectOption('limited');
  await expect(page.locator('.account-console-row')).toHaveCount(1);
  await expect(page.locator('.account-quota-panel')).toContainText('5 小时窗口');
  await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => route.fulfill({ status: 500, json: { error: { code: 'QUOTA_TIMEOUT', message: '测试超时' } } }));
  await page.getByRole('button', { name: '刷新额度' }).click();
  await expect(page.getByRole('alert')).toContainText('测试超时（QUOTA_TIMEOUT）');
  await expect(page.locator('.account-quota-panel')).toContainText('已用 100.0%');
});

test('quota summary and window details are available without expanding account rows', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].quota = { status: 'supported', checkedAt: Date.now(), stale: true, blocked: false, windows: [{ bucket: 'codex', window: 'primary', usedPercent: 13, durationMins: 300, resetsAt: Date.now() + 3600000, reached: false }, { bucket: 'codex', window: 'secondary', usedPercent: 40, durationMins: 10080, resetsAt: Date.now() + 86400000, reached: false }] };
  await page.goto('/?view=accounts');
  const cell = page.locator(`#account-${nativeId} .account-quota-cell`);
  await expect(cell.locator('.quota-summary-meta > span').first()).toHaveText('周额度');
  await expect(cell).toContainText('使用率 40.0%');
  await expect(cell).not.toContainText('数据已过期');
  await cell.getByRole('button', { name: '查看当前周期用量详情' }).hover();
  const tooltip = page.getByRole('tooltip');
  await expect(tooltip).toContainText('13.0%');
  await expect(tooltip).toContainText('40.0%');
  await expect(tooltip).toContainText('5 小时额度');
  await expect(tooltip).toContainText('周额度');
  await expect(tooltip).toContainText('重置时间');
  await expect(page.locator('.account-insights')).toHaveCount(0);
  await page.goto(`/?view=accounts&expanded=${nativeId}`);
  await expect(page.locator('.account-quota-panel')).toContainText('codex · 周窗口');
  await expect(page.locator('.account-quota-panel')).toContainText('codex · 5 小时窗口');
});

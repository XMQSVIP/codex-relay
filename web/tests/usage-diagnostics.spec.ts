import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';

test('ranking diagnostics distinguish known inference samples from management and unknown data', async ({ page }) => {
  const state = await mockConsole(page);
  const source = state.records[0];
  state.records.push({ ...source, id: 'slow-failure', status: 'failed', inputTokens: 900, cachedInputTokens: 0, firstDeltaMs: 950, waitMs: 50, totalMs: 2500 });
  state.records.push({ ...source, id: 'unreported', inputTokens: 10000, cachedInputTokens: null, firstDeltaMs: null, totalMs: null });
  state.records.push({ ...source, id: 'management-failure', kind: 'management', status: 'failed', cachedInputTokens: 9999, firstDeltaMs: 50000, totalMs: 50000 });
  state.records.push({ ...source, id: 'unknown-key', keyId: 'unknown', keyName: '未报告用量的密钥', accountId: null, model: 'unknown-model', inputTokens: null, outputTokens: null, cachedInputTokens: null, firstDeltaMs: null, totalMs: null });
  await page.goto('/?view=statistics');
  await expect(page.locator('.usage-response-latency b')).toHaveText(['625 ms', '575 ms']);
  const ranking = page.getByRole('region', { name: '用量排行', exact: true });
  for (const dimension of ['API 密钥', '账户', '模型']) {
    await ranking.getByRole('button', { name: dimension, exact: true }).click();
    const row = ranking.locator('tbody tr').first();
    await expect(row.locator('.usage-failures')).toHaveText(dimension === '模型' ? '1' : '2');
    await expect(row.locator('.usage-performance')).toContainText('首响 625 ms');
    await expect(row.locator('.usage-performance')).toContainText('模型首响 575 ms');
    await expect(row.locator('.usage-performance')).toContainText('总耗时 2 s');
    await expect(row.locator('.usage-cache strong')).toHaveText('5%');
    await expect(row.locator('.usage-cache small')).toHaveText('2 次缓存用量已知');
  }
  await ranking.getByRole('button', { name: 'API 密钥', exact: true }).click();
  const unknown = ranking.locator('tbody tr').filter({ hasText: '未报告用量的密钥' });
  await expect(unknown.locator('.usage-cache strong')).toHaveText('未知');
  await expect(unknown.locator('.usage-performance')).toHaveText('首响 —模型首响 —总耗时 —');
});

test('live scheduling capacity remains global across historical and account filters and handles zero capacity', async ({ page }) => {
  const state = await mockConsole(page);
  Object.assign(state.data.operations, { active: 2, queued: 3, globalLimitAuto: false, manualGlobalLimit: 8 });
  await page.goto(`/?view=statistics&startDate=2026-10-01&endDate=2026-10-01&accountId=${nativeId}`);
  const scheduling = page.getByRole('region', { name: '账户调度', exact: true });
  await expect(page.locator('.usage-coverage')).toContainText('当前范围没有模型请求');
  await expect(scheduling.getByRole('progressbar', { name: '全局并发占用' })).toHaveAttribute('aria-valuetext', '2 / 8');
  await expect(scheduling.locator('.usage-scheduling-counts dd')).toHaveText(['8', '6', '3']);
  await expect(scheduling.locator('.usage-scheduling-mode dd')).toHaveText(['手动设置', '15']);
  Object.assign(state.data.operations, { active: 0, queued: 0, globalLimitAuto: true });
  state.data.operations.accounts.forEach(account => { account.maxConcurrency = 0; });
  await page.getByRole('button', { name: '刷新状态', exact: true }).click();
  await expect(scheduling.getByRole('progressbar')).toHaveAttribute('aria-valuetext', '0 / 0');
  await expect(scheduling.locator('.usage-scheduling-counts dd')).toHaveText(['0', '0', '0']);
  await expect(scheduling.locator('.usage-scheduling-mode dd').first()).toHaveText('随账户容量调整');
  await scheduling.getByRole('link', { name: '管理账户与并发', exact: true }).click();
  await expect(page).toHaveURL(/view=accounts/);
});

test('model first response keeps zero and unknown distinct without subtracting unrelated waits', async ({ page }) => {
  const state = await mockConsole(page);
  const source = state.records[0];
  Object.assign(source, { firstDeltaMs: 0, waitMs: 1000 });
  state.records.push({ ...source, id: 'unknown-first-output', firstDeltaMs: null, waitMs: 5000 });
  await page.goto('/?view=statistics');
  await expect(page.locator('.usage-response-latency b')).toHaveText(['1 s', '0 ms']);
  await expect(page.locator('.usage-performance')).toContainText('模型首响 0 ms');
  source.firstDeltaMs = null;
  await page.getByRole('button', { name: '刷新用量统计', exact: true }).click();
  await expect(page.locator('.usage-response-latency b')).toHaveText(['—', '—']);
  await expect(page.locator('.usage-performance')).toContainText('模型首响 —');
});

test('compact filters apply to visible model success and failure counts and return keyboard focus', async ({ page }) => {
  const state = await mockConsole(page);
  const source = state.records[0];
  for (const status of ['failed', 'queued', 'cancelled'] as const) state.records.push({ ...source, id: status, status });
  state.records.push({ ...source, id: 'management-failure', kind: 'management', status: 'failed' });
  await page.goto('/?view=statistics');
  const requests = page.locator('.usage-metric-card').first();
  await expect(requests.locator('.usage-metric-copy > strong')).toHaveText('4');
  await expect(requests.locator('.usage-request-success b')).toHaveText('1');
  await expect(requests.locator('.usage-request-failed b')).toHaveText('1');
  await expect(page.getByText('筛选统计', { exact: true })).toHaveCount(0);
  const filters = page.getByRole('region', { name: '统计筛选', exact: true });
  await expect(filters).not.toBeVisible();
  await page.getByRole('button', { name: '筛选', exact: true }).click();
  await filters.getByLabel('请求状态', { exact: true }).selectOption('failed');
  await expect(requests.locator('.usage-metric-copy > strong')).toHaveText('1');
  await expect(requests.locator('.usage-request-success b')).toHaveText('0');
  await expect(requests.locator('.usage-request-failed b')).toHaveText('1');
  await filters.getByLabel('请求状态', { exact: true }).focus();
  await page.keyboard.press('Escape');
  await expect(filters).not.toBeVisible();
  const toggle = page.getByRole('button', { name: '筛选，1 项已应用', exact: true });
  await expect(toggle).toBeFocused();
  await toggle.click();
  await filters.getByRole('button', { name: '清除筛选', exact: true }).click();
  await expect(requests.locator('.usage-metric-copy > strong')).toHaveText('4');
  await expect(page.getByRole('button', { name: '筛选', exact: true })).toHaveAttribute('aria-expanded', 'true');
});

import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

test('request details show adaptive scheduling scores and sticky fallback', async ({ page }) => {
  const state = await mockConsole(page);
  state.records[0].scheduling = { version: 'adaptive_v1', mode: 'balanced', candidateCount: 3, score: 0.805, components: { load: 1, quota: 0.5, health: 0.9, latency: 0.5 }, affinity: 'scoreGap' };
  await page.goto('/?view=requests');
  await page.getByRole('button', { name: /^查看请求详情/ }).first().click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('综合选择', { exact: true })).toBeVisible();
  await dialog.getByText('调度说明', { exact: true }).click();
  await expect(dialog.getByText('评分差距超过 0.08', { exact: true })).toBeVisible();
  await expect(dialog.getByText('0.805', { exact: true })).toBeVisible();
  await expect(dialog.getByText('稳定性（20%）', { exact: true })).toBeVisible();
});

test('records without a scheduling decision display unknown', async ({ page }) => {
  const state = await mockConsole(page);
  state.records[0].scheduling = null;
  await page.goto('/?view=requests');
  await page.getByRole('button', { name: /^查看请求详情/ }).first().click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.locator('.request-fields > div').filter({ has: page.getByText('账户调度', { exact: true }) })).toHaveText('账户调度未知');
  await expect(dialog.getByText('调度说明', { exact: true })).toHaveCount(0);
});

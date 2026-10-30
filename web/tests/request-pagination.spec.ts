import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

test('request records default to 20 per page and reset paging when filters or size change', async ({ page }) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  for (let i = 1; i < 45; i++) state.records.push({ ...record, id: `page-row-${i}`, status: i % 2 ? 'failed' : 'succeeded' });
  await page.goto('/?view=requests');
  const pager = page.getByRole('navigation', { name: '请求记录分页' });
  const rows = page.locator('.records-table tbody tr');
  await expect(rows).toHaveCount(20);
  await expect(pager).toContainText('第 1 / 3 页 · 共 45 条');
  expect(state.statisticsQueries.at(-1)?.get('pageSize')).toBe('20');
  await pager.getByRole('button', { name: '下一页' }).click();
  await expect(pager).toContainText('第 2 / 3 页');
  await expect(rows).toHaveCount(20);
  await pager.getByRole('button', { name: '下一页' }).click();
  await expect(rows).toHaveCount(5);
  await expect(pager.getByRole('button', { name: '下一页' })).toBeDisabled();
  await pager.getByLabel('每页条数').selectOption('50');
  await expect(rows).toHaveCount(45);
  await expect(pager).toContainText('第 1 / 1 页');
  await pager.getByLabel('每页条数').selectOption('20');
  await pager.getByRole('button', { name: '下一页' }).click();
  await expect(page.getByText('筛选统计', { exact: true })).toHaveCount(0);
  await page.getByLabel('筛选字段', { exact: true }).selectOption('status');
  await page.getByLabel('请求状态筛选内容').fill('failed');
  await page.getByRole('button', { name: '搜索请求记录', exact: true }).click();
  await expect(pager).toContainText('第 1 / 2 页 · 共 22 条');
  await page.setViewportSize({ width: 375, height: 844 });
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1);
  await pager.screenshot({ path: '../.cache/request-pagination-mobile.png' });
});

test('pagination remains visible for empty records and out-of-range pages recover', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=requests&page=999');
  const pager = page.getByRole('navigation', { name: '请求记录分页' });
  await expect(pager).toContainText('第 1 / 1 页 · 共 1 条');
  await expect(pager.getByRole('button', { name: '上一页' })).toBeDisabled();
  state.records.splice(0);
  await page.getByRole('button', { name: '刷新请求记录', exact: true }).click();
  await expect(pager).toContainText('第 1 / 1 页 · 共 0 条');
  await expect(pager.getByRole('button', { name: '下一页' })).toBeDisabled();
});


test('key name search filters all pages and can be cleared', async ({ page }) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  state.records.splice(0);
  for (let i = 0; i < 45; i++) state.records.push({ ...record, id: `key-row-${i}`, keyName: i < 22 ? '历史密钥%_名称' : '其他密钥' });
  await page.goto('/?view=requests&page=2');
  const pager = page.getByRole('navigation', { name: '请求记录分页' });
  await expect(page.getByLabel('筛选字段', { exact: true })).toHaveValue('keyName');
  await page.getByLabel('密钥名称筛选内容').fill('%_');
  await page.getByLabel('密钥名称筛选内容').press('Enter');
  await expect(pager).toContainText('第 1 / 2 页 · 共 22 条');
  expect(state.statisticsQueries.at(-1)?.get('keyName')).toBe('%_');
  await pager.getByRole('button', { name: '下一页' }).click();
  await expect(page.locator('.records-table tbody tr')).toHaveCount(2);
  await page.screenshot({ path: '../.cache/request-filters-desktop.png' });
  await page.getByLabel('已应用筛选', { exact: true }).click();
  await page.getByRole('button', { name: '移除密钥名称筛选' }).click();
  await expect(pager).toContainText('第 1 / 3 页 · 共 45 条');
});

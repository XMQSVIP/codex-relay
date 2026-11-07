import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';

test('configured accounts can save parallel request capacity and validate limits', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  const limit = page.getByLabel('账户并发上限', { exact: true });
  await expect(limit).toBeEnabled();
  await limit.fill('4');
  await page.getByRole('button', { name: '保存调度', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: '账户调度已保存' })).toBeVisible();
  expect(state.writes.find(item => item.path === `/accounts/${nativeId}/policy`)?.body.maxConcurrency).toBe(4);
  await page.reload();
  await expect(limit).toHaveValue('4');
  for (const invalid of ['0', '257', '1.5', '']) {
    await limit.fill(invalid);
    await expect(page.getByRole('button', { name: '保存调度', exact: true })).toBeDisabled();
  }
});

test('lowering capacity during execution saves without stopping active requests', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].maxConcurrency = 4;
  state.data.operations.accounts[0].active = 3;
  state.data.accounts[0].state = 'generating';
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  await page.getByLabel('账户并发上限', { exact: true }).fill('2');
  await page.getByRole('button', { name: '保存调度', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: '账户调度已保存' })).toBeVisible();
  expect(state.data.operations.accounts[0].active).toBe(3);
  expect(state.writes.map(item => item.path)).toEqual([`/accounts/${nativeId}/policy`]);
});

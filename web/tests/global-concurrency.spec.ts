import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';

test('automatic capacity follows accounts and manual capacity persists across refresh', async ({ page }) => {
  const state = await mockConsole(page, { count: 2 });
  await page.goto('/?view=settings');
  const mode = page.getByLabel('总并发模式', { exact: true });
  const save = page.getByRole('button', { name: '保存并发', exact: true });
  const limit = page.getByLabel('同时执行上限', { exact: true });
  await expect(mode).toHaveValue('auto');
  await expect(page.getByRole('progressbar', { name: '服务总并发：0 个执行中，上限 10', exact: true })).toBeVisible();
  await expect(limit).toHaveCount(0);
  await expect(save).toBeDisabled();
  await mode.selectOption('manual');
  for (const invalid of ['0', '257', '1.5', '']) {
    await limit.fill(invalid);
    await expect(save).toBeDisabled();
  }
  await limit.fill('3');
  await save.click();
  await expect(save).toBeDisabled();
  expect(state.writes.at(-1)?.body).toMatchObject({ globalLimitAuto: false, globalLimit: 3 });
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  await expect(page.getByLabel('账户并发上限', { exact: true })).toHaveValue('5');
  await page.getByLabel('账户并发上限', { exact: true }).fill('7');
  await page.getByRole('button', { name: '保存调度', exact: true }).click();
  await expect.poll(() => state.data.operations.accountCapacity).toBe(12);
  await page.goto('/?view=settings');
  await expect(mode).toHaveValue('manual');
  await expect(limit).toHaveValue('3');
  await expect(page.getByRole('progressbar', { name: '服务总并发：0 个执行中，上限 3', exact: true })).toBeVisible();
  await mode.selectOption('auto');
  await save.click();
  await expect(save).toBeDisabled();
  expect(state.writes.at(-1)?.body).toMatchObject({ globalLimitAuto: true });
  expect(state.writes.at(-1)?.body).not.toHaveProperty('globalLimit');
  await expect(page.getByRole('progressbar', { name: '服务总并发：0 个执行中，上限 12', exact: true })).toBeVisible();
  await page.reload();
  await expect(mode).toHaveValue('auto');
  await mode.selectOption('manual');
  await expect(limit).toHaveValue('3');
});

test('empty automatic capacity and narrow settings layout remain usable', async ({ page }) => {
  await mockConsole(page, { count: 0 });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/?view=settings');
  const meter = page.getByRole('progressbar', { name: '服务总并发：0 个执行中，上限 0', exact: true });
  await expect(meter).toHaveAttribute('max', '1');
  await expect(page.getByText('按账户并发配置自动汇总，当前为 0 个请求。两个默认账户共 10 并发。', { exact: true })).toBeVisible();
  await page.getByLabel('总并发模式', { exact: true }).selectOption('manual');
  await expect(page.getByLabel('同时执行上限', { exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: '../.cache/global-concurrency-mobile.png', fullPage: true });
});

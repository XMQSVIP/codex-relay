import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const width of [1440, 390]) {
  test(`request and response models remain distinct at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    const state = await mockConsole(page);
    state.records[0].model = 'gpt-6-astra';
    state.records[0].responseModel = 'gpt-6.1-sol';
    await page.goto('/?view=requests');
    const cell = page.locator('.record-model').first();
    await expect(cell.locator('strong')).toHaveText('gpt-6-astra');
    await expect(cell.locator('.record-response-model')).toHaveText('实际响应模型：gpt-6.1-sol');
    await expect(cell).not.toContainText('推理强度');
    await expect(page.locator('.record-effort').first()).toHaveText('默认');
    if (width > 760) await expect(page.getByRole('columnheader', { name: '推理强度', exact: true })).toBeVisible();
    await page.screenshot({ path: `../.cache/response-model-${width}.png`, fullPage: true });
    await page.emulateMedia({ colorScheme: 'dark' });
    await page.screenshot({ path: `../.cache/response-model-dark-${width}.png`, fullPage: true });
    await page.getByRole('button', { name: '查看请求详情 40000000', exact: true }).click();
    await expect(page.locator('.request-fields > div').filter({ has: page.getByText('请求模型', { exact: true }) })).toContainText('gpt-6-astra');
    await expect(page.locator('.request-fields > div').filter({ has: page.getByText('实际响应模型', { exact: true }) })).toContainText('gpt-6.1-sol');
  });
}

test('same, missing and historical response models are displayed honestly', async ({ page }) => {
  const state = await mockConsole(page);
  state.records[0].responseModel = 'test-model';
  state.records.push({ ...state.records[0], id: 'missing', responseModel: null });
  const old = { ...state.records[0], id: 'historical' };
  delete old.responseModel;
  state.records.push(old);
  state.records.push({ ...old, id: 'management', kind: 'management', model: null });
  await page.goto('/?view=requests');
  const cells = page.locator('.record-model');
  await expect(cells).toHaveCount(4);
  await expect(cells.nth(0).locator('.record-response-model')).toHaveCount(0);
  await expect(cells.nth(1).locator('.record-response-model')).toHaveCount(0);
  await expect(cells.nth(2).locator('.record-response-model')).toHaveCount(0);
  await expect(cells.nth(3).locator('.record-response-model')).toHaveCount(0);
});

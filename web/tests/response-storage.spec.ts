import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

test('shows persistent routing fingerprints separately from chat bodies', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.responseBindings = {
    count: 3, bytes: 1048576, maxBytes: 1073741824,
    ttlSeconds: 2592000, persistent: true, bodiesStored: false,
  };
  await page.goto('/?view=statistics');
  const storage = page.locator('.storage-status');
  await storage.locator('summary').click();
  await expect(storage).toContainText('续聊账户绑定');
  await expect(storage).toContainText('3 个指纹');
  await expect(storage).toContainText('保留 30 天，重启后仍可识别原账户');
  await expect(storage).toContainText('聊天正文不写入本地数据库');
  await expect(storage).not.toContainText('undefined');
  await expect(storage).not.toContainText('重启失效');
});

test('binding status failures do not show misleading counts', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.responseBindings = { persistent: true, bodiesStored: false, error: 'RESPONSE_STORAGE' };
  await page.goto('/?view=statistics');
  const storage = page.locator('.storage-status');
  await storage.locator('summary').click();
  await expect(storage).toContainText('暂时无法读取账户绑定状态');
  await expect(storage).not.toContainText('NaN');
});

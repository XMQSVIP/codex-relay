import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';

for (const stale of [false, true]) {
  test(`imported accounts automatically allow capabilities with ${stale ? 'stale' : 'missing'} metadata`, async ({ page }) => {
    const state = await mockConsole(page);
    const policy = state.data.operations.accounts[0];
    policy.features = [];
    if (stale) policy.capabilities = { installation: 'older-native', provider: 'fixture', checkedAt: 1, stale: true, models: [], webSearch: null, imageGeneration: null, errorCode: null, verifiedFeatures: [] };
    await page.goto(`/?view=accounts&account=${nativeId}&section=advanced`);
    const trigger = page.locator('.capability-details > summary');
    await expect(trigger.getByText('展开设置', { exact: true })).toBeVisible();
    await trigger.focus();
    await page.keyboard.press('Enter');
    await expect(trigger.getByText('收起设置', { exact: true })).toBeVisible();
    await page.keyboard.press('Space');
    await expect(trigger.getByText('展开设置', { exact: true })).toBeVisible();
    await trigger.click();
    await expect(page.getByText('客户端 function、custom、namespace 和 MCP 工具默认可用，无需验证。')).toBeVisible();
    for (const name of ['内嵌图片输入', '搜索', '生图']) {
      await expect(page.getByRole('checkbox', { name, exact: true })).toBeChecked();
      await expect(page.getByRole('checkbox', { name, exact: true })).toBeEnabled();
    }
    expect(state.writes).toEqual([]);
  });
}

test('manual disable persists, optional diagnostics leave it disabled, and re-enabling needs no probe', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto(`/?view=accounts&account=${nativeId}&section=advanced`);
  await page.getByText('能力与原生版本', { exact: true }).click();
  const checkbox = page.getByRole('checkbox', { name: '内嵌图片输入', exact: true });
  await checkbox.click();
  await expect(checkbox).not.toBeChecked();
  await expect(checkbox).toBeEnabled();
  expect(state.writes.map(({ path, body }) => ({ path, body }))).toEqual([
    { path: `/accounts/${nativeId}/features`, body: { revision: 1, disabledFeatures: ['imageInput'] } },
  ]);
  await page.reload();
  await page.getByText('能力与原生版本', { exact: true }).click();
  await expect(checkbox).not.toBeChecked();
  await page.getByRole('button', { name: '内嵌图片输入运行诊断', exact: true }).click();
  await expect(page.getByRole('status').filter({ hasText: '能力诊断已通过，接入开关保持不变' })).toBeVisible();
  await expect(checkbox).not.toBeChecked();
  await checkbox.click();
  await expect(checkbox).toBeChecked();
  await expect(checkbox).toBeEnabled();
  expect(state.writes.filter(item => item.path.endsWith('/features/verify'))).toHaveLength(1);
  expect(state.writes.at(-1)).toEqual({ path: `/accounts/${nativeId}/features`, body: { revision: 2, disabledFeatures: [] } });
});

import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const width of [390, 1440]) {
  test(`key fast mode persists through create and edit and controls the lightning icon ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.emulateMedia({ colorScheme: 'dark' });
    const state = await mockConsole(page);
    await page.goto('/?view=keys');
    await expect(page.getByRole('img', { name: '已开启强制快速模式', exact: true })).toHaveCount(0);
    await page.getByRole('button', { name: '创建 API Key', exact: true }).click();
    const create = page.getByRole('dialog', { name: '创建 API Key', exact: true });
    await create.getByLabel('密钥名称', { exact: true }).fill('快速测试');
    await expect(create.getByRole('switch', { name: '强制快速模式', exact: true })).not.toBeChecked();
    await create.getByRole('switch', { name: '强制快速模式', exact: true }).check();
    await create.getByRole('button', { name: '创建密钥', exact: true }).click();
    await expect(page.getByRole('img', { name: '已开启强制快速模式', exact: true })).toHaveCount(1);
    expect(state.writes.find(write => write.path === '/api-keys')?.body.fastMode).toBe(true);
    const row = page.locator('.key-table tbody tr').filter({ has: page.getByText('快速测试', { exact: true }) });
    await row.getByRole('button', { name: /编辑/ }).click();
    const edit = page.getByRole('dialog', { name: '编辑 API Key', exact: true });
    await expect(edit.getByRole('switch', { name: '强制快速模式', exact: true })).toBeChecked();
    await edit.screenshot({ path: info.outputPath('key-fast-mode.png') });
    await edit.getByRole('switch', { name: '强制快速模式', exact: true }).uncheck();
    await edit.getByRole('button', { name: '保存修改', exact: true }).click();
    await expect(page.getByRole('img', { name: '已开启强制快速模式', exact: true })).toHaveCount(0);
    expect(state.writes.at(-1)?.body.fastMode).toBe(false);
    await page.reload();
    await expect(page.getByRole('img', { name: '已开启强制快速模式', exact: true })).toHaveCount(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
}

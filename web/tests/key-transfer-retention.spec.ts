import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { mockConsole, groupId } from './fixtures';

const entry = { name: '备份客户端', secret: `cr_live_${'a'.repeat(64)}`, groupId, models: ['test-model'], state: 'paused', createdAt: 1, expiresAt: null, maxConcurrency: 3 };
const backup = { version: 1, keys: [entry] };

test('exports all keys across filters and pages, then imports the downloaded backup', async ({ page }, testInfo) => {
  const state = await mockConsole(page);
  const key = state.keys[0];
  state.keys.splice(0, 1, ...Array.from({ length: 25 }, (_, i) => ({ ...key, id: `key-${i}`, name: `客户端 ${i}`, secret: `cr_live_${i.toString(16).padStart(64, '0')}` })));
  const exported = { version: 1, keys: state.keys.map(key => ({ ...entry, name: key.name, secret: key.secret })) };
  await page.route('**/api/api-keys/export', route => route.fulfill({ json: exported }));
  let imported: unknown;
  await page.route('**/api/api-keys/import', route => {
    imported = route.request().postDataJSON();
    return route.fulfill({ json: { imported: 0, skipped: 25 } });
  });
  await page.goto('/?view=keys');
  await expect(page.getByLabel('第 2 页')).toBeVisible();
  await page.getByRole('searchbox').fill('客户端 1');
  const downloadPromise = page.waitForEvent('download');
  await page.getByRole('button', { name: '全部导出' }).click();
  const download = await downloadPromise;
  const path = testInfo.outputPath('client-keys.json');
  await download.saveAs(path);
  expect(JSON.parse(await readFile(path, 'utf8'))).toEqual(exported);
  await page.getByRole('button', { name: '批量导入', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '批量导入 API 密钥' });
  await dialog.getByLabel('密钥备份文件').setInputFiles(path);
  await expect(dialog.getByLabel('JSON 备份内容')).toHaveValue(/客户端 24/);
  await dialog.getByRole('button', { name: '导入密钥', exact: true }).click();
  await expect(dialog).not.toBeVisible();
  expect(imported).toEqual({ backup: exported });
  await expect(page.getByRole('status').filter({ hasText: '跳过 25 个重复密钥' })).toBeVisible();
  await expect(page.getByRole('searchbox')).toHaveValue('');
});

test('invalid JSON stays local and server validation can be corrected with a destination group', async ({ page }, testInfo) => {
  await mockConsole(page);
  const bodies: any[] = [];
  await page.route('**/api/api-keys/import', route => {
    const body = route.request().postDataJSON(); bodies.push(body);
    return body.groupId ? route.fulfill({ json: { imported: 1, skipped: 0 } }) : route.fulfill({ status: 400, json: { error: { code: 'INVALID_INPUT', message: '第 1 项的原分组不存在，请选择目标分组后重新导入。' } } });
  });
  await page.goto('/?view=keys');
  const trigger = page.getByRole('button', { name: '批量导入', exact: true });
  await trigger.click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('JSON 备份内容').fill('{bad json');
  await dialog.getByRole('button', { name: '导入密钥', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('JSON 格式无效');
  await expect(dialog.getByRole('alert')).toBeFocused();
  expect(bodies).toHaveLength(0);
  await dialog.getByLabel('JSON 备份内容').fill(JSON.stringify(backup));
  await dialog.getByRole('button', { name: '导入密钥', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('原分组不存在');
  await expect(dialog.getByLabel('JSON 备份内容')).toHaveValue(JSON.stringify(backup));
  await page.screenshot({ path: testInfo.outputPath('import-dialog.png'), fullPage: true });
  await dialog.getByLabel('导入分组').selectOption(groupId);
  await dialog.getByRole('button', { name: '导入密钥', exact: true }).click();
  await expect(dialog).not.toBeVisible();
  expect(bodies[1]).toEqual({ backup, groupId });
  await expect(page.getByRole('status').filter({ hasText: '已导入 1 个密钥' })).toBeVisible();
});

test('import prevents duplicate submissions and leaves backup available after a network failure', async ({ page }) => {
  await mockConsole(page);
  let release!: () => void;
  const waiting = new Promise<void>(resolve => { release = resolve; });
  let requests = 0;
  await page.route('**/api/api-keys/import', async route => { requests++; await waiting; await route.abort('failed'); });
  await page.goto('/?view=keys');
  await page.getByRole('button', { name: '批量导入', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('JSON 备份内容').fill(JSON.stringify(backup));
  await dialog.getByRole('button', { name: '导入密钥', exact: true }).click();
  await expect(dialog.getByRole('button', { name: '正在处理…' })).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeVisible();
  release();
  await expect(dialog.getByRole('alert')).toContainText('可重新导入');
  await expect(dialog.getByLabel('JSON 备份内容')).toHaveValue(JSON.stringify(backup));
  expect(requests).toBe(1);
});

test('retention defaults to 30, validates input, persists and updates statistics', async ({ page }, testInfo) => {
  const state = await mockConsole(page);
  const writes: unknown[] = [];
  await page.route('**/api/settings/retention', route => {
    const body = route.request().postDataJSON(); writes.push(body);
    state.data.operations.retentionDays = body.retentionDays;
    state.data.operations.revision++;
    return route.fulfill({ json: state.data.operations });
  });
  await page.goto('/?view=settings');
  const field = page.getByLabel('请求记录保存时长');
  const save = page.getByRole('button', { name: '保存时长' });
  await expect(field).toHaveValue('30');
  await expect(save).toBeDisabled();
  for (const value of ['', '0', '1.5', '366', '3650']) { await field.fill(value); await expect(save).toBeDisabled(); }
  for (const value of ['1', '365']) { await field.fill(value); await expect(save).toBeEnabled(); }
  await field.fill('90');
  await save.click();
  await expect(page.getByRole('status').filter({ hasText: '已设为 90 天' })).toBeVisible();
  expect(writes).toEqual([{ revision: 1, retentionDays: 90 }]);
  await page.reload();
  await expect(field).toHaveValue('90');
  await page.screenshot({ path: testInfo.outputPath('retention-settings.png'), fullPage: true });
  await page.goto('/?view=statistics');
  await expect(page.locator('.statistics-update')).toContainText('保留 90 天');
});

test('retention save conflicts preserve the draft and mobile controls fit the viewport', async ({ page }, testInfo) => {
  await mockConsole(page);
  await page.route('**/api/settings/retention', route => route.fulfill({ status: 409, json: { error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请刷新后重试。' } } }));
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/?view=settings');
  await page.getByLabel('请求记录保存时长').fill('7');
  await page.getByRole('button', { name: '保存时长' }).click();
  await expect(page.getByRole('alert')).toContainText('配置已更新');
  await expect(page.getByLabel('请求记录保存时长')).toHaveValue('7');
  await page.goto('/?view=keys');
  await page.getByRole('button', { name: '批量导入', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByLabel('JSON 备份内容')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const bounds = await dialog.boundingBox();
  expect(bounds!.width).toBeLessThanOrEqual(390);
  await page.screenshot({ path: testInfo.outputPath('mobile-import.png'), fullPage: true });
  await page.keyboard.press('Escape');
  await expect(dialog).not.toBeVisible();
  await expect(page.getByRole('button', { name: '批量导入', exact: true })).toBeFocused();
});

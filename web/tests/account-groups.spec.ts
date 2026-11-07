import { test, expect } from '@playwright/test';
import { mockConsole, nativeId, groupId } from './fixtures';

const second = '20000000-0000-0000-0000-000000000001';

test('account membership editor saves multiple groups and filters match every membership', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/');
  const trigger = page.getByRole('button', { name: '本机 Codex的所属分组', exact: true });
  await trigger.click();
  const dialog = page.getByRole('dialog', { name: '设置分组：本机 Codex', exact: true });
  await expect(dialog.getByRole('checkbox', { name: '默认分组', exact: true })).toBeDisabled();
  const additional = dialog.getByRole('checkbox', { name: '开发测试', exact: true });
  await additional.focus();
  await page.keyboard.press('Space');
  await expect(additional).toBeChecked();
  await dialog.getByRole('button', { name: '保存分组', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(trigger).toContainText('默认分组、开发测试');
  expect(state.writes.find(write => write.path.endsWith('/policy'))?.body.groupIds).toEqual([groupId, second]);
  await page.getByRole('combobox', { name: '筛选所属分组' }).selectOption(second);
  await expect(page.locator('.account-console-row')).toHaveCount(1);
  await page.getByRole('combobox', { name: '筛选所属分组' }).selectOption(groupId);
  await expect(page.locator('.account-console-row')).toHaveCount(3);
  await page.getByRole('searchbox', { name: '搜索账户' }).fill('开发测试');
  await expect(page.locator('.account-console-row')).toHaveCount(1);
  await page.goto('/?view=groups');
  const group = page.getByRole('article', { name: '开发测试', exact: true });
  await expect(group).toContainText('1 个账户');
  await expect(group).toContainText('本机 Codex');
  await expect(group.getByRole('button', { name: '删除开发测试' })).toBeDisabled();
});

for (const [width, theme] of [[375, 'light'], [1440, 'dark']] as const) test(`group editor fits ${width}px ${theme}`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 });
  await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
  const state = await mockConsole(page);
  state.data.operations.groups[1].name = '用于验证多分组展示与换行的较长分组名称';
  state.data.operations.accounts[0].groupIds = [groupId, second];
  await page.goto('/');
  await page.getByRole('button', { name: '本机 Codex的所属分组', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  const box = await dialog.boundingBox();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(width);
  const choices = dialog.locator('.account-group-options label');
  const firstBox = await choices.nth(0).boundingBox(), secondBox = await choices.nth(1).boundingBox();
  if (width >= 761) {
    expect(secondBox!.y).toBe(firstBox!.y);
    expect(secondBox!.x).toBeGreaterThan(firstBox!.x);
  } else {
    expect(secondBox!.x).toBe(firstBox!.x);
    expect(secondBox!.y).toBeGreaterThan(firstBox!.y);
  }
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1);
  await page.screenshot({ path: info.outputPath('group-editor.png') });
  await page.keyboard.press('Escape');
  await expect(page.getByRole('button', { name: '本机 Codex的所属分组', exact: true })).toBeFocused();
});

test('paused membership does not disable other memberships and errors retain selections', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].groupIds = [groupId, second];
  state.data.operations.groups[0].enabled = false;
  let fail = true;
  await page.route('**/api/accounts/*/policy', route => fail ? route.fulfill({ status: 409, json: { error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请重新打开分组设置。' } } }) : route.fallback());
  await page.goto('/');
  const row = page.locator(`#account-${nativeId}`);
  await expect(row).toContainText('可调度');
  await row.getByRole('button', { name: '本机 Codex的所属分组' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('checkbox', { name: '默认分组（已暂停）' }).uncheck();
  await dialog.getByRole('button', { name: '保存分组' }).click();
  await expect(page.getByRole('alert')).toContainText('配置已更新');
  await expect(dialog.getByRole('checkbox', { name: '开发测试' })).toBeChecked();
  await expect(dialog.getByRole('checkbox', { name: '默认分组（已暂停）' })).not.toBeChecked();
  fail = false;
  await dialog.getByRole('button', { name: '保存分组' }).click();
  await expect(dialog).toHaveCount(0);
  expect(state.data.operations.accounts[0].groupIds).toEqual([second]);
  state.data.operations.accounts[0].active = 1;
  await page.getByRole('button', { name: '刷新账号数据' }).click();
  await expect(row.getByRole('button', { name: '本机 Codex的所属分组' })).toBeDisabled();
});

test('batch configuration sends multiple memberships and details preserve them when saving concurrency', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].groupIds = [groupId, second];
  let batch: Record<string, unknown> | undefined;
  await page.route('**/api/accounts/batch', route => {
    batch = route.request().postDataJSON();
    return route.fulfill({ json: { action: 'configure', status: 'completed', items: [{ accountId: nativeId, status: 'succeeded' }] } });
  });
  await page.goto('/');
  await page.getByRole('checkbox', { name: '选择账号：本机 Codex' }).check();
  await page.getByRole('button', { name: '更多', exact: true }).click();
  await page.getByRole('button', { name: '批量配置', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('checkbox', { name: '修改所属分组' }).check();
  await dialog.getByRole('checkbox', { name: '开发测试', exact: true }).check();
  await dialog.getByRole('button', { name: '提交配置' }).click();
  await expect(dialog).toHaveCount(0);
  expect(batch?.groupIds).toEqual([groupId, second]);
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  await page.getByRole('spinbutton', { name: '账户并发上限' }).fill('7');
  await page.getByRole('button', { name: '保存调度' }).click();
  await expect.poll(() => state.data.operations.accounts[0].maxConcurrency).toBe(7);
  expect(state.data.operations.accounts[0].groupIds).toEqual([groupId, second]);
});

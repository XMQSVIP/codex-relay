import { expect, test } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';

test('action feedback floats above the page without moving content or taking focus', async ({ page }) => {
  await mockConsole(page);
  await page.goto('/');
  const toolbar = page.locator('.account-console-toolbar');
  const before = await toolbar.boundingBox();
  const toggle = page.getByRole('switch', { name: '启用账户：本机 Codex', exact: true });
  await toggle.click();
  const toast = page.locator('.toast-success');
  await expect(toast.getByRole('status')).toContainText('本机 Codex：账户已停用');
  await expect(toast.locator(':focus')).toHaveCount(0);
  expect(await toolbar.boundingBox()).toEqual(before);
  await expect(page.locator('.account-row-feedback')).toHaveCount(0);
  await page.screenshot({ path: '../.cache/toast-desktop.png' });
  await toast.getByRole('button', { name: '关闭操作提示' }).click();
  await expect(toast).toHaveCount(0);
});

test('error feedback is visible and dismissible inside a modal while preserving the draft', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=groups');
  await page.getByRole('button', { name: '编辑分组：默认分组', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: '编辑分组', exact: true });
  await dialog.getByLabel('分组名称').fill('保留编辑内容');
  state.conflict = true;
  await dialog.getByRole('button', { name: '保存分组' }).click();
  const toast = dialog.locator('.toast-error');
  await expect(toast.getByRole('alert')).toContainText('配置已更新');
  await expect(dialog.getByLabel('分组名称')).toHaveValue('保留编辑内容');
  await expect(toast.locator(':focus')).toHaveCount(0);
  await toast.getByRole('button', { name: '关闭操作提示' }).click();
  await expect(toast).toHaveCount(0);
  await expect(dialog).toBeVisible();
});

test('deletion feedback survives closing the modal and removing its account row', async ({ page }) => {
  const state = await mockConsole(page);
  await page.route(`**/api/accounts/${nativeId}`, async route => {
    expect(route.request().method()).toBe('DELETE');
    state.data.accounts = state.data.accounts.filter(account => account.id !== nativeId);
    state.data.operations.accounts = state.data.operations.accounts.filter(account => account.id !== nativeId);
    await route.fulfill({ json: { ok: true } });
  });
  await page.goto('/');
  await page.getByRole('button', { name: /^删除账户 本机 Codex/ }).click();
  await page.getByRole('dialog', { name: '删除“本机 Codex”？', exact: true }).getByRole('button', { name: '确认删除' }).click();
  await expect(page.locator(`#account-${nativeId}`)).toHaveCount(0);
  await expect(page.locator('.toast-success')).toContainText('“本机 Codex”已删除，历史统计保留。');
  await page.locator('.toast-success').getByRole('button', { name: '关闭操作提示' }).click();
  await expect(page.locator('.toast')).toHaveCount(0);
});

test('toasts expire, pause while hovered and show repeated results again', async ({ page }) => {
  await mockConsole(page);
  await page.route(`**/api/accounts/${nativeId}/quota/refresh`, route => route.fulfill({ json: { status: 'supported', checkedAt: Date.now(), stale: false, blocked: false, windows: [{ bucket: 'codex', window: 'primary', durationMins: 300, usedPercent: 20, resetsAt: Date.now() + 60_000, reached: false }] } }));
  await page.goto(`/?view=accounts&expanded=${nativeId}`);
  await page.clock.install();
  const refresh = page.getByRole('button', { name: '刷新额度', exact: true });
  await refresh.click();
  const toast = page.locator('.toast-success');
  await expect(toast).toContainText('上游额度已刷新。');
  await toast.hover();
  await page.clock.fastForward(6000);
  await expect(toast).toBeVisible();
  await page.mouse.move(0, 0);
  await page.clock.fastForward(5100);
  await expect(toast).toHaveCount(0);
  await refresh.click();
  await expect(toast).toContainText('上游额度已刷新。');
  await refresh.click();
  await expect(toast).toHaveCount(1);
  await page.clock.fastForward(5100);
  await expect(toast).toHaveCount(0);
});

test('mobile dark mode keeps long feedback inside the viewport', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' });
  await mockConsole(page);
  await page.goto('/');
  await page.getByRole('switch', { name: '启用账户：本机 Codex', exact: true }).click();
  const toast = page.locator('.toast-success');
  await expect(toast).toBeVisible();
  const box = (await toast.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(390);
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: '../.cache/toast-mobile-dark.png' });
});

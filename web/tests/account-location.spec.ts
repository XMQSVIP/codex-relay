import { test, expect, type Page } from '@playwright/test';
import { mockConsole, nativeId, oauthId } from './fixtures';

async function setup(page: Page) {
  const state = Object.assign(await mockConsole(page), { detectionFailed: false });
  await page.route('**/api/location/options', route => route.fulfill({ json: { timezones: ['America/New_York', 'Asia/Tokyo', 'Asia/Shanghai', 'Etc/UTC'], countries: ['CN', 'JP', 'US'] } }));
  await page.route(/\/api\/accounts\/[^/]+\/location(?:\/detect)?$/, async route => {
    const path = new URL(route.request().url()).pathname.replace('/api', '');
    const body = route.request().postDataJSON();
    state.writes.push({ path, body });
    const policy = state.data.operations.accounts.find(a => a.id === path.split('/')[2])!;
    if (state.conflict) { state.conflict = false; return route.fulfill({ status: 409, json: { error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请刷新后重试。' } } }); }
    if (path.endsWith('/detect')) {
      if (state.detectionFailed) return route.fulfill({ status: 503, json: { error: { code: 'LOCATION_DETECTION', message: '出口位置检测失败，请稍后重试。' } } });
      return route.fulfill({ json: { revision: state.data.operations.revision, preview: { id: 'preview-token', detectedAt: Date.now(), location: { timezone: 'Asia/Tokyo', country: 'JP', region: 'Tokyo', city: 'Tokyo' } } } });
    } else {
      policy.location = body.config;
      policy.locationStatus = !body.config.enabled ? { state: 'disabled', effective: null } : body.config.source === 'custom' ? { state: 'applied', effective: body.config.custom } : { state: 'applied', effective: { timezone: 'Asia/Tokyo', country: 'JP', region: 'Tokyo', city: 'Tokyo' } };
      policy.proxyStatus = { state: policy.active ? 'switching' : 'applied' };
      state.data.operations.revision++;
    }
    return route.fulfill({ json: state.data.operations });
  });
  await page.goto(`/?view=accounts&account=${nativeId}&section=access`);
  return state;
}

test('location is opt-in, custom values persist and stay account scoped', async ({ page }) => {
  const state = await setup(page);
  const panel = page.locator('#account-location');
  await expect(panel.getByLabel('启用时区与搜索位置覆盖')).not.toBeChecked();
  expect(state.writes).toHaveLength(0);
  await panel.getByLabel('启用时区与搜索位置覆盖').check();
  await expect(panel.getByLabel('位置来源')).toHaveValue('proxy');
  await panel.getByLabel('位置来源').selectOption('custom');
  await panel.getByLabel('时区', { exact: true }).fill('America/New_York');
  await panel.getByLabel('国家或地区（选填）').selectOption('US');
  await panel.getByLabel('城市（选填）').fill('New York');
  await panel.getByRole('button', { name: '保存时区与位置', exact: true }).click();
  await expect(panel.getByText('当前生效：美国 · New York · America/New_York')).toBeVisible();
  expect(state.data.operations.accounts[0].location?.custom.city).toBe('New York');
  await panel.getByLabel('启用时区与搜索位置覆盖').uncheck();
  await panel.getByRole('button', { name: '保存时区与位置', exact: true }).click();
  await expect(panel.getByText('已关闭', { exact: true })).toBeVisible();
  expect(state.data.operations.accounts[0].location?.custom.city).toBe('New York');
  await page.goto(`/?view=accounts&account=${oauthId}&section=access`);
  await expect(panel.getByLabel('启用时区与搜索位置覆盖')).not.toBeChecked();
});

test('invalid fields and conflicts preserve user input', async ({ page }) => {
  const state = await setup(page);
  const panel = page.locator('#account-location');
  await panel.getByLabel('启用时区与搜索位置覆盖').check();
  await panel.getByLabel('位置来源').selectOption('custom');
  await panel.getByLabel('时区', { exact: true }).fill('Invalid/Zone');
  await panel.getByRole('button', { name: '保存时区与位置', exact: true }).click();
  await expect(panel.getByText('请选择有效的 IANA 时区，例如 America/New_York。', { exact: true })).toBeVisible();
  await panel.getByLabel('时区', { exact: true }).fill('Asia/Tokyo');
  await panel.getByLabel('城市（选填）').fill('Tokyo');
  await panel.getByRole('button', { name: '保存时区与位置', exact: true }).click();
  await expect(panel.getByText('填写地区或城市时，请先选择国家。', { exact: true })).toBeVisible();
  expect(state.writes).toHaveLength(0);
  await panel.getByLabel('国家或地区（选填）').selectOption('JP');
  state.conflict = true;
  await panel.getByRole('button', { name: '保存时区与位置', exact: true }).click();
  await expect(panel.getByText('配置已更新，请刷新后重试。', { exact: true })).toBeVisible();
  await expect(panel.getByLabel('城市（选填）')).toHaveValue('Tokyo');
  await panel.getByRole('button', { name: '还原时区与位置' }).click();
  await expect(panel.getByLabel('启用时区与搜索位置覆盖')).not.toBeChecked();
});

test('detection previews without saving or restarting and requires confirmation', async ({ page }) => {
  const state = await setup(page);
  const panel = page.locator('#account-location');
  state.data.operations.accounts[0].active = 1;
  const before = JSON.stringify(state.data.operations);
  await panel.getByLabel('启用时区与搜索位置覆盖').check();
  await expect(panel.getByRole('button', { name: '保存检测结果', exact: true })).toBeDisabled();
  await panel.getByRole('button', { name: '检测时区', exact: true }).focus();
  await page.keyboard.press('Enter');
  await expect(panel.getByRole('status', { name: '检测预览' })).toContainText('Asia/Tokyo');
  expect(JSON.stringify(state.data.operations)).toBe(before);
  expect(state.writes).toHaveLength(1);
  expect(state.writes[0].path).toBe(`/accounts/${nativeId}/location/detect`);
  await page.setViewportSize({ width: 375, height: 844 });
  await panel.scrollIntoViewIfNeeded();
  await panel.screenshot({ path: '../.cache/location-preview-mobile.png' });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await panel.getByRole('button', { name: '保存检测结果', exact: true }).click();
  expect(state.writes[1]).toMatchObject({ path: `/accounts/${nativeId}/location`, body: { previewId: 'preview-token', config: { enabled: true, source: 'proxy' } } });
  await expect(panel.getByRole('status', { name: '检测预览' })).toHaveCount(0);
  await expect(panel.getByText('正在立即重启账户实例…', { exact: true })).toBeVisible();
});

test('discarding a preview leaves existing settings unchanged', async ({ page }) => {
  const state = await setup(page);
  const before = JSON.stringify(state.data.operations);
  const panel = page.locator('#account-location');
  await panel.getByLabel('启用时区与搜索位置覆盖').check();
  await panel.getByRole('button', { name: '检测时区', exact: true }).click();
  await panel.getByRole('button', { name: '放弃检测结果', exact: true }).click();
  await expect(panel.getByRole('status', { name: '检测预览' })).toHaveCount(0);
  await expect(panel.getByRole('button', { name: '保存检测结果', exact: true })).toBeDisabled();
  expect(JSON.stringify(state.data.operations)).toBe(before);
  expect(state.writes).toHaveLength(1);
});

test('failed detection keeps current settings and does not enable saving', async ({ page }) => {
  const state = await setup(page);
  state.detectionFailed = true;
  const before = JSON.stringify(state.data.operations);
  const panel = page.locator('#account-location');
  await panel.getByLabel('启用时区与搜索位置覆盖').check();
  await panel.getByRole('button', { name: '检测时区', exact: true }).click();
  await expect(panel.getByText('出口位置检测失败，请稍后重试。', { exact: true })).toBeVisible();
  await expect(panel.getByRole('button', { name: '保存检测结果', exact: true })).toBeDisabled();
  expect(JSON.stringify(state.data.operations)).toBe(before);
});

test('save conflict preserves preview and draft, and reverting discards both', async ({ page }) => {
  const state = await setup(page);
  const panel = page.locator('#account-location');
  await panel.getByLabel('启用时区与搜索位置覆盖').check();
  await panel.getByRole('button', { name: '检测时区', exact: true }).click();
  state.conflict = true;
  await panel.getByRole('button', { name: '保存检测结果', exact: true }).click();
  await expect(panel.getByText('配置已更新，请刷新后重试。', { exact: true })).toBeVisible();
  await expect(panel.getByRole('status', { name: '检测预览' })).toBeVisible();
  expect(state.writes).toHaveLength(2);
  await panel.getByRole('button', { name: '还原时区与位置' }).click();
  await expect(panel.getByRole('status', { name: '检测预览' })).toHaveCount(0);
  await expect(panel.getByLabel('启用时区与搜索位置覆盖')).not.toBeChecked();
});

test('immediate restart feedback and mobile layout', async ({ page }) => {
  const state = await setup(page);
  state.data.operations.accounts[0].active = 1;
  await page.setViewportSize({ width: 390, height: 844 });
  const panel = page.locator('#account-location');
  await panel.getByLabel('启用时区与搜索位置覆盖').check();
  await panel.getByLabel('位置来源').selectOption('custom');
  await panel.getByLabel('时区', { exact: true }).fill('Asia/Shanghai');
  await panel.scrollIntoViewIfNeeded();
  await panel.screenshot({ path: '../.cache/location-mobile.png' });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await panel.getByRole('button', { name: '保存时区与位置', exact: true }).click();
  await expect(panel.getByText('正在立即重启账户实例…', { exact: true })).toBeVisible();
  await expect(panel.getByLabel('启用时区与搜索位置覆盖')).toBeDisabled();
});

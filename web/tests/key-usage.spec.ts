import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';
import { buildCodexConfig, keyModelChoices } from '../src/codexConfig';
import type { ClientKey } from '../src/api';
import { groupId, mockConsole, overview } from './fixtures';

const useKey = (page: Page, name = '开发客户端') => page.getByRole('button', { name: `使用密钥：${name}`, exact: true });
const usage = (page: Page) => page.getByRole('dialog', { name: '使用密钥', exact: true });
const config = (page: Page) => usage(page).getByRole('textbox', { name: 'Codex 配置内容', exact: true });

async function clipboard(page: Page, mode: 'success' | 'reject' | 'timeout' = 'success') {
  await page.addInitScript(mode => {
    let copied = '';
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
      writeText: (value: string) => {
        if (mode === 'reject') return Promise.reject(new Error('Clipboard denied'));
        if (mode === 'timeout') return new Promise<void>(() => {});
        copied = value;
        return Promise.resolve();
      },
      readText: () => Promise.resolve(copied),
    } });
  }, mode);
}

test('TOML escapes credentials and preserves URL paths while removing trailing slashes', () => {
  const result = buildCodexConfig({ secret: 'synthetic"\\\n\t\u0000\u007f中文', baseUrl: 'https://relay.example.test/proxy/v1///', model: 'vendor/model:1', websocketEnabled: true });
  expect(result).toContain('base_url = "https://relay.example.test/proxy/v1"');
  expect(result).toContain('experimental_bearer_token = "synthetic\\"\\\\\\n\\t\\u0000\\u007f中文"');
  expect(result).toContain('model = "vendor/model:1"');
  expect(result).toContain('review_model = "vendor/model:1"');
  expect(result).toContain('supports_websockets = true');
  expect(result).toContain('requires_openai_auth = false');
  expect(result).toContain('[model_providers.OpenAI.capabilities]\n# Codex 0.162.0+');
  expect(result).toContain('remote_compaction = "v2"');
  expect(result).toContain('X-OpenAI-Actor-Authorization = "proxy-managed"');
  expect(result).toContain('[features]\nimage_generation = true\ngoals = true');
  expect(result).not.toContain('env_key');
});

test('model defaults respect key scope, enabled groups and account routes', () => {
  const operations = overview().operations;
  const key = { groupId, models: ['allowed-first', 'test-model'] } as ClientKey;
  expect(keyModelChoices(key, operations)).toEqual({ choices: ['allowed-first', 'test-model'], preferred: 'test-model' });
  key.models = ['test-model', 'gpt-6.1-sol'];
  expect(keyModelChoices(key, operations).preferred).toBe('gpt-6.1-sol');
  key.models = ['other'];
  expect(keyModelChoices(key, operations).preferred).toBe('other');
  key.models = [];
  operations.accounts[0].enabled = false;
  operations.accounts[1].groupIds = ['another-group'];
  operations.accounts[2].route!.model = 'chosen-route';
  expect(keyModelChoices(key, operations)).toEqual({ choices: ['gpt-6.1-sol', 'chosen-route'], preferred: 'gpt-6.1-sol' });
  operations.groups[0].enabled = false;
  expect(keyModelChoices(key, operations)).toEqual({ choices: ['gpt-6.1-sol'], preferred: 'gpt-6.1-sol' });
});

test('copies the selected key and current settings without writes or browser persistence', async ({ page }) => {
  await clipboard(page);
  const state = await mockConsole(page);
  state.keys.push({ ...state.keys[0], id: 'second-key', name: '第二个客户端', secret: 'cr_second_synthetic', models: ['other-model', 'test-model'] });
  await page.goto('/?view=keys');
  await expect(useKey(page)).toHaveText('');
  await expect(useKey(page)).toHaveAttribute('title', '使用密钥');
  await useKey(page).click();
  await expect(usage(page).getByLabel('接入地址', { exact: true })).toBeFocused();
  await expect(config(page)).toHaveValue(/model = "gpt-6.1-sol"/);
  await expect(config(page)).toHaveValue(/supports_websockets = false/);
  await expect(config(page)).toHaveValue(/experimental_bearer_token = "cr_test_secret_only_for_fixture"/);
  await usage(page).getByLabel('模型 ID', { exact: true }).fill('custom/model:2');
  await usage(page).getByRole('checkbox', { name: '启用 WebSocket' }).check();
  await usage(page).getByRole('button', { name: '复制配置', exact: true }).click();
  await expect(usage(page).getByRole('button', { name: '已复制配置', exact: true })).toBeEnabled();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(await config(page).inputValue());
  await expect(config(page)).toHaveValue(/model = "custom\/model:2"/);
  await expect(config(page)).toHaveValue(/review_model = "custom\/model:2"/);
  await expect(config(page)).toHaveValue(/\[features\]\nimage_generation = true\ngoals = true\n$/);
  await expect(config(page)).toHaveValue(/supports_websockets = true/);
  await page.keyboard.press('Escape');
  await expect(useKey(page)).toBeFocused();
  await useKey(page, '第二个客户端').click();
  const models = usage(page).getByLabel('模型 ID', { exact: true });
  await expect(models).toHaveValue('test-model');
  await expect(models.locator('option')).toHaveText(['other-model', 'test-model']);
  await models.selectOption('other-model');
  await expect(config(page)).toHaveValue(/cr_second_synthetic/);
  await expect(config(page)).not.toHaveValue(/cr_test_secret_only_for_fixture/);
  await expect(config(page)).toHaveValue(/supports_websockets = false/);
  await usage(page).getByRole('button', { name: '复制配置', exact: true }).click();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(await config(page).inputValue());
  expect(state.writes).toEqual([]);
  const storage = await page.evaluate(() => JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } }));
  for (const key of state.keys) expect(storage).not.toContain(key.secret!);
});

test('defaults to gpt-6.1-sol without routes and validates manual unrestricted models', async ({ page }) => {
  const state = await mockConsole(page);
  for (const account of state.data.operations.accounts) account.enabled = false;
  await page.goto('/?view=keys');
  await useKey(page).click();
  const model = usage(page).getByLabel('模型 ID', { exact: true });
  const copy = usage(page).getByRole('button', { name: '复制配置', exact: true });
  await expect(model).toHaveValue('gpt-6.1-sol');
  await expect(copy).toBeEnabled();
  await model.fill('');
  await expect(model).toHaveValue('');
  await expect(copy).toBeDisabled();
  await expect(config(page)).toHaveValue('');
  await model.fill('invalid model"');
  await expect(model).toHaveAttribute('aria-invalid', 'true');
  await expect(copy).toBeDisabled();
  await model.fill('vendor/new-model:1');
  await expect(copy).toBeEnabled();
  await expect(config(page)).toHaveValue(/model = "vendor\/new-model:1"/);
});

for (const allowPublicAccess of [false, true]) {
  test(`shares guide address selection with external access ${allowPublicAccess}`, async ({ page }) => {
    const state = await mockConsole(page);
    await page.route('**/api/session', route => route.fulfill({ json: { requiresToken: false, passwordEnabled: false, allowPublicAccess, authenticated: true } }));
    const gateway = 'https://relay.example.test/proxy/v1///';
    const lan = 'http://192.168.1.20:4317/v1';
    const local = 'http://127.0.0.1:4317/v1';
    await page.route('**/api/api-keys', route => route.fulfill({ json: {
      keys: state.keys, baseUrl: gateway, localBaseUrl: local, lanBaseUrls: [lan],
      networkEndpoints: [{ baseUrl: gateway, listen: '127.0.0.1:4317', transport: 'reverse-proxy' }], inferenceEnabled: true,
    } }));
    await page.goto('/?view=keys&tab=guide');
    const guide = page.getByLabel('HTTP Base URL', { exact: true });
    await expect(guide).toBeEnabled();
    await expect(guide).toHaveValue(allowPublicAccess ? gateway : local);
    const options = await guide.locator('option').allTextContents();
    const preferred = await guide.inputValue();
    expect(preferred).toBe(allowPublicAccess ? gateway : local);
    await page.getByRole('link', { name: 'API 密钥', exact: true }).click();
    await useKey(page).click();
    const address = usage(page).getByLabel('接入地址', { exact: true });
    await expect(address).toHaveValue(preferred);
    await expect(address.locator('option')).toHaveText(options);
    await address.selectOption(lan);
    await expect(config(page)).toHaveValue(/base_url = "http:\/\/192.168.1.20:4317\/v1"/);
    await address.selectOption(gateway);
    await expect(config(page)).toHaveValue(/base_url = "https:\/\/relay.example.test\/proxy\/v1"/);
    expect(state.writes).toEqual([]);
  });
}

for (const [agent, platform, path] of [
  ['Windows NT 10.0', 'Windows', '%USERPROFILE%\\.codex\\config.toml'],
  ['Macintosh; Intel Mac OS X', 'macOS / Linux', '~/.codex/config.toml'],
] as const) {
  test(`detects ${platform} and changes only the displayed config path`, async ({ page }) => {
    await page.addInitScript(agent => Object.defineProperty(navigator, 'userAgent', { value: agent, configurable: true }), agent);
    await mockConsole(page);
    await page.goto('/?view=keys');
    await useKey(page).click();
    await expect(usage(page).getByRole('button', { name: platform, exact: true })).toHaveAttribute('aria-pressed', 'true');
    await expect(usage(page).getByText(path, { exact: true })).toBeVisible();
    const before = await config(page).inputValue();
    await usage(page).getByRole('button', { name: platform === 'Windows' ? 'macOS / Linux' : 'Windows', exact: true }).click();
    await expect(usage(page).getByText(platform === 'Windows' ? '~/.codex/config.toml' : '%USERPROFILE%\\.codex\\config.toml', { exact: true })).toBeVisible();
    await expect(config(page)).toHaveValue(before);
    await expect(usage(page)).toContainText('CODEX_HOME');
  });
}

test('unusable keys have disabled actions and visible explanations', async ({ page }) => {
  const state = await mockConsole(page);
  state.keys[0].state = 'paused';
  state.keys.push({ ...state.keys[0], id: 'expired', name: '过期密钥', state: 'expired' });
  state.keys.push({ ...state.keys[0], id: 'missing-secret', name: '缺少密钥内容', state: 'active', secret: null });
  state.keys.push({ ...state.keys[0], id: 'past', name: '已到期密钥', state: 'active', expiresAt: Date.now() - 1 });
  await page.goto('/?view=keys');
  for (const key of state.keys) await expect(useKey(page, key.name)).toBeDisabled();
  await expect(page.getByText('密钥已暂停，请先启用密钥。', { exact: true })).toBeVisible();
  await expect(page.getByText('未取得完整密钥，无法生成配置，请刷新密钥列表。', { exact: true })).toBeVisible();
  await expect(page.getByText('密钥已过期，请创建新密钥。', { exact: true })).toHaveCount(2);
});

for (const change of ['delete', 'pause', 'expire', 'lose-secret'] as const) {
  test(`closes the dialog when polling detects key change: ${change}`, async ({ page }) => {
    const state = await mockConsole(page);
    await page.clock.install();
    await page.goto('/?view=keys');
    await useKey(page).click();
    if (change === 'delete') state.keys.splice(0);
    else if (change === 'pause') state.keys[0].state = 'paused';
    else if (change === 'expire') state.keys[0].state = 'expired';
    else state.keys[0].secret = null;
    await page.clock.fastForward(16_000);
    await expect(usage(page)).toHaveCount(0);
    await expect(page.locator('.toast-error')).toBeVisible();
    await expect(page.getByRole('button', { name: '创建 API Key', exact: true })).toBeFocused();
    expect(state.writes).toEqual([]);
  });
}

test('closes at the expiry time without waiting for a list refresh', async ({ page }) => {
  const state = await mockConsole(page);
  const now = Date.now();
  state.keys[0].expiresAt = now + 60_000;
  await page.clock.install({ time: now });
  await page.goto('/?view=keys');
  await useKey(page).click();
  await page.unroute('**/api/api-keys');
  await page.route('**/api/api-keys', route => route.abort());
  await page.clock.fastForward(60_001);
  await expect(usage(page)).toHaveCount(0);
  await expect(page.locator('.toast-error')).toContainText('密钥已过期');
});

for (const failure of ['reject', 'timeout'] as const) {
  test(`clipboard ${failure} selects the current config for manual copy`, async ({ page }) => {
    await clipboard(page, failure);
    await mockConsole(page);
    await page.clock.install();
    await page.goto('/?view=keys');
    await useKey(page).click();
    await usage(page).getByRole('button', { name: '复制配置', exact: true }).click();
    if (failure === 'timeout') {
      await expect(usage(page).getByRole('button', { name: '正在复制', exact: true })).toBeDisabled();
      await page.clock.fastForward(2600);
    }
    await expect(usage(page).getByText('自动复制不可用，已选中配置内容，请手动复制。', { exact: true })).toBeVisible();
    await expect(config(page)).toBeFocused();
    expect(await config(page).evaluate((element: HTMLTextAreaElement) => element.value.slice(element.selectionStart, element.selectionEnd))).toBe(await config(page).inputValue());
    await usage(page).getByLabel('模型 ID', { exact: true }).fill('changed-model');
    await expect(usage(page).getByText('自动复制不可用，已选中配置内容，请手动复制。', { exact: true })).toHaveCount(0);
    await expect(usage(page).getByRole('button', { name: '复制配置', exact: true })).toBeEnabled();
  });
}

test('closing while clipboard permission is pending does not notify a later dialog', async ({ page }) => {
  await clipboard(page, 'timeout');
  await mockConsole(page);
  await page.clock.install();
  await page.goto('/?view=keys');
  await useKey(page).click();
  await usage(page).getByRole('button', { name: '复制配置', exact: true }).click();
  await page.keyboard.press('Escape');
  await useKey(page).click();
  await page.clock.fastForward(2600);
  await expect(page.locator('.toast-error')).toHaveCount(0);
  await expect(usage(page).getByRole('button', { name: '复制配置', exact: true })).toBeEnabled();
});

for (const [width, height, colorScheme] of [[1440, 1000, 'light'], [390, 844, 'light'], [390, 844, 'dark']] as const) {
  test(`dialog layout and keyboard focus at ${width}px in ${colorScheme} mode`, async ({ page }, info) => {
    await page.setViewportSize({ width, height });
    await page.emulateMedia({ colorScheme, reducedMotion: 'reduce' });
    const state = await mockConsole(page);
    state.keys[0].name = '较长的客户端名称用于检查弹窗换行与操作按钮';
    await page.goto('/?view=keys');
    await useKey(page, state.keys[0].name).click();
    const dialog = usage(page);
    await expect(dialog).toBeVisible();
    const bounds = await dialog.boundingBox();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
    expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(height);
    expect(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    await dialog.getByRole('button', { name: '复制配置', exact: true }).focus();
    await page.keyboard.press('Tab');
    await expect(dialog.getByRole('button', { name: '关闭使用密钥窗口', exact: true })).toBeFocused();
    await page.keyboard.press('Shift+Tab');
    await expect(dialog.getByRole('button', { name: '复制配置', exact: true })).toBeFocused();
    await dialog.evaluate(element => { element.scrollTop = 0; });
    await page.screenshot({ path: info.outputPath('key-usage.png'), fullPage: true });
    await page.keyboard.press('Escape');
    await expect(useKey(page, state.keys[0].name)).toBeFocused();
    expect(await page.evaluate(() => document.body.style.overflow)).toBe('');
  });
}

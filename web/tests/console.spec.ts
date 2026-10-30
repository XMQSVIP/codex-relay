import { test, expect } from '@playwright/test';
import { mockConsole, nativeId, oauthId, groupId } from './fixtures';

test('navigation, diagnostics links and shared session', async ({ page }) => {
  await mockConsole(page);
  let sessions = 0;
  page.on('request', request => { if (new URL(request.url()).pathname === '/api/session') sessions++; });
  await page.goto('/');
  await expect(page.getByRole('region', { name: '账户列表', exact: true })).toBeVisible();
  const initialSessions = sessions;
  for (const name of ['分组管理', '客户端接入', '请求监控', '用量统计', '系统设置']) {
    await page.locator('aside').getByRole('link', { name, exact: true }).click();
    await expect(page.locator('main h1')).toHaveText(name);
  }
  expect(sessions).toBe(initialSessions);
  await page.goBack();
  await expect(page.locator('main h1')).toHaveText('用量统计');
  await page.goto('/?view=settings&tab=diagnostics');
  await expect(page).toHaveURL(/view=settings&tab=diagnostics/);
  await expect(page.getByRole('heading', { name: '本机环境检查' })).toBeVisible();
  await expect(page.locator('.desktop-debug')).toHaveAttribute('open', '');
  await expect(page.getByRole('button', { name: '启动桌面调试', exact: true })).toBeVisible();
});

test('account filters, pagination, deep links and return location survive reload', async ({ page }) => {
  await mockConsole(page, { count: 35 });
  await page.goto('/');
  await page.getByRole('searchbox', { name: '搜索账户', exact: true }).fill('生产');
  await page.getByRole('button', { name: '下一页', exact: true }).click();
  await expect(page).toHaveURL(/page=2/);
  const link = page.locator('.account-name-line a').first();
  const name = await link.innerText();
  await link.click();
  await expect(page.getByRole('navigation', { name: '账户配置分类' })).toBeVisible();
  await page.getByRole('link', { name: '返回账户列表', exact: true }).click();
  await expect(page.getByRole('searchbox', { name: '搜索账户', exact: true })).toHaveValue('生产');
  await expect(page.locator('.account-name-line a').first()).toHaveText(name);
  await expect(page.locator('.account-console-row:focus')).toHaveCount(1);
  await page.reload();
  await expect(page.getByRole('searchbox', { name: '搜索账户', exact: true })).toHaveValue('生产');
  await expect(page.locator('.pagination')).toContainText('第 2');
});

test('one account switch preserves configuration and replaces scheduling enablement', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=accounts');
  const toggle = page.getByRole('switch', { name: '启用账户：独立测试账户', exact: true });
  await expect(toggle).toBeChecked();
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  expect(state.data.operations.accounts[1].enabled).toBe(false);
  expect(state.data.operations.accounts[1].route?.id).toBe('openai');
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect.poll(() => state.data.operations.accounts[1].route?.id).toBe('openai');
  await page.getByRole('link', { name: '账户设置：独立测试账户', exact: true }).click();
  await page.getByRole('link', { name: '接入与调度', exact: true }).click();
  await expect(page.getByLabel('参与调度')).toHaveCount(0);
  await page.getByRole('switch', { name: '启用账户：独立测试账户', exact: true }).click();
  await expect.poll(() => state.data.operations.accounts[1].enabled).toBe(false);
  expect(state.writes.filter(item => item.path === `/accounts/${oauthId}/activation`)).toHaveLength(3);
  expect(state.writes.some(item => item.path.endsWith('/route') || item.path.endsWith('/policy'))).toBe(false);
});

test('native credentials and access have separate controls and one detection session', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto(`/?view=accounts&account=${nativeId}&section=login`);
  await expect(page.getByRole('button', { name: '重新检测并更新', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '停用接入', exact: true })).toHaveCount(0);
  await expect(page.getByRole('link', { name: '创建 API Key', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: '重新检测并更新', exact: true }).click();
  await expect(page.getByText('已识别本机配置', { exact: true })).toBeVisible();
  const detections = state.writes.filter(item => item.path === '/local-connection/inspect').length;
  await page.getByRole('link', { name: '接入与调度', exact: true }).click();
  await expect(page.getByRole('button', { name: '重新检测', exact: true })).toHaveCount(0);
  await expect(page.getByRole('group', { name: '所属分组', exact: true })).toHaveCount(1);
  await expect(page.getByRole('link', { name: '创建 API Key', exact: true })).toBeVisible();
  const toggle = page.getByRole('switch', { name: '启用账户：本机 Codex', exact: true });
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  expect(state.data.operations.accounts[0].route?.id).toBe('openai');
  await toggle.click();
  await expect(toggle).toBeChecked();
  await expect.poll(() => state.data.operations.accounts[0].route?.id).toBe('openai');
  await page.getByRole('link', { name: '登录与凭据', exact: true }).click();
  await expect(page.getByText('已识别本机配置', { exact: true })).toBeVisible();
  expect(state.writes.filter(item => item.path === '/local-connection/inspect')).toHaveLength(detections);
});

test('detail tab changes retain credential, model and scheduling drafts', async ({ page }) => {
  await mockConsole(page);
  await page.goto(`/?view=accounts&account=${oauthId}&section=login`);
  await page.getByLabel('或粘贴 JSON / Access Token').fill('unsaved-credential');
  await page.getByRole('link', { name: '接入与调度', exact: true }).click();
  await expect(page.getByRole('button', { name: '导入到独立实例', exact: true })).toHaveCount(0);
  await page.getByText('测试连接（可选）', { exact: true }).click();
  await expect(page.getByLabel('测试模型（选填）')).toBeVisible();
  await page.getByLabel('测试模型（选填）').fill('unsaved-model');
  await page.getByRole('checkbox', { name: '开发测试', exact: true }).check();
  await page.getByRole('checkbox', { name: '默认分组', exact: true }).uncheck();
  await page.getByRole('link', { name: '登录与凭据', exact: true }).click();
  await expect(page.getByLabel('或粘贴 JSON / Access Token')).toHaveValue('unsaved-credential');
  await expect(page.getByRole('button', { name: '停用接入', exact: true })).toHaveCount(0);
  await page.getByRole('link', { name: '接入与调度', exact: true }).click();
  await expect(page.getByLabel('测试模型（选填）')).toHaveValue('unsaved-model');
  await expect(page.getByRole('checkbox', { name: '开发测试', exact: true })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: '默认分组', exact: true })).not.toBeChecked();
});

test('account scheduling has one settings entry and retries preserve drafts', async ({ page }) => {
  const state = await mockConsole(page);
  let conflict = true;
  await page.route('**/api/accounts/*/policy', route => {
    if (!conflict) return route.fallback();
    conflict = false;
    return route.fulfill({ status: 409, json: { error: { code: 'CONFIG_CONFLICT', message: '配置已更新，请检查后重试。' } } });
  });
  await page.goto('/');
  await page.getByRole('link', { name: '账户设置：本机 Codex', exact: true }).click();
  await page.getByRole('link', { name: '接入与调度', exact: true }).click();
  await expect(page.getByRole('heading', { name: '账户调度', exact: true })).toHaveCount(1);
  await page.getByRole('checkbox', { name: '开发测试', exact: true }).check();
  await page.getByRole('checkbox', { name: '默认分组', exact: true }).uncheck();
  await page.waitForResponse(response => response.url().endsWith('/api/overview'));
  await expect(page.getByRole('checkbox', { name: '开发测试', exact: true })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: '默认分组', exact: true })).not.toBeChecked();
  await page.getByRole('button', { name: '保存调度', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('配置已更新');
  await expect(page.getByRole('checkbox', { name: '开发测试', exact: true })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: '默认分组', exact: true })).not.toBeChecked();
  await page.getByRole('button', { name: '保存调度', exact: true }).click();
  await expect.poll(() => state.data.operations.accounts[0].groupIds[0]).toBe('20000000-0000-0000-0000-000000000001');
  expect(state.data.operations.accounts[0].enabled).toBe(true);
  await page.getByRole('link', { name: '返回账户列表', exact: true }).click();
  const remove = page.getByRole('button', { name: /^删除账户 本机 Codex/ });
  await remove.click();
  await expect(page.getByRole('dialog', { name: '删除“本机 Codex”？', exact: true })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(remove).toBeFocused();
  expect(state.writes.some(item => item.path === `/accounts/${nativeId}`)).toBe(false);
});

test('stopping a busy account requires a compact confirmation', async ({ page }) => {
  const state = await mockConsole(page);
  let failStop = true;
  await page.route('**/api/accounts/*/stop', route => {
    if (!failStop) return route.fallback();
    failStop = false;
    return route.fulfill({ status: 502, json: { error: { code: 'INSTANCE_STOP', message: '停止失败，请重试。' } } });
  });
  state.data.accounts[0].pid = 12345;
  state.data.accounts[0].state = 'generating';
  state.data.operations.accounts[0].active = 1;
  await page.goto(`/?view=accounts&account=${nativeId}&section=advanced`);
  const stop = page.getByRole('button', { name: '停止实例', exact: true });
  await stop.click();
  const dialog = page.getByRole('dialog', { name: '停止“本机 Codex”？', exact: true });
  await expect(dialog).toBeVisible();
  expect(state.writes.some(item => item.path.endsWith('/stop'))).toBe(false);
  await dialog.getByRole('button', { name: '取消', exact: true }).click();
  await expect(stop).toBeFocused();
  await stop.click();
  await dialog.getByRole('button', { name: '停止当前任务与实例', exact: true }).click();
  await expect(dialog.getByRole('alert')).toHaveText('停止失败，请重试。');
  await dialog.getByRole('button', { name: '停止当前任务与实例', exact: true }).click();
  await expect.poll(() => state.writes.some(item => item.path === `/accounts/${nativeId}/stop`)).toBe(true);
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole('button', { name: '启动实例', exact: true })).toBeFocused();
});

test('native account creation then API key creation', async ({ page }) => {
  const state = await mockConsole(page, { count: 0, emptyKeys: true });
  await page.goto('/');
  await page.getByRole('button', { name: '添加账户', exact: true }).click();
  await page.getByLabel('账户名称', { exact: true }).fill('接入测试账户');
  await page.getByRole('button', { name: '连接并启用', exact: true }).click();
  await expect(page.getByText('接入测试账户 · 已连接并启用')).toBeVisible();
  await page.getByRole('link', { name: '创建 API Key', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.getByLabel('密钥名称', { exact: true }).fill('测试调用方');
  await page.getByRole('button', { name: '创建密钥', exact: true }).click();
  await expect(page.getByRole('dialog')).not.toBeVisible();
  await expect(page.locator('.key-table-name')).toHaveText('测试调用方');
  expect(state.writes.some(item => item.path === '/local-connection/connect')).toBeTruthy();
  expect(state.writes.some(item => item.path === '/api-keys' && item.body.groupId === groupId)).toBeTruthy();
});

test('OAuth account and credentials remain available', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto(`/?view=accounts&account=${oauthId}&section=credentials`);
  await page.getByLabel('或粘贴 JSON / Access Token').fill('{"accessToken":"fixture"}');
  await page.getByRole('button', { name: '导入到独立实例', exact: true }).click();
  await expect(page.getByText('凭据已保存。请在“接入与调度”中启用账户。')).toBeVisible();
  await page.getByRole('button', { name: '使用设备码登录', exact: true }).click();
  await expect(page.getByText('TEST-1234', { exact: true })).toBeVisible();
  expect(state.writes.some(item => item.path.endsWith('/login') && item.body.kind === 'chatgptDeviceCode')).toBeTruthy();
});

test('group edit conflict, recovery and modal focus', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=groups');
  const edit = page.getByRole('button', { name: '编辑分组：默认分组', exact: true });
  await edit.click();
  await page.getByLabel('分组名称', { exact: true }).fill('新分组名');
  state.conflict = true;
  await page.getByRole('button', { name: '保存分组', exact: true }).click();
  await expect(page.getByRole('dialog').getByRole('alert')).toContainText('配置已更新');
  await expect(page.getByLabel('分组名称', { exact: true })).toHaveValue('新分组名');
  await page.getByRole('button', { name: '保存分组', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByRole('heading', { name: '新分组名', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '编辑分组：新分组名', exact: true })).toBeFocused();
});

test('key edits, pause, guide and URL filters', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=keys&q=开发');
  await page.getByRole('button', { name: '编辑开发客户端', exact: true }).click();
  await page.getByRole('dialog').getByLabel('密钥名称', { exact: true }).fill('更新后的客户端');
  await page.getByRole('button', { name: '保存修改', exact: true }).click();
  await expect.poll(() => state.keys[0].name).toBe('更新后的客户端');
  await page.getByRole('searchbox', { name: '搜索密钥名称、前缀或分组' }).fill('更新');
  await expect(page).toHaveURL(url => url.searchParams.get('q') === '更新');
  await page.reload();
  await expect(page.getByRole('searchbox', { name: '搜索密钥名称、前缀或分组' })).toHaveValue('更新');
  await page.getByRole('link', { name: '接入指南', exact: true }).click();
  await expect(page.getByRole('heading', { name: '连接你的客户端' })).toBeVisible();
  await expect(page.locator('.key-list')).toBeHidden();
});

for (const allowPublicAccess of [false, true]) {
  test(`guide shows LAN addresses with external access ${allowPublicAccess}`, async ({ page }) => {
    await mockConsole(page);
    await page.route('**/api/session', route => route.fulfill({ json: { requiresToken: false, passwordEnabled: false, allowPublicAccess, authenticated: true } }));
    await page.route('**/api/api-keys', route => route.fulfill({ json: {
      keys: [], baseUrl: 'http://127.0.0.1:4317/v1', localBaseUrl: 'http://127.0.0.1:4317/v1',
      lanBaseUrls: ['http://192.168.1.20:4317/v1', 'http://10.0.0.20:4317/v1'], networkEndpoints: [], inferenceEnabled: true,
    } }));
    await page.goto('/?view=keys&tab=guide');
    const address = page.getByLabel('HTTP Base URL', { exact: true });
    await expect(address).toHaveValue(allowPublicAccess ? 'http://192.168.1.20:4317/v1' : 'http://127.0.0.1:4317/v1');
    await expect(address.locator('option', { hasText: '局域网' })).toHaveCount(2);
    await address.selectOption('http://10.0.0.20:4317/v1');
    await expect(page.getByLabel('cURL 接入示例')).toContainText('http://10.0.0.20:4317/v1/responses');
    await page.getByText('WebSocket 与监听详情', { exact: true }).click();
    await expect(page.getByLabel('原生 WebSocket', { exact: true })).toHaveValue('ws://10.0.0.20:4317/v1/native');
  });
}

test('queue cancellation and request detail focus', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=requests&tab=queue');
  await page.getByRole('button', { name: '取消排队请求 queue-fi', exact: true }).click();
  await expect.poll(() => state.queue).toBe(false);
  await expect(page.getByText('当前筛选范围没有排队请求。')).toBeVisible();
  await page.getByRole('link', { name: '请求记录', exact: true }).click();
  const detail = page.getByRole('button', { name: '查看请求详情 40000000', exact: true });
  await expect(page.locator('.record-latency').first()).toContainText('首响250 ms');
  await expect(page.locator('.record-latency').first()).toContainText('模型首响200 ms');
  await detail.click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await expect(page.getByRole('dialog')).toContainText('模型首响');
  await page.keyboard.press('Escape');
  await expect(detail).toBeFocused();
});

test('statistics preserve filters when opening matching requests', async ({ page }) => {
  await mockConsole(page);
  await page.goto('/?view=statistics&startDate=2026-10-01&endDate=2026-10-15&groupId=' + groupId);
  await expect(page.locator('.usage-primary-metrics')).toContainText('输入 Token');
  await expect(page.locator('.records-panel')).toHaveCount(0);
  await page.getByRole('link', { name: '查看对应请求记录', exact: true }).click();
  await expect(page).toHaveURL(/view=requests/);
  await expect(page).toHaveURL(/startDate=2026-10-01/);
  await expect(page).toHaveURL(/endDate=2026-10-15/);
  await expect(page).toHaveURL(new RegExp(`groupId=${groupId}`));
});

test.describe('calendar dates and refresh intervals', () => {
  test.use({ timezoneId: 'America/New_York' });
  test('both pages default to today and date ranges respect daylight saving', async ({ page }) => {
    await page.clock.install({ time: new Date('2026-11-02T16:00:00-05:00') });
    const state = await mockConsole(page);
    for (const view of ['statistics', 'requests']) {
      await page.goto(`/?view=${view}`);
      await expect(page.getByLabel('开始日期', { exact: true })).toHaveValue('2026-11-02');
      await expect(page.getByLabel('结束日期', { exact: true })).toHaveValue('2026-11-02');
      await expect.poll(() => state.statisticsQueries.at(-1)?.get('from')).toBe(String(new Date('2026-11-02T00:00:00-05:00').getTime()));
      expect(state.statisticsQueries.at(-1)?.has('hours')).toBe(false);
      await expect(page.getByRole('button', { name: /近.*小时|近.*天/ })).toHaveCount(view === 'statistics' ? 2 : 0);
    }
    for (const [date, hours] of [['2026-03-08', 23], ['2026-11-01', 25]] as const) {
      await page.getByLabel('开始日期', { exact: true }).fill(date);
      await page.getByLabel('结束日期', { exact: true }).fill(date);
      await page.getByRole('button', { name: '筛选日期', exact: true }).click();
      await expect.poll(() => {
        const query = state.statisticsQueries.at(-1);
        return query ? (Number(query.get('to')) - Number(query.get('from')) + 1) / 3600000 : 0;
      }).toBe(hours);
    }
    await page.getByLabel('开始日期', { exact: true }).fill('2026-11-02');
    await page.getByRole('button', { name: '筛选日期', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('结束日期不能早于开始日期');
    await page.getByRole('button', { name: '今天', exact: true }).click();
    await expect(page.getByLabel('开始日期', { exact: true })).toHaveValue('2026-11-02');
  });
  for (const [view, seconds] of [['statistics', 30], ['requests', 30]] as const) {
    test(`${view} refreshes every ${seconds}s and offers only a refresh icon`, async ({ page }) => {
      await page.clock.install();
      const state = await mockConsole(page);
      await page.goto(`/?view=${view}`);
      const refresh = page.getByRole('button', { name: view === 'statistics' ? '刷新用量统计' : '刷新请求记录', exact: true });
      await expect(refresh).toBeEnabled();
      await expect(page.locator('.statistics-update > p')).toContainText(`每 ${seconds} 秒更新`);
      await expect(page.getByRole('combobox', { name: '更新方式' })).toHaveCount(0);
      const count = state.statisticsQueries.length;
      await page.clock.fastForward((seconds - 1) * 1000);
      expect(state.statisticsQueries.length).toBe(count);
      await page.clock.fastForward(1100);
      await expect.poll(() => state.statisticsQueries.length).toBe(count + 1);
      await expect(refresh).toBeEnabled();
      await refresh.click();
      await expect.poll(() => state.statisticsQueries.length).toBe(count + 2);
    });
  }
});

test('polling and manual refresh preserve settings drafts and open dialogs', async ({ page }) => {
  await mockConsole(page);
  await page.goto('/?view=settings');
  await page.getByLabel('总并发模式', { exact: true }).selectOption('manual');
  await page.getByLabel('同时执行上限', { exact: true }).fill('9');
  await page.getByRole('button', { name: '停止中转站', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.waitForResponse(response => response.url().endsWith('/api/overview'));
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByLabel('同时执行上限', { exact: true })).toHaveValue('9');
  await page.getByRole('button', { name: '刷新状态', exact: true }).click();
  await expect(page.getByLabel('同时执行上限', { exact: true })).toHaveValue('9');
});

test('connection failure recovers and session expiry shows login', async ({ page }) => {
  const state = await mockConsole(page);
  state.offline = true;
  await page.goto('/');
  await expect(page.getByRole('button', { name: '重试连接', exact: true })).toBeVisible();
  state.offline = false;
  await page.getByRole('button', { name: '重试连接', exact: true }).click();
  await expect(page.getByRole('button', { name: '添加账户', exact: true })).toBeVisible();
  state.expired = true;
  await page.getByRole('button', { name: '刷新状态', exact: true }).click();
  await expect(page.getByRole('heading', { name: '登录管理后台', exact: true })).toBeVisible();
  const password = '密码🔑'.repeat(256);
  await page.getByLabel('后台密码', { exact: true }).fill(password);
  const login = page.waitForRequest(request => request.url().endsWith('/api/session/login') && request.method() === 'POST');
  await page.getByRole('button', { name: '登录', exact: true }).click();
  expect((await login).postDataJSON()).toEqual({ password });
  await expect(page.getByRole('button', { name: '添加账户', exact: true })).toBeVisible();
});

test('account group-only edits, repeated saves and restore', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto(`/?view=accounts&account=${oauthId}&section=access`);
  await page.getByRole('checkbox', { name: '开发测试', exact: true }).check();
  await page.getByRole('checkbox', { name: '默认分组', exact: true }).uncheck();
  await expect(page.getByRole('button', { name: '保存调度', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '保存调度', exact: true }).click();
  await expect.poll(() => state.data.operations.accounts[1].groupIds[0]).toBe('20000000-0000-0000-0000-000000000001');
  await page.getByRole('checkbox', { name: '默认分组', exact: true }).check();
  await page.getByRole('checkbox', { name: '开发测试', exact: true }).uncheck();
  await page.getByRole('button', { name: '还原调度', exact: true }).click();
  await expect(page.getByRole('checkbox', { name: '开发测试', exact: true })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: '默认分组', exact: true })).not.toBeChecked();
  await page.getByRole('checkbox', { name: '默认分组', exact: true }).check();
  await page.getByRole('checkbox', { name: '开发测试', exact: true }).uncheck();
  await page.getByRole('button', { name: '保存调度', exact: true }).click();
  await expect.poll(() => state.data.operations.accounts[1].groupIds[0]).toBe(groupId);
  expect(state.data.operations.accounts[1].enabled).toBe(true);
});

for (const [name, password] of [['short', '1'], ['long', '密码🔑'.repeat(256)]]) test(`security settings save one visible ${name} password`, async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=settings&tab=security');
  await expect(page.getByRole('switch', { name: '允许公网用户访问 同时控制管理后台和客户端 API' })).toBeChecked();
  await page.getByRole('radio', { name: '密码登录 验证后进入后台' }).check();
  const passwordField = page.getByLabel('设置密码', { exact: true });
  await expect(passwordField).toHaveAttribute('type', 'text');
  await expect(page.getByLabel('确认新密码', { exact: true })).toHaveCount(0);
  await passwordField.fill(password);
  await page.getByRole('button', { name: '保存设置', exact: true }).click();
  await expect(page.getByText('访问设置已保存。', { exact: true })).toBeVisible();
  expect(state.writes.find(item => item.path === '/settings/access')?.body).toEqual({ passwordEnabled: true, allowPublicAccess: true, password });
  await page.getByLabel('当前密码', { exact: true }).fill(password);
  const newPassword = page.getByLabel('新密码（选填）', { exact: true });
  await expect(newPassword).toHaveAttribute('type', 'text');
  await expect(page.getByLabel('确认新密码', { exact: true })).toHaveCount(0);
  await newPassword.fill('2');
  await page.getByRole('button', { name: '保存设置', exact: true }).click();
  await expect.poll(() => state.writes.filter(item => item.path === '/settings/access').length).toBe(2);
  expect(state.writes.filter(item => item.path === '/settings/access')[1].body).toEqual({ passwordEnabled: true, allowPublicAccess: true, currentPassword: password, password: '2' });
  expect(state.writes.some(item => item.path === '/service/stop')).toBe(false);
});

for (const theme of ['light', 'dark'] as const) test(`semantic color contrast ${theme}`, async ({ page }) => {
  await page.emulateMedia({ colorScheme: theme });
  await mockConsole(page);
  await page.goto('/');
  await expect(page.locator('main#main')).toBeVisible();
  const contrasts = await page.evaluate(() => {
    const css = getComputedStyle(document.documentElement);
    const luminance = (name: string) => {
      const hex = css.getPropertyValue(name).trim().replace('#', '');
      const [r, g, b] = [0, 2, 4].map(i => Number.parseInt(hex.slice(i, i + 2), 16) / 255).map(c => c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4);
      return r * .2126 + g * .7152 + b * .0722;
    };
    return [['--ink', '--surface'], ['--muted', '--surface'], ['--muted', '--canvas'], ['--on-accent', '--accent'], ['--link', '--selected'], ['--success', '--success-bg'], ['--warning', '--warning-bg'], ['--error', '--error-bg']].map(([fg, bg]) => {
      const a = luminance(fg), b = luminance(bg);
      return { pair: `${fg}/${bg}`, ratio: (Math.max(a, b) + .05) / (Math.min(a, b) + .05) };
    });
  });
  for (const item of contrasts) expect(item.ratio, item.pair).toBeGreaterThanOrEqual(4.5);
});

test('empty states do not fabricate activity', async ({ page }) => {
  await mockConsole(page, { count: 0, emptyRecords: true, emptyKeys: true });
  await page.goto('/');
  await expect(page.getByRole('heading', { name: '添加第一个账户', exact: true })).toBeVisible();
  await page.goto('/?view=statistics');
  await expect(page.getByText('当前范围暂无模型请求', { exact: true })).toBeVisible();
  await expect(page.locator('.chart-column:not(.is-empty)')).toHaveCount(0);
  await page.goto('/?view=requests');
  await expect(page.getByRole('heading', { name: '当前范围没有请求记录' })).toBeVisible();
});

const routes = ['accounts', 'groups', 'keys', 'keys&tab=guide', 'requests', 'requests&tab=queue', 'statistics', 'settings', 'settings&tab=security', 'settings&tab=diagnostics', `accounts&account=${nativeId}`, `accounts&account=${nativeId}&section=access`, `accounts&account=${oauthId}&section=access`, `accounts&account=${oauthId}`, `accounts&account=${nativeId}&section=advanced`];
for (const width of [375, 768, 1024, 1440, 2560]) for (const theme of ['light', 'dark'] as const) {
  test(`layout ${width}px ${theme}: all pages, long names and dialogs`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
    await mockConsole(page, { count: 25, longNames: true });
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    for (const route of routes) {
      await page.goto(`/?view=${route}`);
      await expect(page.locator('main#main')).toBeVisible();
      await expect(page.getByText('正在读取账户与运行状态…', { exact: true })).toHaveCount(0);
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth), { message: route }).toBeLessThanOrEqual(1);
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await page.screenshot({ path: info.outputPath(`${route.replace(/[^a-z]/g, '-')}.png`), fullPage: false });
    }
    await page.goto('/?view=accounts');
    await page.screenshot({ path: info.outputPath('account-actions.png') });
    const remove = page.getByRole('button', { name: /^删除账户/ }).first();
    await remove.click();
    const deleteDialog = page.getByRole('dialog');
    await expect(deleteDialog).toBeVisible();
    const deleteBox = await deleteDialog.boundingBox();
    expect(deleteBox!.x).toBeGreaterThanOrEqual(0); expect(deleteBox!.x + deleteBox!.width).toBeLessThanOrEqual(width);
    await page.screenshot({ path: info.outputPath('account-delete-dialog.png') });
    await page.keyboard.press('Escape');
    await expect(remove).toBeFocused();
    await page.goto('/?view=keys');
    await page.getByRole('button', { name: '创建 API Key', exact: true }).click();
    const box = await page.getByRole('dialog').boundingBox();
    expect(box!.x).toBeGreaterThanOrEqual(0); expect(box!.x + box!.width).toBeLessThanOrEqual(width);
    await page.screenshot({ path: info.outputPath('key-dialog.png'), fullPage: false });
    await page.keyboard.press('Escape');
    if (width < 761) {
      await page.getByRole('button', { name: '展开导航菜单', exact: true }).click();
      await expect(page.locator('aside')).toBeVisible();
      await page.keyboard.press('Escape');
      await expect(page.getByRole('button', { name: '展开导航菜单', exact: true })).toBeFocused();
    }
    expect(errors).toEqual([]);
  });
}

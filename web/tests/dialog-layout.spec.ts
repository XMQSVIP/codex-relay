import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';
import { mockConsole, oauthId } from './fixtures';

const dialogs = [
  { name: 'shared modal', view: 'groups', trigger: '新建分组', close: '关闭窗口' },
  { name: 'create account', view: 'accounts', trigger: '添加账户', close: '关闭添加账户' },
  { name: 'create key', view: 'keys', trigger: '创建 API Key', close: '关闭创建窗口' },
  { name: 'edit key', view: 'keys', trigger: '编辑开发客户端', close: '关闭编辑窗口' },
  { name: 'use key', view: 'keys', trigger: '使用密钥：开发客户端', close: '关闭使用密钥窗口' },
  { name: 'request details', view: 'requests', trigger: '查看请求详情 40000000', close: '关闭请求详情' },
  { name: 'delete account', view: 'accounts', trigger: /^删除账户 独立测试账户/, close: '关闭删除账户窗口', confirmation: true },
  { name: 'clear records', view: 'requests', trigger: '清空所有记录', close: '关闭清空记录窗口', confirmation: true },
  { name: 'stop service', view: 'groups', trigger: '停止中转站', close: '关闭停止服务窗口', confirmation: true },
] as const;

async function showNavigation(page: Page) {
  if ((page.viewportSize()?.width ?? 1440) > 760) return;
  const menu = page.getByRole('button', { name: '展开导航菜单', exact: true });
  await menu.click();
  await expect(menu).toHaveCount(0);
  await expect(page.getByRole('button', { name: '收起导航菜单', exact: true })).toHaveAttribute('aria-expanded', 'true');
}

for (const width of [375, 1440]) {
  for (const scenario of dialogs) {
    test(`${scenario.name} keeps its close button fixed after scrolling at ${width}px`, async ({ page }, info) => {
      const height = 'confirmation' in scenario || scenario.name === 'shared modal' ? 240 : 400;
      await page.setViewportSize({ width, height });
      await page.emulateMedia({ colorScheme: width === 375 ? 'dark' : 'light' });
      const state = await mockConsole(page);
      await page.goto(`/?view=${scenario.view}`);
      if (scenario.name === 'stop service') await showNavigation(page);
      const trigger = page.getByRole('button', { name: scenario.trigger, exact: typeof scenario.trigger === 'string' });
      await trigger.click();
      const dialog = page.locator('dialog[open]');
      if (scenario.name === 'create account') await dialog.getByRole('radio', { name: 'API Key', exact: true }).check();
      const close = dialog.getByRole('button', { name: scenario.close, exact: true });
      await expect(close).toBeInViewport({ ratio: 1 });
      const before = await close.boundingBox();
      const body = dialog.locator(':scope > .dialog-body');
      await expect.poll(() => body.evaluate(element => element.scrollHeight - element.clientHeight)).toBeGreaterThan(0);
      await body.hover();
      await page.mouse.wheel(0, 10_000);
      await expect.poll(() => body.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
      await expect(close).toBeInViewport({ ratio: 1 });
      const after = await close.boundingBox();
      expect(Math.abs(after!.y - before!.y)).toBeLessThanOrEqual(1);
      expect(await dialog.evaluate(element => element.scrollTop)).toBe(0);
      expect(await dialog.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
      expect(await close.evaluate(element => {
        const rect = element.getBoundingClientRect();
        return element.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2));
      })).toBe(true);
      await dialog.screenshot({ path: info.outputPath('scrolled-dialog.png') });
      await close.click();
      await expect(dialog).not.toBeVisible();
      await expect(trigger).toBeFocused();
      expect(state.writes.filter(write => write.path !== '/local-connection/inspect')).toHaveLength(0);
    });
  }
}

test('a long account name leaves the close button visible in a short viewport', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 400 });
  const state = await mockConsole(page);
  const name = '账户用途'.repeat(100);
  state.data.accounts.find(account => account.id === oauthId)!.name = name;
  state.data.operations.accounts.find(account => account.id === oauthId)!.name = name;
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: `删除账户 ${name} ${oauthId.slice(0, 8)}`, exact: true }).click();
  const dialog = page.getByRole('dialog');
  const close = dialog.getByRole('button', { name: '关闭删除账户窗口', exact: true });
  await expect(close).toBeInViewport({ ratio: 1 });
  await expect(dialog.getByRole('button', { name: '取消', exact: true })).toBeFocused();
  await close.click();
  await expect(dialog).not.toBeVisible();
  expect(state.writes).toHaveLength(0);
});

for (const scenario of [
  { view: 'accounts', trigger: /^删除账户 独立测试账户/, close: '关闭删除账户窗口', confirm: '确认删除', cancel: '取消', path: `/accounts/${oauthId}` },
  { view: 'requests', trigger: '清空所有记录', close: '关闭清空记录窗口', confirm: '确认清空全部记录', cancel: '取消', path: '/requests' },
  { view: 'groups', trigger: '停止中转站', close: '关闭停止服务窗口', confirm: '确认停止', cancel: '继续运行', path: '/service/stop' },
] as const) {
  test(`${scenario.close} stays disabled while confirmation is pending`, async ({ page }) => {
    const state = await mockConsole(page);
    state.data.operations.active = 0;
    state.data.operations.queued = 0;
    let release!: () => void;
    const pending = new Promise<void>(resolve => { release = resolve; });
    await page.route(`**/api${scenario.path}`, async route => {
      if (route.request().method() === 'GET') return route.fallback();
      await pending;
      await route.fulfill({ status: 503, json: { error: { message: '请重试。' } } });
    });
    await page.goto(`/?view=${scenario.view}`);
    await page.getByRole('button', { name: scenario.trigger, exact: typeof scenario.trigger === 'string' }).click();
    const dialog = page.locator('dialog[open]');
    await expect(dialog.getByRole('button', { name: scenario.cancel, exact: true })).toBeFocused();
    await dialog.getByRole('button', { name: scenario.confirm, exact: true }).click();
    const close = dialog.getByRole('button', { name: scenario.close, exact: true });
    try {
      await expect(close).toBeDisabled();
      await page.keyboard.press('Escape');
      await expect(dialog).toBeVisible();
    } finally { release(); }
    await expect(close).toBeEnabled();
    await close.click();
    await expect(dialog).not.toBeVisible();
  });
}

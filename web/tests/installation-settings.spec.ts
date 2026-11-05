import { test, expect } from '@playwright/test';
import { installationSchema } from '../contracts/admin';
import { mockConsole } from './fixtures';

test('standalone Codex needs only one path and disables desktop-only actions', async ({ page }) => {
  const state = await mockConsole(page);
  const initial = state.data.installation!;
  state.data.installation = null;
  let submitted: any;
  await page.route('**/api/jobs', route => {
    submitted = route.request().postDataJSON();
    state.data.installation = { ...initial, ...submitted.paths, manuallyConfigured: true, desktopExecutable: null, archive: null };
    state.data.jobs.push({ id: 'standalone', kind: 'prepare', state: 'completed', createdAt: Date.now(), report: null, error: null });
    return route.fulfill({ status: 202, json: { id: 'standalone' } });
  });
  await page.goto('/?view=settings&tab=diagnostics');
  const panel = page.getByRole('region', { name: '原生运行环境' });
  await page.getByLabel('运行模式').selectOption('standalone');
  await expect(page.getByLabel('运行模式')).toHaveValue('standalone');
  await expect(page.getByLabel('运行模式').locator('option')).toHaveText(['自动模式', '独立模式']);
  await expect(page.getByLabel('桌面程序路径', { exact: true })).toHaveCount(0);
  await expect(page.getByLabel('应用归档路径（app.asar）')).toHaveCount(0);
  await page.getByLabel('Codex 路径', { exact: true }).fill('D:\\tools\\codex.exe');
  await panel.getByRole('button', { name: '校验并保存路径', exact: true }).click();
  await expect(panel.getByRole('status')).toHaveText('安装路径已校验并保存。');
  expect(submitted).toEqual({ kind: 'prepare', paths: { appServerExecutable: 'D:\\tools\\codex.exe' } });
  await expect(page.getByRole('button', { name: '启动桌面调试', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '检查桌面：桌面检查', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '读取配置：读取提供方', exact: true })).toBeEnabled();
  await page.reload();
  await expect(panel.getByText('D:\\tools\\codex.exe', { exact: true })).toBeVisible();
  await expect(panel).not.toContainText('应用归档路径');
});

test('installation paths are visible and manual settings survive reload without losing drafts to polling', async ({ page }) => {
  const state = await mockConsole(page);
  const original = installationSchema.parse(state.data.installation);
  let submitted: any;
  await page.route('**/api/jobs', async route => {
    submitted = route.request().postDataJSON();
    state.data.installation = { ...original, ...submitted.paths, manuallyConfigured: true, desktopExecutable: null, archive: null };
    state.data.jobs.push({ id: 'manual', kind: 'prepare', state: 'completed', createdAt: Date.now(), report: null, error: null });
    await route.fulfill({ status: 202, json: { id: 'manual' } });
  });
  await page.goto('/?view=settings&tab=diagnostics');
  const panel = page.getByRole('region', { name: '原生运行环境' });
  await expect(panel.getByText(original.desktopExecutable!, { exact: true })).toBeVisible();
  await expect(panel.getByText(original.appServerExecutable, { exact: true })).toBeVisible();
  await expect(panel.getByText(original.archive!, { exact: true })).toBeVisible();
  await page.getByLabel('运行模式').selectOption('standalone');
  await expect(page.getByLabel('Codex 路径', { exact: true })).toHaveValue(original.appServerExecutable);
  await expect(panel).not.toContainText('应用归档路径');
  const desktop = 'D:\\自定义安装\\codex.exe';
  await page.getByLabel('Codex 路径', { exact: true }).fill(`  ${desktop}  `);
  const calls = state.overviewCalls;
  await expect.poll(() => state.overviewCalls, { timeout: 7000 }).toBeGreaterThan(calls);
  await expect(page.getByLabel('Codex 路径', { exact: true })).toHaveValue(`  ${desktop}  `);
  await panel.getByRole('button', { name: '校验并保存路径', exact: true }).click();
  await expect(panel.getByRole('status')).toHaveText('安装路径已校验并保存。');
  expect(submitted).toEqual({ kind: 'prepare', paths: { appServerExecutable: desktop } });
  await page.reload();
  await expect(panel.getByText(desktop, { exact: true })).toBeVisible();
  await expect(panel).toContainText('独立模式');
});

test('invalid paths show inline errors, preserve saved paths and allow retry', async ({ page }) => {
  const state = await mockConsole(page);
  const original = state.data.installation!;
  await page.route('**/api/jobs', route => route.fulfill({ status: 400, json: { error: { code: 'INVALID_INPUT', message: 'Codex 路径必须是已存在的文件绝对路径。' } } }));
  await page.goto('/?view=settings&tab=diagnostics');
  const panel = page.getByRole('region', { name: '原生运行环境' });
  await page.getByLabel('运行模式').selectOption('standalone');
  await page.getByLabel('Codex 路径', { exact: true }).fill('missing');
  await panel.getByRole('button', { name: '校验并保存路径', exact: true }).click();
  await expect(panel.getByRole('alert')).toContainText('文件绝对路径');
  await expect(panel.getByText(original.appServerExecutable, { exact: true })).toBeVisible();
  await expect(panel.getByRole('button', { name: '校验并保存路径', exact: true })).toBeEnabled();
  await page.getByLabel('Codex 路径', { exact: true }).fill('   ');
  await expect(panel.getByRole('button', { name: '校验并保存路径', exact: true })).toBeDisabled();
});

test('automatic detection omits overrides and reports asynchronous validation failure', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.installation = null;
  let submitted: any;
  await page.route('**/api/jobs', route => {
    submitted = route.request().postDataJSON();
    state.data.jobs.push({ id: 'auto', kind: 'prepare', state: 'running', createdAt: Date.now(), report: null, error: null });
    state.data.busy = true;
    return route.fulfill({ status: 202, json: { id: 'auto' } });
  });
  await page.goto('/?view=settings&tab=diagnostics');
  const panel = page.getByRole('region', { name: '原生运行环境' });
  await panel.getByRole('button', { name: '检测安装', exact: true }).click();
  await expect(panel.getByRole('status')).toHaveText('正在校验原生安装…');
  await expect(page.getByLabel('运行模式')).toBeDisabled();
  expect(submitted).toEqual({ kind: 'prepare' });
  state.data.jobs[0].state = 'failed';
  state.data.jobs[0].error = { code: 'DESKTOP_SELECTION', message: '无法确定唯一桌面。' };
  state.data.busy = false;
  await expect(panel.getByRole('alert')).toContainText('无法确定唯一桌面');
  await expect(page.getByLabel('运行模式')).toBeEnabled();
});

test('long installation paths fit mobile screens and shutdown disables editing', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.installation!.appServerExecutable = `C:\\${'long directory\\'.repeat(25)}codex.exe`;
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/?view=settings&tab=diagnostics');
  const panel = page.getByRole('region', { name: '原生运行环境' });
  await page.getByLabel('运行模式').selectOption('standalone');
  await expect(page.getByLabel('Codex 路径', { exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await panel.screenshot({ path: '../.cache/installation-settings-mobile.png' });
  state.data.operations.draining = true;
  await page.reload();
  await expect(page.getByLabel('运行模式')).toBeDisabled();
  await expect(panel.getByRole('button', { name: '重新检测安装', exact: true })).toBeDisabled();
});

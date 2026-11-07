import { test, expect } from '@playwright/test';
import type { Login } from '../src/api';
import { mockConsole, oauthId } from './fixtures';

test('login restores after refresh, forwards callback and stops polling at success', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await mockConsole(page);
  let login: Login | null = null;
  let reads = 0;
  await page.route(`**/api/accounts/${oauthId}/login**`, async route => {
    const request = route.request(), path = new URL(request.url()).pathname;
    if (request.method() === 'GET') { reads++; return route.fulfill({ json: login }); }
    if (path.endsWith('/callback')) {
      expect(request.postDataJSON()).toEqual({ loginId: 'test-login', callbackUrl: 'http://127.0.0.1:1455/auth/callback?code=one&state=test' });
      login = { ...login!, status: 'completing' }; return route.fulfill({ status: 202, json: login });
    }
    login = { type: 'chatgpt', loginId: 'test-login', status: 'waiting', startedAt: Date.now(), expiresAt: Date.now() + 600000, authUrl: 'https://auth.openai.com/authorize', manualCallbackAvailable: true };
    return route.fulfill({ json: login });
  });
  await page.goto(`/?view=accounts&account=${oauthId}`);
  await page.getByRole('button', { name: '使用 ChatGPT 登录', exact: true }).click();
  await expect(page.getByRole('heading', { name: '等待浏览器授权' })).toBeVisible();
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.getByRole('button', { name: '复制授权链接', exact: true }).click();
  await expect(page.getByText('授权链接已复制。', { exact: true })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('https://auth.openai.com/authorize');
  await page.reload();
  await expect(page.getByRole('heading', { name: '等待浏览器授权' })).toBeVisible();
  await page.getByText('已授权但没有反应？', { exact: true }).click();
  await page.getByLabel('完整回调地址').fill('http://127.0.0.1:1455/auth/callback?code=one&state=test');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
  await page.screenshot({ path: '../.cache/login-callback-mobile.png', fullPage: true });
  await page.getByRole('button', { name: '提交回调地址' }).click();
  await expect(page.getByRole('heading', { name: '正在确认登录结果' })).toBeVisible();
  login = { ...login!, status: 'succeeded', manualCallbackAvailable: false, authUrl: undefined };
  await expect(page.getByRole('heading', { name: '登录成功，凭据已保存' })).toBeVisible();
  await expect(page.getByRole('link', { name: '前往接入与调度' })).toBeVisible();
  await expect(page.getByRole('link', { name: '打开授权页面' })).toHaveCount(0);
  const completedReads = reads;
  await page.waitForTimeout(2400);
  expect(reads).toBe(completedReads);
});

test('device login recovers status connection and cancellation remains terminal', async ({ page }) => {
  await mockConsole(page);
  let offline = false;
  let login: Login | null = null;
  await page.route(`**/api/accounts/${oauthId}/login**`, async route => {
    if (route.request().method() === 'GET') return offline ? route.abort() : route.fulfill({ json: login });
    if (new URL(route.request().url()).pathname.endsWith('/cancel')) { login = { ...login!, status: 'cancelled' }; return route.fulfill({ json: { ok: true } }); }
    login = { type: 'chatgptDeviceCode', loginId: 'device-login', status: 'waiting', startedAt: Date.now(), expiresAt: Date.now() + 600000, verificationUrl: 'https://auth.openai.com/device', userCode: 'ABCD-1234', manualCallbackAvailable: false };
    return route.fulfill({ json: login });
  });
  await page.goto(`/?view=accounts&account=${oauthId}`);
  await page.getByRole('button', { name: '使用设备码登录', exact: true }).click();
  await expect(page.getByText('ABCD-1234', { exact: true })).toBeVisible();
  offline = true;
  await expect(page.getByText('状态连接中断，正在重试；这不代表授权失败。')).toBeVisible();
  offline = false;
  await expect(page.getByText('状态连接中断，正在重试；这不代表授权失败。')).toHaveCount(0);
  await page.getByRole('button', { name: '取消本次登录' }).click();
  await expect(page.getByRole('heading', { name: '本次登录已取消' })).toBeVisible();
  await expect(page.getByText('ABCD-1234', { exact: true })).toHaveCount(0);
});

test('narrow screen shows failure clearly', async ({ page }) => {
  await mockConsole(page);
  await page.setViewportSize({ width: 375, height: 812 });
  await page.route(`**/api/accounts/${oauthId}/login**`, route => route.fulfill({ json: { type: 'chatgpt', loginId: 'failed', status: 'failed', startedAt: 0, expiresAt: 0, manualCallbackAvailable: false, error: { code: 'LOGIN_AUTH_FAILED', message: '原生授权未完成，请重新登录。' } } }));
  await page.goto(`/?view=accounts&account=${oauthId}`);
  await expect(page.getByRole('heading', { name: '登录失败', exact: true })).toBeVisible();
  await expect(page.getByRole('alert')).toContainText('原生授权未完成');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
  await page.screenshot({ path: '../.cache/login-mobile.png', fullPage: true });
});

test('port conflict leaves device login available and missing session ends waiting', async ({ page }) => {
  await mockConsole(page);
  let started = false, expired = false;
  await page.route(`**/api/accounts/${oauthId}/login**`, async route => {
    if (route.request().method() === 'GET') {
      if (expired) return route.fulfill({ status: 410, json: { error: { code: 'LOGIN_EXPIRED', message: '会话失效' } } });
      return route.fulfill({ json: started ? { type: 'chatgptDeviceCode', loginId: 'device', status: 'waiting', startedAt: Date.now(), expiresAt: Date.now() + 600000, verificationUrl: 'https://auth.openai.com/device', userCode: 'TEST', manualCallbackAvailable: false } : null });
    }
    if (route.request().postDataJSON().kind === 'chatgpt') return route.fulfill({ status: 409, json: { error: { code: 'LOGIN_PORT_BUSY', message: '本地登录回调端口 1455 不可用，请使用设备码。' } } });
    started = true;
    return route.fulfill({ json: { type: 'chatgptDeviceCode', loginId: 'device', status: 'waiting', startedAt: Date.now(), expiresAt: Date.now() + 600000, verificationUrl: 'https://auth.openai.com/device', userCode: 'TEST', manualCallbackAvailable: false } });
  });
  await page.goto(`/?view=accounts&account=${oauthId}`);
  await page.getByRole('button', { name: '使用 ChatGPT 登录', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('1455');
  await page.getByRole('button', { name: '使用设备码登录', exact: true }).click();
  await expect(page.getByText('TEST', { exact: true })).toBeVisible();
  expired = true;
  await expect(page.getByRole('alert')).toContainText('登录会话已失效，请重新发起');
  await expect(page.getByRole('button', { name: '使用 ChatGPT 登录', exact: true })).toBeEnabled();
});

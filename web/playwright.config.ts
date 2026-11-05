import { defineConfig } from '@playwright/test';
import { testWorkers } from './test-workers';

// 独立测试端口避免与用户已运行的页面冲突；始终启动自己的服务，不复用现有实例。
const port = Number(process.env.BRIDGE_TEST_WEB_PORT || 5174);
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Invalid BRIDGE_TEST_WEB_PORT');
const baseURL = `http://127.0.0.1:${port}`;
const workers = testWorkers();
console.log(`[Playwright] Using ${workers} worker(s), limited by available memory and CPUs.`);

export default defineConfig({
  testDir: './tests',
  outputDir: '../.cache/ui-tests',
  timeout: 45_000,
  expect: { timeout: 6_000 },
  forbidOnly: !!process.env.CI,
  fullyParallel: true,
  workers,
  reporter: [['list']],
  // 本地复用已安装的 Chrome；CI 安装与 Playwright 版本配套的 Chromium。
  use: { baseURL, channel: process.env.CI ? undefined : 'chrome', viewport: { width: 1440, height: 1000 }, trace: 'retain-on-failure', screenshot: 'only-on-failure' },
  webServer: { command: `pnpm exec vite --host 127.0.0.1 --port ${port} --strictPort`, url: baseURL, reuseExistingServer: false },
});

import { chromium } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import { resolve } from 'node:path';

const origin = process.argv[2] ?? 'http://127.0.0.1:5173';
if (!['127.0.0.1', 'localhost'].includes(new URL(origin).hostname)) throw new Error('This smoke check only reads the local console.');
const output = resolve('../.cache/ui-real');
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: 'chrome' });
const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: 'light' });
const page = await context.newPage();
const errors = [];
const writes = [];
page.on('pageerror', error => errors.push(error.message));
page.on('response', response => { if (new URL(response.url()).pathname.startsWith('/api/') && response.status() >= 400) errors.push(`${new URL(response.url()).pathname}: ${response.status()}`); });
await page.route('**/api/**', async route => {
  if (route.request().method() === 'GET') return route.continue();
  writes.push(new URL(route.request().url()).pathname);
  return route.abort('blockedbyclient');
});
try {
  const routes = ['accounts', 'groups', 'keys', 'keys&tab=guide', 'requests', 'requests&tab=queue', 'statistics', 'settings', 'settings&tab=security', 'settings&tab=diagnostics'];
  for (const width of [1440, 375]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const view of routes) {
      await page.goto(`${origin}/?view=${view}`);
      await page.locator('main#main').waitFor();
      await page.locator('main .loading').waitFor({ state: 'hidden' });
      await page.waitForFunction(() => !document.querySelector('main [aria-busy="true"]'));
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - innerWidth);
      if (overflow > 1) throw new Error(`Overflow at ${width}px in ${view}: ${overflow}`);
      // Keep real secrets out of captured images.
      if (view === 'accounts' || view === 'statistics') await page.screenshot({ path: resolve(output, `${width}-${view}.png`) });
    }
  }
  if (errors.length || writes.length) throw new Error(JSON.stringify({ errors, blockedWrites: writes }));
  console.log(`Read-only smoke passed: ${routes.length * 2} page/viewport combinations. No write requests or browser exceptions.`);
} finally {
  await browser.close();
}

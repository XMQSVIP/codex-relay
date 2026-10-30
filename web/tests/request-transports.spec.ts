import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const width of [375, 1024, 1280, 1440, 1920]) {
  test(`request transport columns distinguish both connections at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    const state = await mockConsole(page);
    const original = state.records[0];
    state.records.splice(0, state.records.length,
      { ...original, upstreamTransport: 'ws', clientTransport: 'sse' },
      { ...original, id: 'http', endpoint: '/v1/responses/compact', upstreamTransport: 'http', clientTransport: 'http' },
      { ...original, id: 'native', upstreamTransport: 'native', clientTransport: 'ws' },
      { ...original, id: 'unknown', endpoint: null, upstreamTransport: null, clientTransport: null },
    );
    await page.goto('/?view=requests');
    const table = page.locator('.records-table');
    if (width > 760) for (const name of ['连接信息']) await expect(table.getByRole('columnheader', { name, exact: true })).toBeAttached();
    await expect(table.locator('thead')).not.toContainText('响应类型');
    const rows = table.locator('tbody tr');
    await expect(rows).toHaveCount(4);
    await expect(table.locator('.record-endpoint')).toHaveText(['/v1/responses', '/v1/responses/compact', '/v1/responses', '未知']);
    await expect(table.locator('.record-upstream')).toHaveText(['WS', 'HTTP', '原生', '未知']);
    await expect(table.locator('.record-client')).toHaveText(['SSE', 'HTTP', 'WS', '未知']);
    const scroll = page.locator('.records-scroll');
    if (width >= 1280 || width <= 760) {
      expect(await scroll.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    } else {
      await scroll.evaluate(el => { el.scrollLeft = el.scrollWidth; });
      await expect(rows.first().getByRole('button')).toBeInViewport();
    }
    await table.locator('.record-client').first().scrollIntoViewIfNeeded();
    await expect(table.locator('.record-client').first()).toBeInViewport();
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1);
    await page.screenshot({ path: `../.cache/ui-tests/request-transports-${width}.png`, fullPage: true });
  });
}

import { expect, test } from '@playwright/test';
import { exactTokens, formatTokens } from '../src/tokenFormat';
import { mockConsole, nativeId } from './fixtures';

test('Token formatting preserves small, zero and unknown values and rounds K/M/B boundaries', () => {
  for (const [input, expected] of [
    [0, '0'], [999, '999'], [1000, '1K'], [1250, '1.25K'],
    [999994, '999.99K'], [999995, '1M'],
    [1000000, '1M'], [1250000, '1.25M'], [1234567, '1.23M'],
    [999994999, '999.99M'], [999995000, '1B'],
    [1000000000, '1B'], [1250000000, '1.25B'], [1500000000, '1.5B'],
    [null, '未知'], [undefined, '未知'], [NaN, '未知'], [Infinity, '未知'],
  ] as const) expect(formatTokens(input)).toBe(expected);
  expect(formatTokens(null, '—')).toBe('—');
  expect(exactTokens(1234567)).toBe('1,234,567');
  expect(exactTokens(1250000000)).toBe('1,250,000,000');
});

for (const width of [390, 1440]) {
  test(`accounts, rankings and requests abbreviate tokens and retain exact values at ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    const state = await mockConsole(page);
    Object.assign(state.records[0], { inputTokens: 1_250_000, outputTokens: 1_500_000_000, cachedInputTokens: 1_000_000 });
    await page.goto(`/?view=accounts&expanded=${nativeId}`);
    const accountTotal = page.locator(`#account-${nativeId} .account-usage-cell strong`);
    await expect(accountTotal).toHaveText('1.5B');
    await expect(accountTotal.locator('span')).toHaveAttribute('title', '1,501,250,000 Token');
    const details = page.locator('.account-token-grid');
    await expect(details.locator('dd').nth(0)).toHaveText('1.25M');
    await expect(details.locator('dd').nth(1)).toHaveText('1.5B');
    await expect(details.locator('dd').nth(2)).toHaveText('1M');
    await expect(page.locator('.account-model-metrics tbody td').last()).toHaveText('1.5B');

    await page.goto('/?view=statistics');
    const ranking = page.locator('.usage-ranking tbody tr');
    await expect(ranking.locator('td').nth(0).locator('strong')).toHaveText('1');
    await expect(ranking.locator('td').nth(1)).toHaveText('1.25M');
    await expect(ranking.locator('td').nth(2)).toHaveText('1.5B');
    await expect(ranking.locator('.usage-total strong')).toHaveText('1.5B');
    await expect(ranking.locator('.usage-total strong span')).toHaveAttribute('title', '1,501,250,000 Token');
    await page.screenshot({ path: info.outputPath('compact-token-usage.png'), fullPage: true });

    await page.goto('/?view=requests');
    await expect(page.locator('.token-input')).toContainText('1.25M');
    await expect(page.locator('.token-output')).toContainText('1.5B');
    await expect(page.locator('.token-cache')).toContainText('1M');
    await page.getByRole('button', { name: /^查看请求详情/ }).click();
    const dialog = page.getByRole('dialog', { name: '请求详情', exact: true });
    await expect(dialog.locator('.request-fields')).toContainText('1,250,000');
    await expect(dialog.locator('.request-fields')).toContainText('1,500,000,000');
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
}

test('usage overview abbreviates tokens without changing request counts or cache hit rates', async ({ page }) => {
  await mockConsole(page);
  await page.route('**/api/statistics?**', route => route.fulfill({ json: {
    from: Date.now() - 1000, to: Date.now(), page: 1, pageSize: 50, records: [], trend: [], accountUsage: [], keyUsage: [], modelUsage: [],
    summary: { total: 1250, succeeded: 1250, failed: 0, rejected: 0, cancelled: 0, running: 0, queued: 0, timedOut: 0, queueFull: 0,
      averageWaitMs: 0, averageMs: 1500, averageFirstResponseMs: 250, averageModelFirstResponseMs: 200, averageTotalMs: 1500, totalTokens: 1_501_250_000, inferenceRequests: 1250, inferenceSucceeded: 1250, inferenceFailed: 0, usageKnownRequests: 1250, cacheKnownRequests: 1250,
      inputTokens: 1_250_000, outputTokens: 1_500_000_000, cachedInputTokens: 1_000_000, cacheKnownInputTokens: 1_250_000, cacheHitRate: .8 },
  } }));
  await page.goto('/?view=statistics');
  const values = page.locator('.usage-metric-copy > strong');
  await expect(values).toHaveText(['1,250', '1.5B', '80%']);
  await expect(page.locator('.usage-metric-card').nth(1).locator('.usage-metric-footer span[title]').first()).toHaveAttribute('title', '1,250,000 Token');
  await expect(page.locator('.usage-coverage')).toContainText('1,250 / 1,250');
  await expect(page.locator('.usage-metric-copy > strong').first()).toHaveText('1,250');
});

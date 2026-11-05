import { test, expect } from '@playwright/test';
import { mockConsole, nativeId } from './fixtures';

for (const slow of ['overview', 'metrics'] as const) {
  test(`account refresh shows progress until ${slow} completes even when data is unchanged`, async ({ page }, info) => {
    await page.clock.install();
    const state = await mockConsole(page);
    await page.goto('/?view=accounts');
    await expect(page.locator('.account-usage-cell strong').first()).toHaveText('125');
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    const calls = { overview: 0, metrics: 0 };
    await page.route('**/api/overview', async route => {
      calls.overview++;
      if (slow === 'overview') await gate;
      await route.fallback();
    });
    await page.route('**/api/accounts/metrics?*', async route => {
      calls.metrics++;
      if (slow === 'metrics') await gate;
      await route.fallback();
    });
    await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
    try {
      const pending = page.getByRole('button', { name: '正在刷新账号数据…', exact: true });
      await expect(pending).toBeDisabled();
      await expect(pending).toHaveAttribute('aria-busy', 'true');
      await pending.evaluate((button: HTMLButtonElement) => { button.click(); button.click(); });
      await expect.poll(() => calls).toEqual({ overview: 1, metrics: 1 });
      await expect(page.getByRole('status').filter({ hasText: '账号数据已刷新' })).toHaveCount(0);
      if (slow === 'metrics') {
        await page.setViewportSize({ width: 375, height: 812 });
        await page.screenshot({ path: info.outputPath('refreshing-mobile.png'), animations: 'disabled' });
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      }
    } finally { release(); }
    await expect(page.getByRole('status').filter({ hasText: '账号数据已刷新' })).toBeVisible();
    await expect(page.getByRole('button', { name: '刷新账号数据', exact: true })).toBeEnabled();
    expect(calls).toEqual({ overview: 1, metrics: 1 });
    expect(state.writes).toHaveLength(0);
  });
}

for (const failure of ['overview', 'metrics'] as const) {
  test(`account refresh reports ${failure} failure, retains data and can retry`, async ({ page }) => {
    await page.clock.install();
    const state = await mockConsole(page);
    await page.goto('/?view=accounts');
    const usage = page.locator(`#account-${nativeId} .account-usage-cell strong`);
    await expect(usage).toHaveText('125');
    let failed = true;
    await page.route(failure === 'overview' ? '**/api/overview' : '**/api/accounts/metrics?*', route => failed
      ? route.fulfill({ status: 503, json: { error: { code: 'TEMPORARY_ERROR', message: '暂时无法读取' } } })
      : route.fallback());
    await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
    await expect(page.getByRole('alert').filter({ hasText: '账号数据刷新未完成' })).toBeVisible();
    await expect(page.getByRole('status').filter({ hasText: '账号数据已刷新' })).toHaveCount(0);
    await expect(usage).toHaveText('125');
    await expect(page.getByRole('button', { name: '刷新账号数据', exact: true })).toBeEnabled();
    failed = false;
    state.records[0].outputTokens = 50;
    await page.getByRole('button', { name: '刷新账号数据', exact: true }).click();
    await expect(page.getByRole('status').filter({ hasText: '账号数据已刷新' })).toBeVisible();
    await expect(usage).toHaveText('150');
    expect(state.writes).toHaveLength(0);
  });
}

import { expect, test } from '@playwright/test';
import type { Request } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const action of ['filter', 'clear'] as const) {
  test(`${action} invalidates a pending statistics poll and only the new query keeps polling`, async ({
    page,
  }) => {
    await page.clock.install();
    const state = await mockConsole(page);
    state.data.operations.queued = 0;
    const initial = page.waitForResponse((response) => response.url().includes('/api/statistics?'));
    await page.goto('/?view=requests');
    const stale = await (await initial).json();
    const rows = page.locator('.records-table tbody tr');
    await expect(rows).toHaveCount(1);

    let capture!: (request: Request) => void;
    const captured = new Promise<Request>((resolve) => {
      capture = resolve;
    });
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let finish!: () => void;
    const finished = new Promise<void>((resolve) => {
      finish = resolve;
    });
    let hold = true;
    await page.route('**/api/statistics?**', async (route) => {
      if (!hold) return route.fallback();
      hold = false;
      capture(route.request());
      await gate;
      try {
        await route.fulfill({ json: stale });
      } finally {
        finish();
      }
    });
    await page.route('**/api/requests', async (route) => {
      expect(route.request().method()).toBe('DELETE');
      const deleted = state.records.length;
      state.records.splice(0);
      await route.fulfill({ json: { deleted } });
    });

    await page.clock.fastForward(30_100);
    const pending = await captured;
    const aborted = page.waitForEvent('requestfailed', (request) => request === pending);
    try {
      if (action === 'filter') {
        await page.getByLabel('密钥名称筛选内容').fill('不存在的密钥');
        await page.getByRole('button', { name: '搜索请求记录', exact: true }).click();
      } else {
        await page.getByRole('button', { name: '清空所有记录', exact: true }).click();
        await page.getByRole('button', { name: '确认清空全部记录', exact: true }).click();
      }
      await aborted;
      await expect(page.getByText('当前范围没有请求记录')).toBeVisible();
    } finally {
      release();
    }
    await finished;
    await expect(rows).toHaveCount(0);

    const queries = state.statisticsQueries.length;
    await page.clock.fastForward(30_100);
    await expect.poll(() => state.statisticsQueries.length).toBe(queries + 1);
    await expect(rows).toHaveCount(0);
    await expect(page.getByRole('navigation', { name: '请求记录分页' })).toContainText(
      '第 1 / 1 页 · 共 0 条',
    );
  });
}

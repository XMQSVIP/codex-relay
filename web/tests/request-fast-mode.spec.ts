import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const width of [375, 1440]) for (const theme of ['light', 'dark'] as const) {
  test(`fast request confirmation and downgrade at ${width}px ${theme}`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.emulateMedia({ colorScheme: theme });
    const state = await mockConsole(page);
    const ordinary = state.records[0];
    state.records.push(
      { ...ordinary, id: '50000000-0000-0000-0000-000000000001', serviceTier: 'priority', responseServiceTier: 'priority' },
      { ...ordinary, id: '60000000-0000-0000-0000-000000000001', serviceTier: 'priority', responseServiceTier: 'default' },
      { ...ordinary, id: '70000000-0000-0000-0000-000000000001', serviceTier: 'priority', responseServiceTier: null },
      { ...ordinary, id: '80000000-0000-0000-0000-000000000001', serviceTier: 'default', responseServiceTier: 'priority' },
      { ...ordinary, id: '90000000-0000-0000-0000-000000000001', serviceTier: 'default', responseServiceTier: 'default' },
    );
    await page.goto('/?view=requests');
    const confirmed = page.getByRole('img', { name: '快速模式（上游已确认）', exact: true });
    const downgraded = page.getByRole('img', { name: '已请求快速模式，上游降级为普速', exact: true });
    const unconfirmed = page.getByRole('img', { name: '已请求快速模式', exact: true });
    await expect(confirmed).toHaveCount(2);
    await expect(downgraded).toHaveCount(1);
    await expect(unconfirmed).toHaveCount(1);
    for (const icon of await confirmed.all()) await expect(icon.locator('svg')).toHaveAttribute('fill', 'currentColor');
    await expect(downgraded.locator('svg')).toHaveAttribute('fill', 'none');
    await expect(unconfirmed.locator('svg')).toHaveAttribute('fill', 'none');
    await expect(downgraded).toHaveClass(/downgraded/);
    await expect(unconfirmed).toHaveClass(/unconfirmed/);
    await expect(page.locator('main')).not.toContainText('未确认');
    await expect(page.locator('.records-table .record-fast-mode')).toHaveCount(4);
    await page.screenshot({ path: info.outputPath('fast-mode.png'), fullPage: true });

    for (const [id, requested, actual] of [
      ['50000000', '快速模式', '快速模式'],
      ['60000000', '快速模式', '普速'],
      ['70000000', '快速模式', '未确认（上游未返回实际等级）'],
      ['80000000', '普速', '快速模式'],
    ]) {
      await page.getByRole('button', { name: `查看请求详情 ${id}`, exact: true }).click();
      const dialog = page.getByRole('dialog');
      const field = (name: string) => dialog.locator('.request-fields > div').filter({ has: page.getByText(name, { exact: true }) }).locator('dd');
      await expect(field('请求服务模式')).toHaveText(requested);
      await expect(field('实际服务模式')).toHaveText(actual);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await dialog.getByRole('button', { name: '关闭请求详情' }).click();
    }
  });
}

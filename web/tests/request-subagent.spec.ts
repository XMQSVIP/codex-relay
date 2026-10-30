import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const width of [375, 1440]) {
  test(`only subagent requests have an icon beside reasoning effort at ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.emulateMedia({ colorScheme: 'dark' });
    const state = await mockConsole(page);
    const ordinary = state.records[0];
    state.records.push({ ...ordinary, id: '50000000-0000-0000-0000-000000000001', subagentKind: 'worker', reasoningEffort: 'high' });
    state.records.push({ ...ordinary, id: '50000000-0000-0000-0000-000000000002', subagentKind: 'review', reasoningEffort: 'max' });
    await page.goto('/?view=requests');
    const rows = page.locator('.records-table tbody tr');
    await expect(rows).toHaveCount(3);
    await expect(page.getByRole('img', { name: '子代理请求：worker', exact: true })).toHaveAttribute('title', '子代理请求：worker');
    await expect(page.getByRole('img', { name: '子代理请求：review', exact: true })).toBeVisible();
    await expect(page.locator('.record-effort .record-subagent')).toHaveCount(2);
    await expect(page.locator('.record-model .record-subagent')).toHaveCount(0);
    const icon = page.getByRole('img', { name: '子代理请求：worker', exact: true });
    const alignment = await icon.evaluate(element => {
      const cell = element.parentElement!;
      const text = Array.from(cell.childNodes).find(node => node.nodeType === Node.TEXT_NODE && node.textContent === 'high')!;
      const range = document.createRange();
      range.selectNode(text);
      const label = range.getBoundingClientRect();
      const image = element.getBoundingClientRect();
      return { beside: image.left >= label.right, overlaps: image.top < label.bottom && image.bottom > label.top };
    });
    expect(alignment).toEqual({ beside: true, overlaps: true });
    await expect(rows.filter({ has: page.getByRole('button', { name: '查看请求详情 40000000', exact: true }) }).getByRole('img')).toHaveCount(0);
    await page.locator('.records-table').screenshot({ path: info.outputPath('subagent-icons.png') });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
}

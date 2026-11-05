import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

test('debug toggle enables capture and disables with clearing semantics', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=settings&tab=diagnostics');
  const toggle = page.getByRole('switch', { name: '请求原文调试' });
  await expect(toggle).toHaveAttribute('aria-checked', 'false');
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-checked', 'true');
  expect(state.debug.enabled).toBe(true);
  await toggle.click();
  await expect(toggle).toHaveAttribute('aria-checked', 'false');
  expect(state.debug.enabled).toBe(false);
});

test('successful request shows raw SSE and clears displayed raw text after debug is disabled', async ({ page }) => {
  const state = await mockConsole(page);
  state.debug.enabled = true;
  const raw = 'data: {"delta":"diagnostic-original"}\n\ndata: [DONE]\n\n';
  await page.route('**/api/requests/*/content', route => route.fulfill({ json: {
    id: state.records[0].id,
    request: { body: '{ "reasoning": {"effort":"high"} }', debug: true, debugRevision: 0, encoding: 'utf8', textOmitted: false, truncated: false, complete: true },
    response: { body: raw, debug: true, debugRevision: 0, encoding: 'utf8', transport: 'sse', statusCode: 200, truncated: false, complete: true },
  } }));
  await page.goto('/?view=requests');
  await page.getByRole('button', { name: '查看请求详情 40000000', exact: true }).click();
  await expect(page.getByLabel('响应 JSON')).toHaveText(raw);
  await expect(page.getByLabel('原请求 JSON')).toContainText('high');
  state.debug.enabled = false;
  await expect(page.getByText('此记录未保存原请求内容。')).toBeVisible();
  await expect(page.getByText('diagnostic-original', { exact: false })).toHaveCount(0);
});


test('token column shows cache rate and unknown when input is zero', async ({ page }) => {
  const state = await mockConsole(page);
  Object.assign(state.records[0], { inputTokens: 1000, cachedInputTokens: 750 });
  state.records.push({ ...state.records[0], id: '50000000-0000-0000-0000-000000000001', inputTokens: 0, cachedInputTokens: 0 });
  await page.goto('/?view=requests');
  const cells = page.locator('.record-tokens');
  await expect(cells.filter({ hasText: '75%' })).toHaveCount(1);
  await expect(cells.filter({ hasText: '未知' })).toHaveCount(1);
});

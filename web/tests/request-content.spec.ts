import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

const failure = {
  request: { body: { model: 'requested-model', input: [{ role: 'user', content: '[已省略原文]' }], reasoning: { effort: 'high' }, max_output_tokens: 123 }, textOmitted: true, truncated: false },
  response: { body: { error: { code: 'UPSTREAM_REQUEST_REJECTED', message: '上游拒绝了请求：参数组合无效。' } }, transport: 'http', statusCode: 502, truncated: false },
};

for (const [width, theme] of [[375, 'dark'], [1440, 'light']] as const) {
  test(`failed request shows readable request and response content ${width}px ${theme}`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.emulateMedia({ colorScheme: theme });
    const state = await mockConsole(page);
    Object.assign(state.records[0], { status: 'failed', errorCode: 'UPSTREAM_REQUEST_REJECTED' });
    let loads = 0;
    await page.route('**/api/requests/*/content', route => {
      loads++;
      return route.fulfill({ json: { id: state.records[0].id, ...failure } });
    });
    await page.goto('/?view=requests');
    const trigger = page.getByRole('button', { name: '查看请求详情 40000000', exact: true });
    await expect(trigger).toBeVisible();
    expect(loads).toBe(0);
    await trigger.click();
    const dialog = page.getByRole('dialog');
    const content = dialog.getByRole('region', { name: '请求与响应内容' });
    await expect(content.getByLabel('原请求 JSON')).toContainText('"max_output_tokens": 123');
    await expect(content.getByLabel('原请求 JSON')).toContainText('[已省略原文]');
    await expect(content.getByLabel('响应 JSON')).toContainText('上游拒绝了请求：参数组合无效。');
    await expect(content).toContainText('HTTP 502');
    const loaded = loads;
    await content.getByLabel('原请求 JSON').focus();
    await expect(content.getByLabel('原请求 JSON')).toBeFocused();
    await expect.poll(() => dialog.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
    await content.scrollIntoViewIfNeeded();
    await content.screenshot({ path: info.outputPath('failure-content.png') });
    expect(loads).toBe(loaded);
    await page.keyboard.press('Escape');
    await expect(trigger).toBeFocused();
  });
}

test('content load errors can be retried and a large SSE terminal remains readable', async ({ page }) => {
  const state = await mockConsole(page);
  state.records[0].status = 'failed';
  let retry = false;
  await page.route('**/api/requests/*/content', route => {
    if (!retry) return route.fulfill({ status: 503, json: { error: { message: '内容暂时无法读取。' } } });
    return route.fulfill({ json: { id: state.records[0].id, request: failure.request, response: { transport: 'sse', statusCode: 200, truncated: true, body: { type: 'response.failed', response: { error: { message: `<script>alert('safe')</script>${'long-content-'.repeat(600)}` } } } } } });
  });
  await page.goto('/?view=requests');
  await page.getByRole('button', { name: '查看请求详情 40000000', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('alert')).toContainText('内容暂时无法读取。');
  retry = true;
  await dialog.getByRole('button', { name: '重试加载内容' }).click();
  await expect(dialog.getByLabel('响应 JSON')).toContainText('<script>');
  await expect(dialog.locator('script')).toHaveCount(0);
  await expect(dialog).toContainText('SSE · HTTP 200 · 终态事件');
  await expect(dialog).toContainText('响应内容较大，部分内容已截断。');
  await expect.poll(() => dialog.evaluate(element => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(1);
});

test('content loads only on opening details and missing content is explicit for success and interruption', async ({ page }) => {
  const state = await mockConsole(page);
  let loads = 0;
  await page.route('**/api/requests/*/content', route => {
    loads++;
    return route.fulfill({ json: { id: state.records[0].id, request: null, response: null } });
  });
  await page.goto('/?view=requests');
  await expect(page.getByRole('button', { name: '查看请求详情 40000000', exact: true })).toBeVisible();
  expect(loads).toBe(0);
  await page.getByRole('button', { name: '查看请求详情 40000000', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await expect(page.getByRole('region', { name: '请求与响应内容' })).toBeVisible();
  await expect(page.getByRole('dialog')).toContainText('此记录未保存原请求内容。');
  await expect(page.getByRole('dialog')).toContainText('此记录未保存响应内容。');
  expect(loads).toBeGreaterThan(0);
  await page.keyboard.press('Escape');
  state.records[0].status = 'interrupted';
  await page.getByRole('button', { name: '刷新请求记录', exact: true }).click();
  await expect(page.locator('.record-status.interrupted')).toBeVisible();
  await page.getByRole('button', { name: '查看请求详情 40000000', exact: true }).click();
  await expect(page.getByRole('dialog')).toContainText('此记录未保存原请求内容。');
  await expect(page.getByRole('dialog')).toContainText('此记录未保存响应内容。');
});

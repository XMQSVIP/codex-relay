import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

test('search and rejected API requests have readable actions and failure details', async ({ page }) => {
  const state = await mockConsole(page);
  Object.assign(state.records[0], { action: 'web.search', status: 'failed', errorCode: 'UPSTREAM_REQUEST_REJECTED' });
  state.records.push({ ...state.records[0], id: '50000000-0000-0000-0000-000000000001', action: 'api.rejected', status: 'rejected', errorCode: 'ENDPOINT_NOT_SUPPORTED' });
  await page.route('**/api/requests/*/content', route => route.fulfill({ json: {
    id: state.records[0].id,
    request: { body: { model: 'test-model', input: '[已省略搜索指令与上下文]' }, textOmitted: true, truncated: false },
    response: { transport: 'http', statusCode: 405, truncated: false, body: { error: { message: '上游搜索接口不支持此请求。' } } },
  } }));
  await page.goto('/?view=requests');
  await expect(page.locator('.records-table')).toContainText('UPSTREAM_REQUEST_REJECTED');
  await expect(page.locator('.records-table')).toContainText('ENDPOINT_NOT_SUPPORTED');
  await page.getByRole('button', { name: '查看请求详情 40000000', exact: true }).click();
  await expect(page.getByRole('dialog').getByText('网页搜索', { exact: true })).toBeVisible();
  const content = page.getByRole('dialog').getByRole('region', { name: '请求与响应内容' });
  await expect(content).toContainText('HTTP 405');
  await expect(content.getByLabel('响应 JSON')).toContainText('上游搜索接口不支持此请求');
  await page.getByRole('button', { name: '关闭请求详情', exact: true }).click();
  await page.getByRole('button', { name: '查看请求详情 50000000', exact: true }).click();
  await expect(page.getByRole('dialog').getByText('接口拒绝', { exact: true })).toBeVisible();
});

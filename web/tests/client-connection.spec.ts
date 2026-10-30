import { expect, test } from '@playwright/test';
import { mockConsole } from './fixtures';

for (const width of [390, 1440]) test(`multi-turn request format and copy at ${width}px`, async ({ page }, testInfo) => {
  await page.setViewportSize({ width, height: 1000 });
  await mockConsole(page);
  await page.addInitScript(() => {
    let copied = '';
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: {
      writeText: async (text: string) => { copied = text; },
      readText: async () => copied,
    } });
  });
  const modelRequests: string[] = [];
  await page.route('**/v1/**', async route => {
    modelRequests.push(route.request().url());
    await route.abort();
  });
  await page.goto('/?view=keys&tab=guide');
  const examples = page.locator('details.client-example');
  if (!(await examples.getAttribute('open'))) {
    // An open boolean attribute has the empty string as its value.
    if (await examples.getAttribute('open') === null) await examples.locator('summary').click();
  }
  await page.getByRole('button', { name: '多轮 JSON', exact: true }).click();
  await expect(page.getByRole('button', { name: '第一轮', exact: true })).toHaveAttribute('aria-pressed', 'true');
  const code = page.getByLabel('多轮 JSON 接入示例');
  const first = JSON.parse(await code.innerText());
  expect(first.input).toEqual([{ role: 'user', content: '什么是缓存？' }]);
  expect(first.client_metadata.thread_id).toBeTruthy();
  expect(first.stream).toBe(false);
  expect(first.store).toBe(false);
  await page.getByRole('button', { name: '复制请求 JSON', exact: true }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(await code.innerText());

  const next = page.getByRole('button', { name: '第二轮（续聊）', exact: true });
  await next.focus();
  await page.keyboard.press('Enter');
  await expect(next).toHaveAttribute('aria-pressed', 'true');
  const second = JSON.parse(await code.innerText());
  expect(second.client_metadata).toEqual(first.client_metadata);
  expect(second.instructions).toEqual(first.instructions);
  expect(second.model).toBe(first.model);
  expect(second.input[0]).toEqual(first.input[0]);
  expect(second.input[1]).toMatchObject({ type: 'message', role: 'assistant', content: [{ type: 'output_text' }] });
  expect(second.input[2]).toEqual({ role: 'user', content: '请给一个例子。' });
  expect(second.previous_response_id).toBeUndefined();
  await expect(examples).toContainText('不要照抄示例答案或消息 ID');
  await expect(examples).toContainText('未报告表示未知');
  await page.getByRole('button', { name: '复制请求 JSON', exact: true }).click();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(await code.innerText());
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await examples.screenshot({ path: testInfo.outputPath('multi-turn-request-format.png') });

  await page.getByRole('button', { name: 'Python', exact: true }).click();
  await expect(page.getByLabel('Python 接入示例')).toContainText('client.responses.create');
  await expect(code).toHaveCount(0);
  expect(modelRequests).toEqual([]);
});

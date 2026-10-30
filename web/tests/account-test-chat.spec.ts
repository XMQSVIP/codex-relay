import { test, expect } from '@playwright/test';
import { mockConsole, nativeCapabilities, nativeId, oauthId } from './fixtures';
import { readTestChat } from '../src/testChatStream';

const events = (...items: object[]) => items.map(item => `data: ${JSON.stringify(item)}\n\n`).join('');
const success = events({ type: 'ready', model: 'test-model' }, { type: 'delta', text: 'OK <script>text</script>' }, { type: 'done', firstResponseMs: 42, totalMs: 1250, usage: { inputTokens: 5, outputTokens: 2, totalTokens: 7 } });

test('reasoning selection and both puzzle shortcuts send immediately without duplicate requests', async ({ page }, info) => {
  await mockConsole(page);
  const bodies: { model: string; reasoningEffort: string | null; messages: { role: string; content: string }[] }[] = [];
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/accounts/*/chat-test', async route => {
    bodies.push(route.request().postDataJSON());
    if (bodies.length === 1) await gate;
    await route.fulfill({ contentType: 'text/event-stream', body: success });
  });
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '测试对话：本机 Codex', exact: true }).click();
  const dialog = page.getByRole('dialog');
  const effort = dialog.getByRole('combobox', { name: '推理强度', exact: true });
  await expect(effort).toHaveValue('low');
  await effort.selectOption('high');
  await dialog.getByRole('button', { name: '糖果题', exact: true }).click();
  await expect.poll(() => bodies.length).toBe(1);
  expect(bodies[0].reasoningEffort).toBe('high');
  const candy = bodies[0].messages[0].content;
  expect(candy).toContain('不使用任何外部工具回答以下问题');
  expect(candy).toContain('| 圆形 | 7 | 9 | 8 |');
  expect(candy).toContain('| 五角星形 | 7 | 6 | 4 |');
  await expect(effort).toBeDisabled();
  await expect(dialog.getByRole('button', { name: '糖果题', exact: true })).toBeDisabled();
  await expect(dialog.getByRole('button', { name: '牛奶题', exact: true })).toBeDisabled();
  release();
  await expect(dialog.getByRole('status')).toContainText('测试成功');
  await dialog.getByRole('button', { name: '清空对话', exact: true }).click();
  await effort.selectOption('');
  await dialog.getByRole('button', { name: '牛奶题', exact: true }).click();
  await expect(dialog.getByRole('status')).toContainText('测试成功');
  expect(bodies).toHaveLength(2);
  expect(bodies[1].reasoningEffort).toBeNull();
  expect(bodies[1].messages).toHaveLength(1);
  const milk = bodies[1].messages[0].content;
  expect(milk).toContain('Hardy–Weinberg equilibrium');
  expect(milk).toContain('3 feet and 6 inches');
  expect(milk).toContain('four significant figures');
  expect(milk).toMatch(/can not search$/);
  await dialog.screenshot({ path: info.outputPath('reasoning-and-presets.png') });
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: '测试对话：本机 Codex', exact: true }).click();
  await expect(effort).toHaveValue('low');
  await expect(dialog.getByLabel('测试消息')).toHaveValue('ciallo');
});

test('selected account streams temporary multi-turn chat and clears on close', async ({ page }) => {
  await mockConsole(page);
  const bodies: { messages: { role: string; content: string }[] }[] = [];
  await page.route(`**/api/accounts/${oauthId}/chat-test`, async route => {
    bodies.push(route.request().postDataJSON());
    await route.fulfill({ contentType: 'text/event-stream', body: success });
  });
  await page.goto('/?view=accounts');
  const opener = page.getByRole('button', { name: '测试对话：独立测试账户', exact: true });
  await opener.click();
  const dialog = page.getByRole('dialog', { name: '测试对话', exact: true });
  await expect(dialog.getByLabel('测试消息')).toHaveValue('ciallo');
  expect(bodies).toHaveLength(0);
  await expect(dialog.getByText(/消耗|计费/)).toHaveCount(0);
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await expect(dialog.getByRole('status')).toContainText('测试成功');
  await expect(dialog.getByText('OK <script>text</script>', { exact: true })).toBeVisible();
  await expect(dialog.getByText('首响 42 ms', { exact: true })).toBeVisible();
  await expect(dialog.getByText('Token 7', { exact: true })).toBeVisible();
  await dialog.getByLabel('测试消息').fill('继续测试');
  await dialog.getByLabel('测试消息').press('Control+Enter');
  await expect(dialog.getByRole('status')).toContainText('测试成功');
  expect(bodies[1].messages).toEqual([{ role: 'user', content: 'ciallo' }, { role: 'assistant', content: 'OK <script>text</script>' }, { role: 'user', content: '继续测试' }]);
  await page.keyboard.press('Escape');
  await expect(opener).toBeFocused();
  await opener.click();
  await expect(dialog.getByRole('log')).not.toContainText('继续测试');
  await expect(dialog.getByLabel('测试消息')).toHaveValue('ciallo');
  expect(bodies).toHaveLength(2);
});

test('failed turns remain visible but are excluded from the next request', async ({ page }) => {
  await mockConsole(page); let calls = 0;
  const bodies: { messages: unknown[] }[] = [];
  await page.route('**/api/accounts/*/chat-test', async route => {
    bodies.push(route.request().postDataJSON()); calls++;
    await route.fulfill({ contentType: 'text/event-stream', body: calls === 1 ? events({ type: 'delta', text: 'partial' }, { type: 'error', code: 'AUTH_FAILED', message: '凭据无效' }) : success });
  });
  await page.goto(`/?view=accounts&account=${nativeId}`);
  await page.getByRole('link', { name: '运行信息', exact: true }).click();
  await page.getByRole('button', { name: '测试对话：本机 Codex', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await expect(dialog.getByRole('alert')).toContainText('凭据无效');
  await expect(dialog.getByLabel('测试消息')).toHaveValue('ciallo');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await expect(dialog.getByRole('status')).toContainText('测试成功');
  expect(bodies[1].messages).toHaveLength(1);
  await dialog.getByRole('button', { name: '清空对话', exact: true }).click();
  await expect(dialog.getByLabel('测试模型')).toBeEnabled();
  await expect(dialog.getByRole('log')).not.toContainText('partial');
});

test('stop and close abort a pending fetch without retaining text', async ({ page }) => {
  await mockConsole(page);
  await page.addInitScript(() => {
    const original = window.fetch;
    Object.assign(window, { testAborts: 0 });
    window.fetch = (url, init) => String(url).endsWith('/chat-test') ? new Promise((_resolve, reject) => {
      init?.signal?.addEventListener('abort', () => { const target = window as unknown as { testAborts: number }; target.testAborts++; reject(new DOMException('Aborted', 'AbortError')); });
    }) : original(url, init);
  });
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '测试对话：本机 Codex', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await dialog.getByRole('button', { name: '停止生成', exact: true }).click();
  await expect(dialog.getByRole('status')).toHaveText('已停止');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await dialog.getByRole('button', { name: '关闭窗口', exact: true }).click();
  expect(await page.evaluate(() => (window as unknown as { testAborts: number }).testAborts)).toBe(2);
  await expect(dialog).toHaveCount(0);
});

test('mobile dialog stays in viewport and traps focus', async ({ page }) => {
  await mockConsole(page);
  await page.setViewportSize({ width: 375, height: 667 });
  await page.goto('/?view=accounts');
  const actions = page.getByLabel('本机 Codex的操作', { exact: true });
  const actionBounds = await actions.boundingBox();
  for (const button of await actions.locator('button, a').all()) {
    const bounds = await button.boundingBox();
    expect(bounds!.x).toBeGreaterThanOrEqual(actionBounds!.x);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(actionBounds!.x + actionBounds!.width + 1);
  }
  await page.getByRole('button', { name: '测试对话：本机 Codex', exact: true }).click();
  const dialog = page.getByRole('dialog');
  const box = await dialog.boundingBox();
  expect(box!.x).toBeGreaterThanOrEqual(0); expect(box!.y).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(375); expect(box!.y + box!.height).toBeLessThanOrEqual(667);
  await dialog.getByRole('button', { name: '发送', exact: true }).focus();
  await page.keyboard.press('Tab');
  await expect(dialog.getByRole('button', { name: '关闭窗口', exact: true })).toBeFocused();
  await dialog.screenshot({ path: '../.cache/account-test-chat-mobile.png' });
});

test('SSE reader handles split UTF-8, CRLF, comments and premature closure', async () => {
  const bytes = new TextEncoder().encode(': ping\r\n\r\ndata: {"type":"delta","text":"你好"}\r\n\r\ndata: {"type":"done"}\r\n\r\n');
  const received: unknown[] = [];
  await readTestChat(new ReadableStream({ start(controller) { for (const byte of bytes) controller.enqueue(new Uint8Array([byte])); controller.close(); } }), event => received.push(event));
  expect(received).toEqual([{ type: 'delta', text: '你好' }, { type: 'done' }]);
  await expect(readTestChat(new ReadableStream({ start(controller) { controller.close(); } }), () => {})).rejects.toThrow('连接提前断开');
});

test('API key accounts require a model before sending', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.accounts[0].source = 'api-key';
  state.data.operations.accounts[0].route!.model = '';
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '测试对话：本机 Codex', exact: true }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByLabel('测试模型')).toHaveAttribute('placeholder', '输入上游模型 ID');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await expect(dialog.getByRole('alert')).toHaveText('请先输入要测试的上游模型 ID。');
  expect(state.writes).toHaveLength(0);
});

test('model selector shows the whole account catalog and sends the selected model with ciallo', async ({ page }) => {
  const state = await mockConsole(page);
  state.data.operations.accounts[0].capabilities = { ...nativeCapabilities(), models: ['test-model', 'second-model', 'third-model', 'second-model'].map(model => ({ ...nativeCapabilities().models[0], id: model, model })) };
  const bodies: { model: string; messages: { content: string }[] }[] = [];
  await page.route(`**/api/accounts/${nativeId}/chat-test`, async route => {
    bodies.push(route.request().postDataJSON());
    await route.fulfill({ contentType: 'text/event-stream', body: success });
  });
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '测试对话：本机 Codex', exact: true }).click();
  const dialog = page.getByRole('dialog');
  const selector = dialog.getByRole('combobox', { name: '测试模型', exact: true });
  await expect(selector.locator('option')).toHaveText(['使用账户默认模型', 'test-model', 'second-model', 'third-model', '手动输入模型 ID']);
  await selector.selectOption('second-model');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await expect(dialog.getByRole('status')).toContainText('测试成功');
  expect(bodies[0]).toMatchObject({ model: 'second-model', messages: [{ content: 'ciallo' }] });
  await expect(selector).toBeDisabled();
  await dialog.getByRole('button', { name: '清空对话', exact: true }).click();
  await expect(dialog.getByLabel('测试消息')).toHaveValue('ciallo');
  await selector.selectOption('__custom__');
  await dialog.getByLabel('自定义模型 ID').fill('custom-model');
  await dialog.getByRole('button', { name: '发送', exact: true }).click();
  await expect(dialog.getByRole('status')).toContainText('测试成功');
  expect(bodies[1]).toMatchObject({ model: 'custom-model', messages: [{ content: 'ciallo' }] });
});

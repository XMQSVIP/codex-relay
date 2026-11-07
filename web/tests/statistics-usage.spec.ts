import { test, expect } from '@playwright/test';
import { groupId, mockConsole, nativeId, oauthId } from './fixtures';

test('usage rankings show all matching tokens and API key request counts in descending token order', async ({ page }) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  for (let i = 0; i < 60; i++) state.records.push({ ...record, id: `extra-${i}` });
  state.records.push({ ...record, id: 'large', accountId: oauthId, accountName: '高用量账户', keyId: '30000000-0000-0000-0000-000000000002', keyName: '高用量密钥', inputTokens: 10000, outputTokens: 1000 });
  state.records.push({ ...record, id: 'unknown', accountId: oauthId, accountName: '高用量账户', keyId: '30000000-0000-0000-0000-000000000002', keyName: '高用量密钥', inputTokens: null, outputTokens: null, status: 'failed' });
  await page.goto('/?view=statistics');
  const keys = page.getByRole('region', { name: '用量排行', exact: true });
  await expect(keys.locator('tbody tr')).toHaveCount(2);
  const keyRow = keys.locator('tbody tr').first();
  await expect(keyRow.getByRole('rowheader')).toContainText('高用量密钥');
  await expect(keyRow.locator('td').first().locator('strong')).toHaveText('2');
  await expect(keyRow.locator('td').first().locator('small')).toHaveText('3.2%');
  await expect(keys.locator('tbody tr').nth(1).locator('td').first().locator('strong')).toHaveText('61');
  await expect(keys.locator('tbody tr').nth(1).locator('td').last()).toContainText('7.63K');
  await expect(keys.getByRole('columnheader', { name: '总 Token', exact: true })).toHaveAttribute('aria-sort', 'descending');
  await expect(page.locator('.records-panel')).toHaveCount(0);

  const queryCount = state.statisticsQueries.length;
  await keys.getByRole('button', { name: '账户', exact: true }).click();
  const accountRow = keys.locator('tbody tr').first();
  await expect(accountRow.getByRole('rowheader')).toContainText('高用量账户');
  await expect(accountRow.locator('td').first().locator('strong')).toHaveText('2');
  await expect(accountRow.locator('td').last()).toContainText('11K');
  await expect(accountRow.locator('td').last()).toContainText('1 / 2 次用量已知');
  expect(state.statisticsQueries.length).toBe(queryCount);
  await page.reload();
  await expect(keys.getByRole('button', { name: '账户', exact: true })).toHaveAttribute('aria-pressed', 'true');

  await page.goto(`/?view=statistics&accountId=${nativeId}&groupId=${groupId}`);
  await expect(keys.locator('tbody tr')).toHaveCount(1);
  await expect(keys.locator('tbody tr').getByRole('rowheader')).toContainText('开发客户端');
});

test('rankings display unknown and zero distinctly, refresh counts and show empty results', async ({ page }) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  Object.assign(record, { inputTokens: 0, outputTokens: 0 });
  state.records.push({ ...record, id: 'unknown', accountId: oauthId, accountName: null, keyId: '30000000-0000-0000-0000-000000000002', keyName: null, inputTokens: null, outputTokens: null });
  await page.goto('/?view=statistics');
  const keys = page.getByRole('region', { name: '用量排行', exact: true });
  await expect(keys.locator('tbody tr')).toHaveCount(2);
  await expect(keys.locator('tbody tr').first().locator('td').last().locator('strong')).toHaveText('0');
  await expect(keys.locator('tbody tr').last().locator('td').last().locator('strong')).toHaveText('未知');
  await expect(keys.getByRole('rowheader', { name: /未命名API 密钥/ })).toBeVisible();
  state.records.push({ ...record, id: 'refreshed', inputTokens: 30, outputTokens: 5 });
  await page.getByRole('button', { name: '刷新用量统计', exact: true }).click();
  await expect(keys.locator('tbody tr').first().locator('td').first().locator('strong')).toHaveText('2');
  await expect(keys.locator('tbody tr').first().locator('td').last().locator('strong')).toHaveText('35');
  await page.goto('/?view=statistics&status=rejected');
  await expect(page.getByText('当前筛选范围没有API 密钥请求记录。', { exact: true })).toBeVisible();
  await keys.getByRole('button', { name: '账户', exact: true }).click();
  await expect(page.getByText('当前筛选范围没有账户请求记录。', { exact: true })).toBeVisible();
  await page.goto('/?view=requests');
  await expect(page.locator('.usage-ranking')).toHaveCount(0);
});

test('rankings switch between request count and token usage for both dimensions and remember the choice', async ({ page }) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  for (let i = 0; i < 60; i++) state.records.push({ ...record, id: `frequent-${i}` });
  state.records.push({ ...record, id: 'large', accountId: oauthId, accountName: '高用量账户', keyId: '30000000-0000-0000-0000-000000000002', keyName: '高用量密钥', inputTokens: 10000, outputTokens: 1000 });
  await page.goto('/?view=statistics');
  const ranking = page.getByRole('region', { name: '用量排行', exact: true });
  const requestsSort = ranking.getByRole('button', { name: '请求次数', exact: true });
  await expect(ranking.getByRole('combobox')).toHaveCount(0);
  const first = ranking.locator('tbody tr').first();
  await expect(first.getByRole('rowheader')).toContainText('高用量密钥');
  const queryCount = state.statisticsQueries.length;
  await requestsSort.focus();
  await page.keyboard.press('Enter');
  await expect(first.getByRole('rowheader')).toContainText('开发客户端');
  await expect(first.locator('.usage-requests strong')).toHaveText('61');
  await expect(ranking.getByRole('columnheader', { name: '请求次数', exact: true })).toHaveAttribute('aria-sort', 'descending');
  await expect(ranking.getByRole('columnheader', { name: '总 Token', exact: true })).not.toHaveAttribute('aria-sort');
  await expect(ranking.locator('.usage-ranking-note')).toContainText('按请求次数降序排列');
  await ranking.getByRole('button', { name: '账户', exact: true }).click();
  await expect(first.getByRole('rowheader')).toContainText('本机 Codex');
  expect(state.statisticsQueries.length).toBe(queryCount);
  await page.reload();
  await expect(requestsSort).toHaveAttribute('aria-pressed', 'true');
  await expect(first.getByRole('rowheader')).toContainText('本机 Codex');
  await page.getByRole('button', { name: '刷新用量统计', exact: true }).click();
  await expect(requestsSort).toHaveAttribute('aria-pressed', 'true');
  await expect(first.locator('.usage-requests strong')).toHaveText('61');
  await ranking.getByRole('button', { name: '总 Token', exact: true }).click();
  await expect(first.getByRole('rowheader')).toContainText('高用量账户');
  await expect(ranking.getByRole('columnheader', { name: '总 Token', exact: true })).toHaveAttribute('aria-sort', 'descending');
  await expect(ranking.getByRole('columnheader', { name: '请求次数', exact: true })).not.toHaveAttribute('aria-sort');
  await ranking.getByRole('button', { name: 'API 密钥', exact: true }).click();
  await expect(first.getByRole('rowheader')).toContainText('高用量密钥');
});

test('request sorting includes unknown usage while token sorting puts it after known zero', async ({ page }) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  Object.assign(record, { inputTokens: 0, outputTokens: 0 });
  for (let i = 0; i < 2; i++) state.records.push({ ...record, id: `unknown-${i}`, keyId: '30000000-0000-0000-0000-000000000002', keyName: '未知用量密钥', inputTokens: null, outputTokens: null, status: 'failed' });
  await page.goto('/?view=statistics&usageSort=requests');
  const ranking = page.getByRole('region', { name: '用量排行', exact: true });
  const first = ranking.locator('tbody tr').first();
  await expect(first.getByRole('rowheader')).toContainText('未知用量密钥');
  await expect(first.locator('.usage-requests strong')).toHaveText('2');
  await expect(first.locator('.usage-total strong')).toHaveText('未知');
  await ranking.getByRole('button', { name: '总 Token', exact: true }).click();
  await expect(first.getByRole('rowheader')).toContainText('开发客户端');
  await expect(first.locator('.usage-total strong')).toHaveText('0');
  await expect(ranking.locator('tbody tr').last().getByRole('rowheader')).toContainText('未知用量密钥');
});

for (const width of [375, 800, 801, 1280, 1440, 1920]) for (const theme of ['light', 'dark'] as const) {
  test(`usage ranking layout ${width}px ${theme}`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
    const state = await mockConsole(page);
    state.records[0].accountName = '超长账户名称'.repeat(10);
    state.records[0].keyName = '超长密钥名称'.repeat(10);
    for (let i = 1; i < 12; i++) state.records.push({ ...state.records[0], id: `rank-${i}`, accountId: `10000000-0000-0000-0000-${String(i + 1).padStart(12, '0')}`, accountName: `生产账户 ${i}`, keyId: `30000000-0000-0000-0000-${String(i + 1).padStart(12, '0')}`, keyName: `生产客户端 ${i}`, inputTokens: 100 * (i + 1), outputTokens: 25 * (i + 1) });
    await page.goto('/?view=statistics');
    const keys = page.getByRole('region', { name: '用量排行', exact: true });
    await expect(keys.locator('tbody tr')).toHaveCount(12);
    await keys.getByRole('button', { name: '请求次数', exact: true }).scrollIntoViewIfNeeded();
    await expect(keys.getByRole('button', { name: '请求次数', exact: true })).toBeInViewport();
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1);
    const scroll = keys.getByRole('region', { name: 'API 密钥用量，可滚动', exact: true });
    if (width >= 1920) expect(await scroll.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1);
    await scroll.focus();
    await expect(scroll).toBeFocused();
    const rankingBounds = await keys.boundingBox();
    const qualityBounds = await page.locator('.usage-quality').boundingBox();
    expect(rankingBounds!.height).toBeCloseTo(560, 0);
    if (width > 800) {
      expect(qualityBounds!.y).toBeCloseTo(rankingBounds!.y, 0);
      expect(qualityBounds!.height).toBeCloseTo(rankingBounds!.height, 0);
      expect(rankingBounds!.x).toBeGreaterThan(qualityBounds!.x + qualityBounds!.width);
    } else {
      expect(rankingBounds!.y).toBeGreaterThanOrEqual(qualityBounds!.y + qualityBounds!.height);
    }
    await scroll.press('PageDown');
    await expect.poll(() => scroll.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
    const scrollBounds = await scroll.boundingBox();
    const stickyHeader = await keys.getByRole('columnheader', { name: '请求次数', exact: true }).boundingBox();
    expect(stickyHeader!.y).toBeGreaterThanOrEqual(scrollBounds!.y - 1);
    expect(stickyHeader!.y).toBeLessThan(scrollBounds!.y + 10);
    if (width === 375) {
      await scroll.evaluate(element => { element.scrollLeft = element.scrollWidth; });
      await expect(keys.getByRole('columnheader', { name: '总 Token', exact: true })).toBeInViewport();
    }
    await keys.screenshot({ path: info.outputPath('key-usage.png') });
    await keys.getByRole('button', { name: '账户', exact: true }).click();
    await expect(keys.getByRole('columnheader', { name: '账户', exact: true })).toBeAttached();
    await expect(keys.locator('.usage-ranking-scroll')).toHaveJSProperty('scrollLeft', 0);
    await expect(keys.locator('.usage-ranking-scroll')).toHaveJSProperty('scrollTop', 0);
    await keys.screenshot({ path: info.outputPath('account-usage.png') });
    const help = keys.getByText('统计口径与说明', { exact: true });
    await expect(keys.locator('.usage-ranking-note')).not.toBeVisible();
    await help.focus();
    await page.keyboard.press('Enter');
    await expect(keys.locator('.usage-ranking-note')).toBeVisible();
    expect((await keys.boundingBox())!.height).toBeCloseTo(560, 0);
    await page.keyboard.press('Enter');
    await expect(keys.locator('.usage-ranking-note')).not.toBeVisible();
    await page.evaluate(() => window.scrollTo(0, 0));
    await expect(page.getByRole('heading', { name: '缓存用量', exact: true })).toHaveCount(0);
    const trendBounds = await page.locator('.usage-trend').boundingBox();
    const summaryBounds = await page.locator('.usage-summary').boundingBox();
    const schedulingBounds = await page.locator('.usage-scheduling').boundingBox();
    expect(Math.abs((width <= 800 ? trendBounds!.width : trendBounds!.width + schedulingBounds!.width + 20) - summaryBounds!.width)).toBeLessThanOrEqual(1);
    expect(qualityBounds!.width).toBeCloseTo(schedulingBounds!.width, 0);
    if (width >= 1280) {
      const sidebar = await page.locator('.sidebar').boundingBox();
      const workspace = await page.locator('.workspace').boundingBox();
      expect(Math.abs(sidebar!.height - workspace!.height)).toBeLessThanOrEqual(1);
      expect(sidebar!.height).toBeGreaterThan(1000);
      await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
      await expect(page.getByRole('link', { name: '系统设置', exact: true })).toBeInViewport();
      expect((await page.locator('.sidebar-content').boundingBox())!.y).toBeCloseTo(0, 0);
      await page.evaluate(() => window.scrollTo(0, 0));
    }
    await page.screenshot({ path: info.outputPath('usage-page.png'), fullPage: true });
  });
}

test('usage overview distinguishes model requests, partial coverage and cached input', async ({ page }) => {
  const state = await mockConsole(page);
  const record = state.records[0];
  state.records.push({ ...record, id: 'unknown', inputTokens: null, outputTokens: null, cachedInputTokens: null });
  state.records.push({ ...record, id: 'zero', inputTokens: 0, outputTokens: 0, cachedInputTokens: 0 });
  state.records.push({ ...record, id: 'management', kind: 'management', inputTokens: null, outputTokens: null, cachedInputTokens: null });
  await page.goto('/?view=statistics');
  const overview = page.getByRole('region', { name: '模型用量概览', exact: true });
  await expect(overview.locator('.usage-metric-copy > strong')).toHaveText(['3', '125', '50%']);
  await expect(overview).toContainText('另有 1 条管理与检查记录');
  await expect(overview.locator('.usage-coverage')).toContainText('2 / 3');
  await expect(overview.locator('.usage-coverage')).toContainText('1 次未知');
  await expect(overview.locator('.usage-metric-card').last()).toContainText('缓存命中率');
  await expect(page.locator('.usage-quality .metric-strip')).toBeVisible();
});

test('all trends are visible together, cover all records and support keyboard inspection', async ({ page }) => {
  const state = await mockConsole(page);
  for (let i = 0; i < 24; i++) state.records.push({ ...state.records[0], id: `request-${i}` });
  await page.goto('/?view=statistics&trendMetric=requests');
  const chart = page.getByRole('group', { name: '用量趋势，输入、输出和缓存 Token。使用左右方向键查看各时段。', exact: true });
  await expect(chart.getByRole('button', { name: /25 次模型请求，3,125 Token/ })).toHaveCount(1);
  await expect(page.getByRole('group', { name: '趋势指标', exact: true })).toHaveCount(0);
  for (const label of ['模型请求', '总 Token', '缓存命中率']) await expect(page.getByRole('img', { name: `${label}趋势`, exact: true })).toBeVisible();
  await expect(page.locator('.usage-series-line')).toHaveCount(3);
  const queries = state.statisticsQueries.length;
  await chart.getByRole('button').first().focus();
  await page.keyboard.press('End');
  await expect(chart.getByRole('button').last()).toBeFocused();
  await expect(page.locator('.usage-trend-readout')).toContainText('次模型请求');
  await chart.getByRole('button', { name: /25 次模型请求，3,125 Token/ }).focus();
  await expect(page.locator('.usage-trend-readout dd')).toHaveText(['2,500', '625', '1,250', '50%']);
  expect(state.statisticsQueries.length).toBe(queries);
  await page.getByText('查看时段数据', { exact: true }).click();
  const table = page.getByRole('region', { name: '时段用量数据，可横向滚动', exact: true });
  await expect(table.locator('tbody tr')).toHaveCount(24);
  await expect(table.locator('tbody tr').filter({ hasText: '2,500' })).toContainText('1,250');
});

test('unknown token periods are distinct from known zero and empty periods', async ({ page }) => {
  const state = await mockConsole(page);
  Object.assign(state.records[0], { inputTokens: null, outputTokens: null, cachedInputTokens: null });
  await page.goto('/?view=statistics');
  const total = page.locator('.usage-metric-card').nth(1).locator('.usage-metric-copy > strong');
  await expect(total).toHaveText('未知');
  await expect(page.locator('.usage-trend-targets .is-unknown')).toHaveCount(1);
  await page.locator('.usage-trend-targets .is-unknown').click();
  await expect(page.locator('.usage-trend-readout dd')).toHaveText(['未知', '未知', '未知', '未知']);
  expect(await page.locator('.usage-series-line').evaluateAll(paths => paths.every(path => !path.getAttribute('d')?.trim()))).toBe(true);
  Object.assign(state.records[0], { inputTokens: 0, outputTokens: 0, cachedInputTokens: 0 });
  await page.getByRole('button', { name: '刷新用量统计', exact: true }).click();
  await expect(total).toHaveText('0');
  await expect(page.locator('.usage-trend-targets .is-unknown')).toHaveCount(0);
  await expect(page.locator('.usage-trend-readout dd')).toHaveText(['0', '0', '0', '未知']);
});

test('model rankings drill into records while retaining date and account filters', async ({ page }) => {
  const state = await mockConsole(page);
  state.records.push({ ...state.records[0], id: 'larger-model', model: 'larger-model', inputTokens: 5000, outputTokens: 500 });
  await page.goto(`/?view=statistics&accountId=${nativeId}`);
  const ranking = page.getByRole('region', { name: '用量排行', exact: true });
  await page.getByRole('button', { name: '近 7 天', exact: true }).click();
  const query = state.statisticsQueries.length;
  await ranking.getByRole('button', { name: '模型', exact: true }).click();
  await expect(ranking.locator('tbody tr')).toHaveCount(2);
  await expect(ranking.locator('tbody tr').first().getByRole('rowheader')).toContainText('larger-model');
  expect(state.statisticsQueries.length).toBe(query);
  const dates = new URL(page.url()).searchParams;
  await ranking.getByRole('link', { name: 'larger-model', exact: true }).click();
  await expect(page).toHaveURL(/view=requests/);
  const params = new URL(page.url()).searchParams;
  expect(params.get('model')).toBe('larger-model');
  expect(params.get('accountId')).toBe(nativeId);
  expect(params.get('startDate')).toBe(dates.get('startDate'));
  expect(params.get('endDate')).toBe(dates.get('endDate'));
  expect(params.get('kind')).toBe('inference');
  await expect(page.locator('.records-table tbody tr')).toHaveCount(1);
});

test('cache hit rate is weighted by known input and excludes unreported cache data', async ({ page }) => {
  const state = await mockConsole(page);
  state.records.push({ ...state.records[0], id: 'miss', inputTokens: 900, outputTokens: 1, cachedInputTokens: 0 });
  state.records.push({ ...state.records[0], id: 'cache-unknown', inputTokens: 10000, outputTokens: 1, cachedInputTokens: null });
  await page.goto('/?view=statistics');
  const hitRate = page.locator('.usage-metric-card').last();
  await expect(hitRate).toContainText('缓存命中率');
  await expect(hitRate.locator('.usage-metric-copy > strong')).toHaveText('5%');
  await expect(hitRate).toContainText('2 / 3 次缓存用量已知');
  state.records.forEach(record => { record.cachedInputTokens = null; });
  await page.getByRole('button', { name: '刷新用量统计', exact: true }).click();
  await expect(hitRate.locator('.usage-metric-copy > strong')).toHaveText('未知');
  await expect(page.getByRole('img', { name: '缓存命中率趋势，暂无已知数据', exact: true })).toBeVisible();
  expect((await page.locator('.usage-trend-canvas .trend-cache path').getAttribute('d'))!.trim()).toBe('');
});

for (const width of [375, 1440]) for (const theme of ['light', 'dark'] as const) {
  test(`direct usage trends ${width}px ${theme}`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' });
    const state = await mockConsole(page);
    const source = state.records[0];
    const from = await page.evaluate(() => new Date('2026-10-28T00:00:00').getTime());
    state.records.splice(0);
    for (let hour = 0; hour < 24; hour++) {
      const input = Math.round((2 + Math.sin(hour / 3) + hour / 16) * 10000);
      for (let n = 0; n < 1 + hour % 4; n++) state.records.push({ ...source,
        id: `hour-${hour}-${n}`, startedAt: from + hour * 3600000 + 1000,
        inputTokens: input, outputTokens: Math.round(input / 12),
        cachedInputTokens: hour === 8 ? null : Math.round(input * (0.7 + 0.2 * Math.sin(hour / 5))),
      });
    }
    await page.goto('/?view=statistics&startDate=2026-10-28&endDate=2026-10-28');
    await expect(page.locator('.usage-metric-copy > strong')).toHaveCount(3);
    await expect(page.locator('.usage-sparkline')).toHaveCount(3);
    await expect(page.getByRole('group', { name: '趋势指标', exact: true })).toHaveCount(0);
    const cachePath = await page.locator('.usage-trend-canvas .trend-cache path').getAttribute('d');
    expect(cachePath!.match(/M/g)).toHaveLength(2);
    await page.locator('.usage-trend-targets button').nth(8).focus();
    await expect(page.locator('.usage-trend-readout dd')).toHaveText([/^[\d,]+$/, /^[\d,]+$/, '未知', '未知']);
    await page.locator('.usage-trend-targets button').nth(15).focus();
    await page.evaluate(() => window.scrollTo(0, 0));
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath('direct-usage-trends.png'), fullPage: true });
    await page.getByText('查看时段数据', { exact: true }).click();
    const table = page.getByRole('region', { name: '时段用量数据，可横向滚动', exact: true });
    await expect(table.locator('tbody tr')).toHaveCount(24);
    await expect(table.locator('tbody tr').nth(8).locator('td').last()).toHaveText('未知');
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
}

test.describe('current-day sparklines', () => {
  test.use({ timezoneId: 'UTC' });
  test('future intervals are omitted and a known cache miss remains a zero', async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-10-29T12:30:00Z'));
    const state = await mockConsole(page);
    Object.assign(state.records[0], { startedAt: Date.parse('2026-10-29T12:00:00Z'), cachedInputTokens: 0 });
    await page.goto('/?view=statistics');
    await expect(page.locator('.usage-metric-card').first().locator('.usage-sparkline circle')).toHaveCount(13);
    await expect(page.locator('.usage-metric-card').last().locator('.usage-metric-copy > strong')).toHaveText('0%');
    await expect(page.locator('.usage-metric-card').last().locator('.usage-sparkline circle')).toHaveCount(1);
    await page.locator('.usage-trend-targets button').nth(12).focus();
    await expect(page.locator('.usage-trend-readout dd').last()).toHaveText('0%');
    await expect(page.locator('.usage-trend-canvas .trend-cache circle')).toHaveCount(1);
  });
});

test.describe('calendar date presets', () => {
  test.use({ timezoneId: 'America/New_York' });
  test('date presets span local calendar days across daylight saving and preserve filters', async ({ page }) => {
    await page.clock.setFixedTime(new Date('2026-11-02T12:00:00Z'));
    const state = await mockConsole(page);
    await page.goto(`/?view=statistics&accountId=${nativeId}`);
    await page.getByRole('button', { name: '近 7 天', exact: true }).click();
    await expect(page.locator('input[name="startDate"]')).toHaveValue('2026-10-27');
    await expect(page.locator('input[name="endDate"]')).toHaveValue('2026-11-02');
    await expect.poll(() => {
      const query = state.statisticsQueries.at(-1);
      return Number(query?.get('to')) - Number(query?.get('from')) + 1;
    }).toBe(169 * 3600000);
    await page.getByRole('button', { name: '近 30 天', exact: true }).click();
    await expect(page.locator('input[name="startDate"]')).toHaveValue('2026-10-04');
    await page.getByRole('button', { name: '今天', exact: true }).click();
    await expect(page.locator('input[name="startDate"]')).toHaveValue('2026-11-02');
    expect(new URL(page.url()).searchParams.get('accountId')).toBe(nativeId);
  });
});

import { test, expect } from '@playwright/test';
import { mockConsole } from './fixtures';

test('saved results stay with their checks and failures expose details and ordered history', async ({
  page,
}) => {
  const state = await mockConsole(page);
  state.data.reports['inspect-provider'] = {
    checkedAt: 1000,
    provider: {
      id: 'openai',
      model: 'test-model',
      baseUrl: 'https://model.example.test',
      auth: 'chatgpt',
    },
  };
  state.data.jobs = [
    {
      id: 'older',
      kind: 'inspect-provider',
      state: 'completed',
      createdAt: 1000,
      report: null,
      error: null,
    },
    {
      id: 'newer',
      kind: 'doctor',
      state: 'failed',
      createdAt: 2000,
      report: {
        checkedAt: 2000,
        checks: [{ id: 'stream_cancel', state: 'failed', detail: '未能连接桌面进程。' }],
      },
      error: { code: 'DESKTOP_OFFLINE', message: '请先启动官方桌面。' },
    },
  ];
  await page.goto('/?view=settings&tab=diagnostics');
  const provider = page.getByRole('region', { name: '读取提供方', exact: true });
  await provider.getByText('查看保存结果', { exact: true }).click();
  await expect(provider.getByText('https://model.example.test', { exact: true })).toBeVisible();
  const desktop = page.getByRole('region', { name: '桌面检查', exact: true });
  await expect(desktop.getByRole('status')).toHaveText('需要处理');
  await expect(desktop.getByText('未能连接桌面进程。', { exact: true })).toBeVisible();
  await expect(desktop.getByRole('alert')).toContainText('请先启动官方桌面');
  const records = page.locator('.diagnostic-history .job-row');
  await expect(records).toHaveCount(2);
  await expect(records.first()).toContainText('桌面状态检查');
  await expect(records.last()).toContainText('读取有效提供方');
  expect(state.writes).toEqual([]);
});

test('a running check keeps its previous report readable and updates after completion', async ({
  page,
}) => {
  const state = await mockConsole(page);
  state.data.busy = true;
  state.data.jobs = [
    { id: 'running', kind: 'doctor', state: 'running', createdAt: 2000, report: null, error: null },
  ];
  state.data.reports.doctor = {
    checkedAt: 1000,
    checks: [{ id: 'stream_cancel', state: 'passed', detail: '上次检查结果' }],
  };
  await page.goto('/?view=settings&tab=diagnostics');
  const desktop = page.getByRole('region', { name: '桌面检查', exact: true });
  await expect(desktop).toHaveAttribute('aria-busy', 'true');
  await expect(desktop.getByRole('button')).toBeDisabled();
  await desktop.getByText('查看上次结果', { exact: true }).click();
  await desktop.getByText('查看检查明细', { exact: true }).click();
  await expect(desktop.getByText('上次检查结果', { exact: true })).toBeVisible();
  state.data.jobs[0].state = 'completed';
  state.data.jobs[0].report = { checkedAt: 3000, sharedDesktopVerified: true };
  state.data.busy = false;
  await expect(desktop).toHaveAttribute('aria-busy', 'false');
  await expect(desktop.getByRole('status')).toHaveText('已完成');
  await expect(desktop.getByText('桌面调试连接：已验证', { exact: true })).toBeVisible();
  await expect(desktop.getByRole('button')).toBeEnabled();
});

for (const theme of ['light', 'dark'] as const) {
  for (const width of [375, 1440]) {
    test(`environment workspace fits ${width}px in ${theme} mode and supports keyboard controls`, async ({
      page,
    }, info) => {
      const state = await mockConsole(page);
      await page.addInitScript(
        (value) => localStorage.setItem('codex-bridge.theme.v1', value),
        theme,
      );
      await page.setViewportSize({ width, height: width === 375 ? 844 : 1000 });
      await page.emulateMedia({ reducedMotion: 'reduce' });
      await page.goto('/?view=settings&tab=diagnostics');
      const runtime = page.getByRole('region', { name: '原生运行环境', exact: true });
      await expect(runtime.getByText('已配置', { exact: true })).toBeVisible();
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      const main = await runtime.boundingBox();
      const tools = await page
        .getByRole('region', { name: '请求原文调试', exact: true })
        .boundingBox();
      if (width === 1440) {
        expect(tools!.x).toBeGreaterThan(main!.x + main!.width);
        expect(tools!.y).toBe(main!.y);
      } else {
        expect(tools!.y).toBeGreaterThan(main!.y + main!.height);
      }
      const toggle = page.getByRole('switch', { name: '请求原文调试', exact: true });
      await expect(toggle).toBeEnabled();
      await toggle.focus();
      await page.keyboard.press('Space');
      await expect(toggle).toBeChecked();
      expect(state.debug.enabled).toBe(true);
      await expect(toggle).toBeEnabled();
      await expect(toggle).toBeFocused();
      await page.keyboard.press('Space');
      await expect(toggle).not.toBeChecked();
      const guide = page.locator('.desktop-debug-guide > summary');
      await guide.focus();
      await page.keyboard.press('Enter');
      await expect(page.getByRole('button', { name: '检测原生安装', exact: true })).toBeVisible();
      await page.keyboard.press('Enter');
      await page.evaluate(() => window.scrollTo(0, 0));
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      ).toBe(true);
      await page.screenshot({
        path: info.outputPath(`environment-${width}-${theme}.png`),
        fullPage: true,
      });
      if (width === 375) {
        await page.setViewportSize({ width: 844, height: 375 });
        expect(
          await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        ).toBe(true);
      }
    });
  }
}

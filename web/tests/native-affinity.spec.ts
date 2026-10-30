import { expect, test } from '@playwright/test';
import { mockConsole } from './fixtures';

test('affinity settings default to 24 hours and persist whole hours', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=settings');
  const input = page.getByLabel('账户亲和时间', { exact: true });
  const save = page.getByRole('button', { name: '保存亲和时间', exact: true });
  await expect(input).toHaveValue('24');
  await expect(save).toBeDisabled();
  await input.fill('36');
  await save.click();
  await expect.poll(() => state.data.operations.nativeAffinityTtlHours).toBe(36);
  await page.reload();
  await expect(input).toHaveValue('36');
  await input.fill('30');
  await save.click();
  await expect.poll(() => state.data.operations.nativeAffinityTtlHours).toBe(30);
  await page.reload();
  await expect(input).toHaveValue('30');
});

test('invalid values cannot save and conflicts preserve the draft across refresh', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=settings');
  const input = page.getByLabel('账户亲和时间', { exact: true });
  const save = page.getByRole('button', { name: '保存亲和时间', exact: true });
  for (const value of ['', '0', '721']) {
    await input.fill(value);
    await expect(input).toHaveAttribute('aria-invalid', 'true');
    await expect(save).toBeDisabled();
  }
  await input.fill('1.5');
  await expect(save).toBeDisabled();
  await input.fill('90');
  state.conflict = true;
  await save.click();
  await expect(page.getByText('配置已更新，请刷新后重试。', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '刷新状态', exact: true }).click();
  await expect(input).toHaveValue('90');
  await save.click();
  await expect.poll(() => state.data.operations.nativeAffinityTtlHours).toBe(90);
});

for (const width of [390, 1440]) test(`affinity settings remain usable at ${width}px`, async ({ page }, testInfo) => {
  await page.setViewportSize({ width, height: 1000 });
  await mockConsole(page);
  await page.goto('/?view=settings');
  const section = page.getByRole('region', { name: '账户亲和', exact: true });
  await section.scrollIntoViewIfNeeded();
  await expect(section.getByLabel('账户亲和时间', { exact: true })).toBeVisible();
  await expect(section.getByText('小时', { exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await section.screenshot({ path: testInfo.outputPath('native-affinity.png') });
});

import { expect, test } from '@playwright/test';
import { mockConsole, oauthId, nativeId } from './fixtures';

 test('ChatGPT creation uses a single optional note as its name', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '添加账户', exact: true }).click();
  await page.getByRole('radio', { name: '登录其他 ChatGPT 账户', exact: true }).check();
  await expect(page.getByLabel('账户名称', { exact: true })).toHaveCount(0);
  await page.getByLabel('账户备注（可选）').fill('开发账号');
  await page.getByRole('button', { name: '创建并继续登录' }).click();
  await expect(page.getByRole('heading', { name: '开发账号', exact: true })).toBeVisible();
  expect(state.writes.find(write => write.path === '/accounts')?.body).toEqual({ name: '开发账号', note: '开发账号', autoName: false });
});

test('an empty creation note waits for the verified email prefix', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto('/?view=accounts');
  await page.getByRole('button', { name: '添加账户', exact: true }).click();
  await page.getByRole('radio', { name: '登录其他 ChatGPT 账户', exact: true }).check();
  await page.getByRole('button', { name: '创建并继续登录' }).click();
  await expect(page.getByRole('heading', { name: '未命名账户', exact: true })).toBeVisible();
  expect(state.writes.find(write => write.path === '/accounts')?.body).toEqual({ name: '', note: '', autoName: true });
});

test('editing the note changes the name, preserves the email and clearing restores the email prefix', async ({ page }) => {
  const state = await mockConsole(page);
  const account = state.data.accounts.find(account => account.id === oauthId)!;
  account.email = '123@xx.com'; account.name = '123'; account.note = '123'; account.account = null;
  await page.goto(`/?view=accounts&account=${oauthId}`);
  await page.getByRole('button', { name: '编辑账户备注', exact: true }).click();
  await page.getByLabel('账户备注（可选）').fill('生产备用');
  await page.getByRole('button', { name: '保存备注', exact: true }).click();
  await expect(page.getByRole('heading', { name: '生产备用', exact: true })).toBeVisible();
  await page.getByRole('link', { name: '返回账户列表' }).click();
  await expect(page.locator(`#account-${oauthId}`)).toContainText('123@xx.com');
  await page.getByRole('searchbox').fill('生产备用');
  await expect(page.locator('.account-console-row')).toHaveCount(1);
  await page.reload();
  await expect(page.locator(`#account-${oauthId}`)).toContainText('生产备用');
  await page.getByRole('link', { name: '生产备用', exact: true }).click();
  await page.getByRole('button', { name: '编辑账户备注', exact: true }).click();
  await page.getByLabel('账户备注（可选）').clear();
  await page.getByRole('button', { name: '保存备注', exact: true }).click();
  await expect(page.getByRole('heading', { name: '123', exact: true })).toBeVisible();
  expect(account.note).toBe('123');
  expect(account.email).toBe('123@xx.com');
});

test('conflicts preserve unsaved notes until reset', async ({ page }) => {
  const state = await mockConsole(page);
  await page.goto(`/?view=accounts&account=${oauthId}`);
  await page.getByRole('button', { name: '编辑账户备注', exact: true }).click();
  await page.getByLabel('账户备注（可选）').fill('未保存草稿');
  state.conflict = true;
  await page.getByRole('button', { name: '保存备注', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('账户备注已更新');
  await expect(page.getByLabel('账户备注（可选）')).toHaveValue('未保存草稿');
  await page.getByRole('button', { name: '还原备注' }).click();
  await expect(page.getByLabel('账户备注（可选）')).toHaveValue('');
});

test('long notes wrap on narrow screens and render as text', async ({ page }, testInfo) => {
  const state = await mockConsole(page);
  const note = '<script>alert(1)</script>' + '账户用途'.repeat(100);
  state.data.accounts[1].note = note; state.data.accounts[1].name = note;
  await page.setViewportSize({ width: 375, height: 812 });
  await page.goto('/?view=accounts');
  await expect(page.locator(`#account-${oauthId} h3`)).toContainText('<script>alert(1)</script>');
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
  await page.screenshot({ path: testInfo.outputPath('account-notes-mobile.png'), fullPage: true });
});

for (const [width, theme] of [[375, 'dark'], [1200, 'light']] as const) {
  test(`note editing stays in the heading and opens an accessible dialog at ${width}px in ${theme}`, async ({ page }, info) => {
    await mockConsole(page);
    await page.addInitScript(theme => localStorage.setItem('codex-bridge.theme.v1', theme), theme);
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`/?view=accounts&account=${nativeId}`);
    const trigger = page.getByRole('button', { name: '编辑账户备注', exact: true });
    await expect(page.locator('.detail-title-row').getByRole('button', { name: '编辑账户备注' })).toBeVisible();
    await expect(page.locator('details.account-metadata')).toHaveCount(0);
    await page.screenshot({ path: info.outputPath('account-heading.png'), fullPage: true });
    await trigger.click();
    const dialog = page.getByRole('dialog', { name: '编辑账户备注', exact: true });
    await expect(dialog.getByLabel('账户备注（可选）')).toBeFocused();
    await dialog.getByLabel('账户备注（可选）').fill('未保存备注');
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
    await dialog.screenshot({ path: info.outputPath('edit-note.png') });
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await expect(trigger).toBeFocused();
    await trigger.click();
    await expect(dialog.getByLabel('账户备注（可选）')).toHaveValue('');
    await dialog.getByRole('button', { name: '取消', exact: true }).click();
  });
}

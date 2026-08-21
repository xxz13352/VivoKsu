import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { authenticateE2eUser, prepareE2eLogin } from './authenticated-session';

const screenshotDirectory = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../../docs/migration-baselines/screenshots',
);

describe('奶蛙Flash navigation', () => {
  before(async () => {
    await prepareE2eLogin();
  });

  it('starts at the blocking login window before the main shell is available', async () => {
    await $('[data-role="login-window"]').waitForDisplayed();
    assert.equal(await $('aside[aria-label="主导航"]').isExisting(), false);
    await browser.waitUntil(async () => {
      const size = await browser.execute(() => ({ width: window.innerWidth, height: window.innerHeight }));
      return size.width === 400 && size.height === 564;
    }, { timeout: 5_000, timeoutMsg: '登录窗口未恢复为 400x564 客户区' });
    const loginWindowSize = await browser.execute(() => ({ width: window.innerWidth, height: window.innerHeight }));
    assert.equal(loginWindowSize.width, 400);
    assert.equal(loginWindowSize.height, 564);
    const loginCardSize = await $('.nw-login-card').getSize();
    assert.equal(loginCardSize.width, 352);
    assert.equal(loginCardSize.height, 516);
    assert.equal(await $('[data-role="login-window"] [role="status"]').getText(), '');
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-login.png'));
  });

  describe('after an accepted login', () => {
    beforeEach(async () => {
      await authenticateE2eUser();
    });

  it('loads the WebdriverIO bridge only in the explicit E2E build', async () => {
    const hasWdioTauriApi = await browser.execute(() => typeof window.wdioTauri !== 'undefined');

    assert.equal(hasWdioTauriApi, true);
  });

  it('preserves the ten visible navigation entries and opens each page', async () => {
    const navigation = await $('aside[aria-label="主导航"]');
    await navigation.waitForDisplayed();
    const labels = await $$('[data-page-id]').map((item) => item.getText());
    assert.deepEqual(labels, ['设备概览', '文件管理', 'ADB 投屏', '快速刷写', '可视刷写', 'VIVO 线刷', '固件提取', 'Vivo ROOT', '在线状态', '软件']);
    const expectedTitles = ['设备概览', '文件管理', 'ADB 投屏', '快速刷写', '可视刷写', 'VIVO 线刷', '固件提取', 'Vivo ROOT', '在线状态', '软件'];
    const items = await $$('[data-page-id]');

    for (const [index, item] of items.entries()) {
      await item.click();
      await browser.waitUntil(async () => await $('h1').getText() === expectedTitles[index], {
        timeout: 5_000,
        timeoutMsg: `导航未切换到 ${expectedTitles[index]}`,
      });
      assert.equal(await $('h1').getText(), expectedTitles[index]);
    }
  });
  });
});

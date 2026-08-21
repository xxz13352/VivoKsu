import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { VISUAL_STATE_FIXTURES } from '../../src/test/visual-state-fixtures';

const screenshotDirectory = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../../docs/migration-baselines/screenshots',
);

const mockCommand = async (command: string, value: unknown) => {
  const mock = await browser.tauri.mock(command);
  await mock.mockResolvedValue(value);
};

describe('奶蛙Flash visual baseline', () => {
  it('renders the Chinese software readiness state from Tauri command DTOs', async () => {
    await mockCommand('version_check', VISUAL_STATE_FIXTURES.versionAllowed);
    await mockCommand('session_state', VISUAL_STATE_FIXTURES.signedOutSession);
    await mockCommand('auth_login', { name: '验收用户', username: 'acceptance-user' });
    await mockCommand('session_start', null);
    await mockCommand('resource_inventory', VISUAL_STATE_FIXTURES.resourcesReady);
    await mockCommand('operation_logs_snapshot', VISUAL_STATE_FIXTURES.operationLogs);
    await mockCommand('software_status', VISUAL_STATE_FIXTURES.softwareReady);

    await $('[aria-label="账号"]').waitForDisplayed();
    assert.equal(await browser.execute(() => typeof window.wdioTauri), 'undefined');
    await $('[aria-label="账号"]').setValue('acceptance-user');
    await $('[aria-label="密码"]').setValue('test-password');
    await $('[aria-label="点击登录"]').click();
    await $('h1').waitForDisplayed();

    const surfaces = await browser.execute(() => ({
      body: getComputedStyle(document.body).backgroundColor,
      titlebar: getComputedStyle(document.querySelector<HTMLElement>('.nw-titlebar')!).backgroundColor,
      sidebar: getComputedStyle(document.querySelector<HTMLElement>('.nw-sidebar')!).backgroundColor,
      page: getComputedStyle(document.querySelector<HTMLElement>('.nw-page-card')!).backgroundColor,
      sideTools: getComputedStyle(document.querySelector<HTMLElement>('.nw-device-status-panel')!).backgroundColor,
    }));

    assert.deepEqual(surfaces, {
      body: 'rgb(247, 249, 251)',
      titlebar: 'rgb(255, 255, 255)',
      sidebar: 'rgb(255, 255, 255)',
      page: 'rgb(248, 250, 252)',
      sideTools: 'rgb(255, 255, 255)',
    });

    await $('[data-page-id="Software"]').click();
    await $('[aria-label="组件状态"]').waitForDisplayed();

    assert.equal(await $$('.nw-page-header').length, 0);
    assert.equal(await $$('.nw-software-page-heading h1').length, 1);
    assert.equal(await $('.nw-software-page-heading h1').getText(), '软件');

    const componentText = await $('[aria-label="组件状态"]').getText();
    assert.match(componentText, /奶蛙Flash 客户端/);
    assert.match(componentText, /版本 v1\.0\.1/);
    assert.match(componentText, /ADB（WinUSB）/);
    assert.match(componentText, /Fastboot（fastbootd 刷写）/);
    assert.match(componentText, /MediaTek（联发科 \/ BROM 救砖）/);
    assert.doesNotMatch(componentText, /ADB 工具：/);
    assert.equal(await $('[aria-label="软件帮助"]').getText(), '需要帮助?\n驱动未安装时,启动会自动弹出驱动提醒;也可点右上角「重新检测」刷新各组件状态。');

    const controls = await browser.execute(() => ({
      activeNav: getComputedStyle(document.querySelector<HTMLElement>('.nw-nav-item.active')!).backgroundColor,
      activeNavText: getComputedStyle(document.querySelector<HTMLElement>('.nw-nav-item.active')!).color,
      inactiveNav: getComputedStyle(document.querySelector<HTMLElement>('.nw-nav-item:not(.active)')!).backgroundColor,
      log: getComputedStyle(document.querySelector<HTMLElement>('.nw-operation-log-panel')!).backgroundColor,
      action: getComputedStyle(document.querySelector<HTMLElement>('.nw-software-components button')!).backgroundColor,
    }));

    assert.deepEqual(controls, {
      activeNav: 'rgb(234, 247, 245)',
      activeNavText: 'rgb(8, 122, 112)',
      inactiveNav: 'rgba(0, 0, 0, 0)',
      log: 'rgb(255, 255, 255)',
      action: 'rgb(255, 255, 255)',
    });

    const shellGeometry = await browser.execute(() => {
      const shell = document.querySelector<HTMLElement>('.nw-shell')!;
      const titlebar = document.querySelector<HTMLElement>('.nw-titlebar')!;
      const layout = document.querySelector<HTMLElement>('.nw-shell-layout')!;
      const sidebar = document.querySelector<HTMLElement>('.nw-sidebar')!;
      const page = document.querySelector<HTMLElement>('.nw-page-card')!;
      const rail = document.querySelector<HTMLElement>('.nw-status-rail');
      const layoutColumns = getComputedStyle(layout).gridTemplateColumns.split(' ');
      return {
        shellPadding: getComputedStyle(shell).padding,
        titlebarHeight: getComputedStyle(titlebar).height,
        titlebarRadius: getComputedStyle(titlebar).borderRadius,
        layoutLeftColumn: layoutColumns[0],
        layoutRightColumn: layoutColumns[layoutColumns.length - 1],
        layoutGap: getComputedStyle(layout).gap,
        sidebarRadius: getComputedStyle(sidebar).borderRadius,
        sidebarRightBorder: getComputedStyle(sidebar).borderRightWidth,
        pageBorder: getComputedStyle(page).borderTopWidth,
        railExists: rail !== null,
        railRadius: rail ? getComputedStyle(rail).borderRadius : null,
        softwareTitleFontSize: getComputedStyle(document.querySelector<HTMLElement>('.nw-software-page-heading h1')!).fontSize,
      };
    });

    assert.deepEqual(shellGeometry, {
      shellPadding: '0px',
      titlebarHeight: '64px',
      titlebarRadius: '0px',
      layoutLeftColumn: '160px',
      layoutRightColumn: '286px',
      layoutGap: '0px',
      sidebarRadius: '0px',
      sidebarRightBorder: '1px',
      pageBorder: '0px',
      railExists: true,
      railRadius: '0px',
      softwareTitleFontSize: '27px',
    });

    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-software-ready.png'));
  });
});

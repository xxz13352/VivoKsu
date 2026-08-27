import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { VISUAL_STATE_FIXTURES } from '../../src/test/visual-state-fixtures';
import { authenticateE2eUser } from './authenticated-session';

const specDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(specDirectory, '../../../..');
const screenshotDirectoryOverride = process.env.NWFLASH_E2E_SCREENSHOT_DIR;

if (screenshotDirectoryOverride && !path.isAbsolute(screenshotDirectoryOverride)) {
  throw new Error('NWFLASH_E2E_SCREENSHOT_DIR must be an absolute path.');
}

const resolvedScreenshotOverride = screenshotDirectoryOverride
  ? path.resolve(screenshotDirectoryOverride)
  : null;
if (resolvedScreenshotOverride) {
  const relativeToRepository = path.relative(repositoryRoot, resolvedScreenshotOverride);
  const isInsideRepository = relativeToRepository === ''
    || (!relativeToRepository.startsWith(`..${path.sep}`)
      && relativeToRepository !== '..'
      && !path.isAbsolute(relativeToRepository));
  if (isInsideRepository) {
    throw new Error('NWFLASH_E2E_SCREENSHOT_DIR must be outside the repository.');
  }
}

const screenshotDirectory = resolvedScreenshotOverride
  ?? path.resolve(specDirectory, '../../../../docs/migration-baselines/screenshots');

const mockCommand = async (command: string, value: unknown) => {
  const mock = await browser.tauri.mock(command);
  await mock.mockResolvedValue(value);
};

const WPF_CLIENT_SIZE = { width: 1240, height: 700 } as const;

const readNativeClientMetrics = async () => browser.execute(() => ({
  width: window.innerWidth,
  height: window.innerHeight,
  devicePixelRatio: window.devicePixelRatio,
}));

const setNativeWpfClientSize = async () => {
  // WebDriver sizes the physical host window, while the WebView reports logical CSS pixels.
  // Calibrate from the real devicePixelRatio instead of assuming 100% Windows display scaling.
  await browser.setWindowSize(1256, 709);
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const client = await readNativeClientMetrics();
    if (client.width === WPF_CLIENT_SIZE.width && client.height === WPF_CLIENT_SIZE.height) {
      return;
    }

    const outer = await browser.getWindowSize();
    const scale = client.devicePixelRatio > 0 ? client.devicePixelRatio : 1;
    await browser.setWindowSize(
      Math.max(1, Math.round(outer.width + ((WPF_CLIENT_SIZE.width - client.width) * scale))),
      Math.max(1, Math.round(outer.height + ((WPF_CLIENT_SIZE.height - client.height) * scale))),
    );
  }

  const client = await readNativeClientMetrics();
  assert.deepEqual(
    { width: client.width, height: client.height },
    WPF_CLIENT_SIZE,
    `Unable to calibrate the native WebView at devicePixelRatio=${client.devicePixelRatio}`,
  );
};

const assertGeometryWithin = (
  actual: Record<string, number>,
  expected: Record<string, number>,
  tolerances: number | Record<string, number> = 1,
) => {
  assert.deepEqual(Object.keys(actual).sort(), Object.keys(expected).sort());
  for (const [key, expectedValue] of Object.entries(expected)) {
    const tolerance = typeof tolerances === 'number' ? tolerances : (tolerances[key] ?? 1);
    assert.ok(
      Math.abs(actual[key] - expectedValue) <= tolerance,
      `${key} expected ${expectedValue}±${tolerance}, received ${actual[key]}`,
    );
  }
};

describe('奶蛙Flash native visual baseline', () => {
  it('captures the software idle state at the WPF main-window client size', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    // The embedded Edge driver sizes its 16x9 host chrome rather than the WebView client area.
    await setNativeWpfClientSize();
    await mockCommand('software_status', VISUAL_STATE_FIXTURES.softwareReady);

    await $('[data-page-id="Software"]').click();
    await $('[aria-label="组件状态"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });

    const titlebarRefresh = await browser.execute(() => {
      const refresh = document.querySelector<HTMLElement>('.nw-titlebar-refresh')!;
      const divider = document.querySelector<HTMLElement>('.nw-titlebar-divider')!;
      const styles = getComputedStyle(refresh);
      return {
        text: refresh.textContent?.trim(),
        borderColor: styles.borderColor,
        backgroundColor: styles.backgroundColor,
        fontSize: styles.fontSize,
        fontWeight: styles.fontWeight,
        padding: styles.padding,
        dividerWidth: getComputedStyle(divider).width,
        dividerHeight: getComputedStyle(divider).height,
      };
    });
    assert.deepEqual(titlebarRefresh, {
      text: '刷新设备',
      borderColor: 'rgb(215, 224, 231)',
      backgroundColor: 'rgb(255, 255, 255)',
      fontSize: '12px',
      fontWeight: '600',
      padding: '7px 12px',
      dividerWidth: '1px',
      dividerHeight: '22px',
    });

    const softwareGeometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-software-page-heading')!.getBoundingClientRect().top),
      tableTop: Math.round(document.querySelector<HTMLElement>('.nw-software-status-table')!.getBoundingClientRect().top),
      tableBottom: Math.round(document.querySelector<HTMLElement>('.nw-software-status-table')!.getBoundingClientRect().bottom),
      tableHeight: Math.round(document.querySelector<HTMLElement>('.nw-software-status-table')!.getBoundingClientRect().height),
    }));
    assertGeometryWithin(softwareGeometry, { headingTop: 94, tableTop: 188, tableBottom: 564, tableHeight: 376 });
    assert.equal(await $('.nw-operation-log-eyebrow').getText(), 'ACTIVITY LOG');
    assert.equal(await $('.nw-operation-log-empty').isDisplayed(), true);
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-software-idle.png'));
  });

  it('captures the Overview idle state with the WPF device profile layout', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await $('[data-page-id="Software"]').click();
    await mockCommand('device_refresh', {
      connection_state: 'Disconnected',
      serial: '--',
      connection_label: '等待连接',
      model: '未检测到设备',
      android_version: '--',
      battery_level: '--',
    });

    await $('[data-page-id="Overview"]').click();
    await $('[aria-label="只读设备档案"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-overview-heading .nw-page-eyebrow').getText(), 'DEVICE / OVERVIEW');
    assert.equal(await $('.nw-overview-device-profile footer span:last-child').getText(), 'READ-ONLY DEVICE PROFILE');
    assert.equal(await $('.nw-overview-reboot-heading p').getText(), 'REBOOT CONTROL');

    const overviewGeometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-overview-heading')!.getBoundingClientRect().top),
      profileTop: Math.round(document.querySelector<HTMLElement>('.nw-overview-device-profile')!.getBoundingClientRect().top),
      profileHeight: Math.round(document.querySelector<HTMLElement>('.nw-overview-device-profile')!.getBoundingClientRect().height),
      rebootTop: Math.round(document.querySelector<HTMLElement>('.nw-overview-reboot-controls')!.getBoundingClientRect().top),
      rebootHeight: Math.round(document.querySelector<HTMLElement>('.nw-overview-reboot-controls')!.getBoundingClientRect().height),
    }));
    assert.deepEqual(overviewGeometry, {
      headingTop: 94,
      profileTop: 188,
      profileHeight: 272,
      rebootTop: 520,
      rebootHeight: 96,
    });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-overview-idle.png'));
  });

  it('captures the File Manager idle state with the WPF device-file workbench', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await $('[data-page-id="FileManager"]').click();
    await $('[aria-label="文件管理"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-file-manager-heading .nw-page-eyebrow').getText(), 'ADB / DEVICE FILES');
    assert.equal(await $('.nw-file-manager-directory-summary strong').getText(), '/sdcard');
    assert.equal(await $('.nw-file-manager-workbench > footer span:last-child').getText(), 'ADB 文件传输');

    const fileManagerGeometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-file-manager-heading')!.getBoundingClientRect().top),
      workbenchTop: Math.round(document.querySelector<HTMLElement>('.nw-file-manager-workbench')!.getBoundingClientRect().top),
      summaryTop: Math.round(document.querySelector<HTMLElement>('.nw-file-manager-directory-summary')!.getBoundingClientRect().top),
      gridHeight: Math.round(document.querySelector<HTMLElement>('.nw-file-manager-entry-grid')!.getBoundingClientRect().height),
    }));
    assertGeometryWithin(fileManagerGeometry, {
      headingTop: 94,
      workbenchTop: 185,
      summaryTop: 268,
      gridHeight: 340,
    });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-filetransfer-idle.png'));
  });

  it('captures the ADB screencast idle state with the WPF console layout', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await $('[data-page-id="Mirror"]').click();
    await $('[aria-label="投屏控制"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({ width: window.innerWidth, height: window.innerHeight, documentHeight: document.documentElement.scrollHeight }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-mirror-heading .nw-page-eyebrow').getText(), 'ADB / SCREENCAST');
    assert.equal(await $('.nw-mirror-console > header p').getText(), 'SCRCPY SESSION');
    assert.equal(await $('.nw-mirror-console footer div:last-child span').getText(), '镜像进程');

    const geometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-mirror-heading')!.getBoundingClientRect().top),
      consoleTop: Math.round(document.querySelector<HTMLElement>('.nw-mirror-console')!.getBoundingClientRect().top),
      consoleHeight: Math.round(document.querySelector<HTMLElement>('.nw-mirror-console')!.getBoundingClientRect().height),
    }));
    assert.deepEqual(geometry, { headingTop: 94, consoleTop: 188, consoleHeight: 356 });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-adbactions-idle.png'));
  });

  it('captures the Quick Flash idle state with the WPF preset panel', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await $('[data-page-id="QuickFlash"]').click();
    await $('[aria-label="刷写预设"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({ width: window.innerWidth, height: window.innerHeight, documentHeight: document.documentElement.scrollHeight }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-quick-flash-heading .nw-page-eyebrow').getText(), 'FLASH / PRESET');
    assert.equal(await $$('.nw-quick-flash-preset-row').length, 4);
    const geometry = await browser.execute(() => ({ headingTop: Math.round(document.querySelector<HTMLElement>('.nw-quick-flash-heading')!.getBoundingClientRect().top), panelTop: Math.round(document.querySelector<HTMLElement>('.nw-quick-flash-preset-panel')!.getBoundingClientRect().top), panelHeight: Math.round(document.querySelector<HTMLElement>('.nw-quick-flash-preset-panel')!.getBoundingClientRect().height) }));
    assert.deepEqual(geometry, { headingTop: 94, panelTop: 188, panelHeight: 198 });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-fastbootflash-idle.png'));
  });

  it('captures the Line Flash idle state with the WPF partition workspace', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await $('[data-page-id="LineFlash"]').click();
    await $('[aria-label="分区工作区"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-line-flash-heading .nw-page-eyebrow').getText(), 'PARTITION / WORKSPACE');
    assert.equal(await $('.nw-line-flash-partition-empty strong').getText(), '读取分区表后开始');
    assert.equal(await $('.nw-line-flash-taskbar strong').getText(), '等待读取分区表');

    const geometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-line-flash-heading')!.getBoundingClientRect().top),
      consoleTop: Math.round(document.querySelector<HTMLElement>('.nw-line-flash-console')!.getBoundingClientRect().top),
      consoleHeight: Math.round(document.querySelector<HTMLElement>('.nw-line-flash-console')!.getBoundingClientRect().height),
      taskbarTop: Math.round(document.querySelector<HTMLElement>('.nw-line-flash-taskbar')!.getBoundingClientRect().top),
      taskbarHeight: Math.round(document.querySelector<HTMLElement>('.nw-line-flash-taskbar')!.getBoundingClientRect().height),
    }));
    assertGeometryWithin(geometry, {
      headingTop: 94,
      consoleTop: 184,
      consoleHeight: 394,
      taskbarTop: 592,
      taskbarHeight: 76,
    });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-lineflash-idle.png'));
  });

  it('captures the VIVO Line Flash idle state with the WPF safe-flash workbench', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await $('[data-page-id="SafeFlash"]').click();
    await $('[aria-label="VIVO 线刷"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-safe-flash-heading .nw-page-eyebrow').getText(), 'VIVO LINE FLASH');
    assert.equal(await $('.nw-safe-flash-device-summary > strong').getText(), '未连接 ADB 设备');
    assert.equal(await $('.nw-safe-flash-statusbar button').getText(), '停止操作');

    const geometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-safe-flash-heading')!.getBoundingClientRect().top),
      consoleTop: Math.round(document.querySelector<HTMLElement>('.nw-safe-flash-console')!.getBoundingClientRect().top),
      consoleHeight: Math.round(document.querySelector<HTMLElement>('.nw-safe-flash-console')!.getBoundingClientRect().height),
      statusbarTop: Math.round(document.querySelector<HTMLElement>('.nw-safe-flash-statusbar')!.getBoundingClientRect().top),
      statusbarHeight: Math.round(document.querySelector<HTMLElement>('.nw-safe-flash-statusbar')!.getBoundingClientRect().height),
    }));
    assertGeometryWithin(geometry, {
      headingTop: 94,
      consoleTop: 184,
      consoleHeight: 426,
      statusbarTop: 610,
      statusbarHeight: 58,
    });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-safeflash-idle.png'));
  });

  it('captures the Firmware Extract idle state with the WPF payload workbench', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await $('[data-page-id="FirmwareExtract"]').click();
    await $('[aria-label="固件提取"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-firmware-extract-heading .nw-page-eyebrow').getText(), 'FIRMWARE / PAYLOAD');
    assert.equal(await $('.nw-firmware-partition-empty strong').getText(), '尚未读取分区');
    assert.equal(await $('.nw-firmware-statusbar strong').getText(), '未加载 payload');

    const geometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-firmware-extract-heading')!.getBoundingClientRect().top),
      workbenchTop: Math.round(document.querySelector<HTMLElement>('.nw-firmware-extract-workbench')!.getBoundingClientRect().top),
      workbenchHeight: Math.round(document.querySelector<HTMLElement>('.nw-firmware-extract-workbench')!.getBoundingClientRect().height),
      statusbarTop: Math.round(document.querySelector<HTMLElement>('.nw-firmware-statusbar')!.getBoundingClientRect().top),
      statusbarHeight: Math.round(document.querySelector<HTMLElement>('.nw-firmware-statusbar')!.getBoundingClientRect().height),
    }));
    assertGeometryWithin(
      geometry,
      {
        headingTop: 94,
        workbenchTop: 184,
        workbenchHeight: 396,
        statusbarTop: 594,
        statusbarHeight: 74,
      },
      { workbenchHeight: 10, statusbarTop: 10 },
    );
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-firmwareextract-idle.png'));
  });

  it('captures the Vivo ROOT idle state with the WPF patching workbench', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await mockCommand('version_check', VISUAL_STATE_FIXTURES.versionAllowed);
    await mockCommand('session_state', { has_token: true, healthy: true, running: false, session_id: null });
    await $('[data-page-id="Root"]').click();
    await $('[aria-label="Vivo ROOT"]').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-root-heading .nw-page-eyebrow').getText(), 'VIVO / ROOT WORKFLOW');
    assert.equal(await $('.nw-root-image-preflight h2').getText(), '启动镜像预检');
    assert.equal(await $('.nw-root-status-chip').getText(), '等待选择启动镜像');

    const geometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-root-heading')!.getBoundingClientRect().top),
      workbenchTop: Math.round(document.querySelector<HTMLElement>('.nw-root-workbench')!.getBoundingClientRect().top),
      workbenchHeight: Math.round(document.querySelector<HTMLElement>('.nw-root-workbench')!.getBoundingClientRect().height),
    }));
    assert.deepEqual(geometry, { headingTop: 94, workbenchTop: 182, workbenchHeight: 486 });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-roottools-idle.png'));
  });

  it('captures the Online Status idle state with the WPF session workbench', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await setNativeWpfClientSize();
    await mockCommand('online_sessions', []);
    await $('[data-page-id="Online"]').click();
    await $('[aria-label="在线状态"]').waitForDisplayed();
    await $('.nw-online-empty').waitForDisplayed();
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());

    const clientSize = await browser.execute(() => ({
      width: window.innerWidth,
      height: window.innerHeight,
      documentHeight: document.documentElement.scrollHeight,
    }));
    assert.deepEqual(clientSize, { width: 1240, height: 700, documentHeight: 700 });
    assert.equal(await $('.nw-online-heading .nw-page-eyebrow').getText(), 'ONLINE / SESSION');
    assert.equal(await $('.nw-online-empty strong').getText(), '暂无在线用户');

    const geometry = await browser.execute(() => ({
      headingTop: Math.round(document.querySelector<HTMLElement>('.nw-online-heading')!.getBoundingClientRect().top),
      workbenchTop: Math.round(document.querySelector<HTMLElement>('.nw-online-workbench')!.getBoundingClientRect().top),
      workbenchHeight: Math.round(document.querySelector<HTMLElement>('.nw-online-workbench')!.getBoundingClientRect().height),
    }));
    assert.deepEqual(geometry, { headingTop: 94, workbenchTop: 184, workbenchHeight: 484 });
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-onlinestatus-idle.png'));
  });
});

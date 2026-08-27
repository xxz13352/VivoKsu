import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { VISUAL_STATE_FIXTURES } from '../../src/test/visual-state-fixtures';
import {
  authenticateE2eUser,
  E2E_SESSION_GENERATION,
  prepareE2eLogin,
} from './authenticated-session';

const screenshotDirectory = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../../docs/migration-baselines/screenshots',
);

const mockCommand = async (command: string, value: unknown) => {
  const mock = await browser.tauri.mock(command);
  await mock.mockResolvedValue(value);
};

const openPage = async (pageId: string) => {
  await $(`[data-page-id="${pageId}"]`).click();
  const expectedTitles: Record<string, string> = {
    FileManager: '文件管理',
    QuickFlash: '快速刷写',
    LineFlash: '可视刷写',
    SafeFlash: 'VIVO 线刷',
    FirmwareExtract: '固件提取',
    Root: 'Vivo ROOT',
    Software: '软件',
  };
  await browser.waitUntil(async () => await $('h1').getText() === expectedTitles[pageId], {
    timeout: 5_000,
    timeoutMsg: `页面未切换到 ${expectedTitles[pageId]}`,
  });
};

const selectLinePartition = async (partitionName: string) => {
  const checkbox = $(`.nw-test-line-partition-select-${partitionName}`);
  await checkbox.waitForExist();
  const row = $(`//input[contains(@class, "nw-test-line-partition-select-${partitionName}")]/ancestor::li[1]`);
  await row.waitForExist();
  await row.click();
  await browser.keys('Enter');
  await expect(checkbox).toBeChecked();
};

const emitTauriEvent = async (eventName: string, eventPayload: unknown) => {
  const emitted = await browser.execute((name, payload) => {
    const runtime = window as Window & {
      __nwflash_wdio_emit_event__?: (event: string, value: unknown) => void;
    };
    if (!runtime.__nwflash_wdio_emit_event__) {
      return false;
    }
    runtime.__nwflash_wdio_emit_event__(name, payload);
    return true;
  }, eventName, eventPayload);
  assert.equal(emitted, true, `E2E 事件桥必须发送 ${eventName}`);
};

describe('奶蛙Flash interaction baseline', () => {
  beforeEach(async () => {
    await authenticateE2eUser();
    await $('h1').waitForDisplayed();
  });

  it('opens and closes the Chinese USB driver confirmation modal from the software page', async () => {
    await mockCommand('version_check', VISUAL_STATE_FIXTURES.versionAllowed);
    await mockCommand('operation_logs_snapshot', VISUAL_STATE_FIXTURES.operationLogs);
    await mockCommand('software_status', VISUAL_STATE_FIXTURES.softwareReady);

    await openPage('Software');
    await $('[aria-label="组件状态"]').waitForDisplayed();
    await $('.nw-test-driver-reinstall-open').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /USB 驱动安装/);
    assert.match(await dialog.getText(), /管理员权限/);

    await dialog.$('button=取消').click();
    await dialog.waitForDisplayed({ reverse: true });
  });

  it('shows selected missing resources in the component download modal', async () => {
    await mockCommand('version_check', VISUAL_STATE_FIXTURES.versionAllowed);
    await mockCommand('operation_logs_snapshot', VISUAL_STATE_FIXTURES.operationLogs);
    await mockCommand('software_status', VISUAL_STATE_FIXTURES.softwareMissingResources);
    await mockCommand('resource_inventory', VISUAL_STATE_FIXTURES.resourcesMissing);

    await openPage('Software');
    await $('[aria-label="组件状态"]').waitForDisplayed();
    await $('.nw-test-resource-install-open').click();

    const dialog = await $('[role="dialog"][aria-label="内置组件检查"]');
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /内置组件检查/);
    assert.match(await dialog.getText(), /scrcpy：内置缺失/);
    await expect($('.nw-test-resource-install')).toHaveText('校验所选 (2)');

    await $('.nw-test-resource-close').click();
    await dialog.waitForDisplayed({ reverse: true });
  });

  it('requires an explicit Chinese confirmation before deleting a device file', async () => {
    await mockCommand('files_list', VISUAL_STATE_FIXTURES.fileEntries);
    await mockCommand('files_delete', null);

    await openPage('FileManager');
    await $('.nw-test-file-refresh').click();
    await $('.nw-test-file-delete').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /确认删除/);
    assert.match(await dialog.getText(), /update\.zip/);
    assert.match(await dialog.getText(), /无法恢复/);

    await $('.nw-test-file-delete-confirm').click();
    await dialog.waitForDisplayed({ reverse: true });
    await expect($('.nw-file-manager-log p')).toHaveText('删除已完成');
  });

  it('confirms a dual-slot quick flash before executing the preset batch', async () => {
    await mockCommand('plugin:dialog|open', 'test-image.img');
    await mockCommand('quick_flash_inspect_image', VISUAL_STATE_FIXTURES.quickFlashImage);
    await mockCommand('quick_flash_execute_preset_images', null);

    await openPage('QuickFlash');
    await $('.nw-test-quick-flash-dual-slot').click();
    await $('.nw-test-quick-flash-switch-slot').click();
    await $('.nw-test-quick-flash-select-image').click();
    await $('[aria-label="镜像元数据"]').waitForDisplayed();
    await $('.nw-test-quick-flash-prepare-boot').click();

    await $('.nw-test-quick-flash-execute-boot').click();
    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /将刷写 1 个分区/);
    assert.match(await dialog.getText(), /双槽刷入/);
    assert.match(await dialog.getText(), /刷完切换槽位/);

    await $('.nw-test-quick-flash-confirm-boot').click();
    await dialog.waitForDisplayed({ reverse: true });
    await expect($('.nw-quick-flash-status')).toHaveText('已完成刷写 1 个预设分区');
  });

  it('preflights VIVO line flash before allowing an explicit confirmation', async () => {
    await mockCommand('safe_flash_prepare_online', VISUAL_STATE_FIXTURES.safeFlashPreflight);
    await mockCommand('safe_flash_execute_prepared', VISUAL_STATE_FIXTURES.safeFlashCompletion);

    await openPage('SafeFlash');
    await $('.nw-test-safe-flash-form button[type="submit"]').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /确认刷写/);
    assert.match(await dialog.getText(), /在线 OTA/);
    assert.match(await dialog.getText(), /可刷写分区：3\/4/);

    await dialog.$('button=确认刷写').click();
    await dialog.waitForDisplayed({ reverse: true });
    await expect($('.nw-safe-flash-current p')).toHaveText('VIVO 线刷已完成');
  });

  it('requires partition erase preflight and explicit confirmation before execution', async () => {
    await mockCommand('partitions_refresh', VISUAL_STATE_FIXTURES.partitionSnapshot);
    await mockCommand('partitions_prepare_erase', VISUAL_STATE_FIXTURES.partitionConfirmation);
    await mockCommand('partitions_execute_erase', null);

    await openPage('LineFlash');
    await $('[aria-label="分区工作区"]').waitForDisplayed();
    await $('.nw-test-line-partitions-refresh').click();
    await selectLinePartition('boot');
    await $('.nw-test-line-partitions-prepare-erase').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /确认擦除分区/);
    assert.match(await dialog.getText(), /1 个分区/);
    assert.match(await dialog.getText(), /1 个高风险分区/);

    await $('.nw-test-line-partitions-confirm-erase').click();
    await dialog.waitForDisplayed({ reverse: true });
    await expect($('.nw-line-flash-taskbar strong')).toHaveText('分区擦除已完成');
  });

  it('requires an accessible Chinese confirmation before partition write, and cancel leaves it unexecuted', async () => {
    await mockCommand('partitions_refresh', VISUAL_STATE_FIXTURES.partitionSnapshot);
    await mockCommand('plugin:dialog|open', ['boot.img']);
    await mockCommand('partitions_map_images', { mapped_count: 1 });
    await mockCommand('partitions_prepare_write', VISUAL_STATE_FIXTURES.partitionConfirmation);
    const executeWrite = await browser.tauri.mock('partitions_execute_write');
    await executeWrite.mockRejectedValue(new Error('取消后不应执行分区写入'));

    await openPage('LineFlash');
    const workspace = await $('[aria-label="分区工作区"]');
    await workspace.waitForDisplayed();
    assert.equal(await workspace.getAttribute('aria-label'), '分区工作区');
    await $('.nw-test-line-partitions-refresh').click();
    await $('.nw-test-line-partitions-select-images').click();
    await selectLinePartition('boot');
    await $('.nw-test-line-partitions-prepare-write').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.equal(await dialog.getAttribute('aria-label'), '确认写入分区');
    assert.match(await dialog.getText(), /将写入 1 个分区/);
    assert.match(await dialog.getText(), /1 个高风险分区/);
    await dialog.$('button=取消').click();
    await dialog.waitForDisplayed({ reverse: true });
    assert.doesNotMatch(await $('body').getText(), /分区写入已完成/);
    await executeWrite.update();
    assert.equal(executeWrite.mock.calls.length, 0);
  });

  it('executes partition backup immediately after choosing an output directory', async () => {
    await mockCommand('partitions_refresh', VISUAL_STATE_FIXTURES.partitionSnapshot);
    await mockCommand('plugin:dialog|open', 'C:\\backup');
    const executeBackup = await browser.tauri.mock('partitions_execute_backup');
    await executeBackup.mockResolvedValue(null);

    await openPage('LineFlash');
    const workspace = await $('[aria-label="分区工作区"]');
    await workspace.waitForDisplayed();
    assert.equal(await workspace.getAttribute('aria-label'), '分区工作区');
    await $('.nw-test-line-partitions-refresh').click();
    await selectLinePartition('boot');
    await $('.nw-test-line-partitions-backup').click();

    await expect($('.nw-line-flash-taskbar strong')).toHaveText('分区备份已完成');
    const dialog = await $('[role="dialog"]');
    assert.equal(await dialog.isExisting(), false);
    await executeBackup.update();
    assert.deepEqual(executeBackup.mock.calls, [[{
      selectedNames: ['boot'],
      outputDirectory: 'C:\\backup',
    }]]);
  });

  it('requires explicit confirmation before flashing an extracted firmware artifact', async () => {
    await mockCommand('plugin:dialog|open', 'firmware-selection');
    await mockCommand('firmware_inspect_local', VISUAL_STATE_FIXTURES.firmwareInspection);
    await mockCommand('firmware_select_output_directory', {
      selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
    const extractLocal = await browser.tauri.mock('firmware_extract_vivo_local');
    await extractLocal.mockResolvedValue(VISUAL_STATE_FIXTURES.firmwareExtraction);
    await mockCommand('firmware_prepare_extracted_artifact', { artifactId: 'firmware-artifact-boot' });
    await mockCommand('quick_flash_prepare_firmware_artifact', VISUAL_STATE_FIXTURES.firmwareArtifactConfirmation);
    await mockCommand('quick_flash_execute_firmware_artifact', null);

    await openPage('FirmwareExtract');
    await $('.nw-test-firmware-select').click();
    await $('.nw-test-firmware-inspect').click();
    await $('.nw-test-firmware-entry').waitForDisplayed();
    await $('.nw-test-firmware-entry').click();
    await $('.nw-test-firmware-extract').click();
    await $('.nw-test-firmware-flash').waitForDisplayed();
    await extractLocal.update();
    assert.deepEqual(extractLocal.mock.calls, [[{
      sourcePath: 'firmware-selection',
      selectedIds: ['firmware-entry-boot'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    }]]);
    await $('.nw-test-firmware-flash').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.match(await dialog.getText(), /确认刷入 boot 分区/);
    assert.match(await dialog.getText(), /1 个任务/);

    await $('.nw-test-firmware-confirm-flash').click();
    await dialog.waitForDisplayed({ reverse: true });
    await expect($('.nw-firmware-status')).toHaveText('镜像刷写已完成。');
  });

  it('submits only a Rust-issued output capability for remote firmware extraction', async () => {
    await mockCommand('firmware_inspect_remote', {
      format: 'zip',
      entries: [{ id: 'remote-entry-boot', name: 'boot', sizeBytes: 2048 }],
    });
    await mockCommand('firmware_select_output_directory', {
      selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
    const extractRemote = await browser.tauri.mock('firmware_extract_remote');
    await extractRemote.mockResolvedValue(VISUAL_STATE_FIXTURES.firmwareExtraction);

    await openPage('FirmwareExtract');
    await $('.nw-test-firmware-source').setValue('https://firmware.example.test/ota.zip');
    await $('.nw-test-firmware-inspect').click();
    await $('.nw-test-firmware-entry').waitForDisplayed();
    await $('.nw-test-firmware-entry').click();
    await $('.nw-test-firmware-extract').click();
    await expect($('.nw-firmware-status')).toHaveText('已提取 1 个镜像。');

    await extractRemote.update();
    assert.deepEqual(extractRemote.mock.calls, [[{
      url: 'https://firmware.example.test/ota.zip',
      selectedIds: ['remote-entry-boot'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    }]]);
    assert.doesNotMatch(JSON.stringify(extractRemote.mock.calls), /private/);
  });

  it('requires an accessible Chinese confirmation before a Safe Flash can run or be canceled', async () => {
    await mockCommand('safe_flash_prepare_online', VISUAL_STATE_FIXTURES.safeFlashPreflight);
    await mockCommand('safe_flash_cancel_prepared', null);
    await mockCommand('safe_flash_execute_prepared', VISUAL_STATE_FIXTURES.safeFlashCompletion);

    await openPage('SafeFlash');
    await $('.nw-test-safe-flash-form button[type="submit"]').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.equal(await dialog.getAttribute('aria-label'), '安全线刷确认');
    assert.match(await dialog.getText(), /在线 OTA/);
    assert.match(await dialog.getText(), /仅刷写可直接镜像分区/);

    await dialog.$('button=取消').click();
    await dialog.waitForDisplayed({ reverse: true });

    await mockCommand('safe_flash_prepare_online', VISUAL_STATE_FIXTURES.safeFlashPreflight);
    await $('.nw-test-safe-flash-form button[type="submit"]').click();
    const confirmedDialog = await $('[role="dialog"]');
    await confirmedDialog.waitForDisplayed();
    await confirmedDialog.$('button=确认刷写').click();
    await confirmedDialog.waitForDisplayed({ reverse: true });
    await expect($('.nw-safe-flash-current p')).toHaveText('VIVO 线刷已完成');
  });

  it('requires explicit ROOT artifact confirmation before the controlled Quick Flash handoff', async () => {
    await mockCommand('version_check', VISUAL_STATE_FIXTURES.versionAllowed);
    await mockCommand('session_state', VISUAL_STATE_FIXTURES.signedOutSession);
    await mockCommand('root_select_image', VISUAL_STATE_FIXTURES.rootInitBootSelection);
    await mockCommand('root_patch_vivo_ksu', VISUAL_STATE_FIXTURES.rootPatchedArtifact);
    await mockCommand('root_prepare_patched_artifact_flash', VISUAL_STATE_FIXTURES.rootPatchedFlashConfirmation);
    await mockCommand('root_execute_patched_artifact_flash', null);

    await openPage('Root');
    await $('.nw-test-root-select-init').click();
    await $('.nw-test-root-patch').click();
    await $('.nw-test-root-transfer').waitForDisplayed();
    await $('.nw-test-root-transfer').click();

    const dialog = await $('[role="dialog"]');
    await dialog.waitForDisplayed();
    assert.equal(await dialog.getAttribute('aria-label'), '确认刷写 ROOT 修补镜像');
    assert.match(await dialog.getText(), /init_boot/);
    assert.match(await dialog.getText(), /1 项刷写任务/);

    await $('.nw-test-root-confirm').click();
    await dialog.waitForDisplayed({ reverse: true });
    await browser.waitUntil(async () => (
      await $('body').getText()
    ).includes('ROOT 修补镜像刷写已完成。'));
  });

  it('logs in and out through the Tauri session commands', async () => {
    await prepareE2eLogin();
    await mockCommand('session_start', null);
    await mockCommand('session_stop', null);
    await mockCommand('auth_logout', null);

    await $('[aria-label="账号"]').setValue('acceptance-user');
    await $('[aria-label="密码"]').setValue('test-password');
    await $('[aria-label="点击登录"]').click();

    await expect($('[aria-label="退出登录"]')).toBeDisplayed();
    await expect($('[data-role="logout-button"]')).toHaveText('登出');

    await $('[aria-label="退出登录"]').click();
    await expect($('[aria-label="账号"]')).toBeDisplayed();
  });

  it('runs resource readiness before the post-login USB driver reminder', async () => {
    await prepareE2eLogin();
    await mockCommand('resource_inventory', VISUAL_STATE_FIXTURES.resourcesMissing);
    await mockCommand('software_status', VISUAL_STATE_FIXTURES.softwareMissingResources);

    await $('[aria-label="账号"]').setValue('acceptance-user');
    await $('[aria-label="密码"]').setValue('test-password');
    await $('[aria-label="点击登录"]').click();

    const resourceDialog = $('[role="dialog"][aria-label="内置组件检查"]');
    await resourceDialog.waitForDisplayed();
    assert.match(await resourceDialog.getText(), /scrcpy：内置缺失/);
    await $('.nw-test-resource-close').click();
    await resourceDialog.waitForDisplayed({ reverse: true });

    const driverDialog = $('[role="dialog"][aria-label="USB 驱动提醒"]');
    await driverDialog.waitForDisplayed();
    assert.match(await driverDialog.getText(), /缺少手机 USB 驱动/);
    assert.match(await driverDialog.getText(), /ADB 或 Fastboot/);
  });

  it('disables the accessible logout actions while a busy operation is active', async () => {
    await $('[aria-label="退出登录"]').waitForDisplayed();

    await emitTauriEvent('operation:snapshot', VISUAL_STATE_FIXTURES.partitionOperationEvent);

    assert.match(await $('[data-role="operation-progress"]').getText(), /快速刷写：正在写入 boot 分区/);
    await expect($('[aria-label="退出登录"]')).toBeDisabled();
    await expect($('[data-role="logout-button"]')).toBeDisabled();
  });

  it('renders an operation event in the accessible log and auto-scrolls to its newest entry', async () => {
    const log = await $('[aria-label="操作日志"]');
    await log.waitForDisplayed();
    for (const stage of ['正在写入 boot 分区 1', '正在写入 boot 分区 2', '正在写入 boot 分区 3', '正在写入 boot 分区 4', '正在写入 boot 分区 5']) {
      await emitTauriEvent('operation:snapshot', {
        ...VISUAL_STATE_FIXTURES.partitionOperationEvent,
        stage,
      });
    }
    await expect(log).toHaveText(expect.stringContaining('正在写入 boot 分区 5'));
    await browser.execute((element) => {
      element.scrollTop = element.scrollHeight;
    }, log);

    await emitTauriEvent('operation:snapshot', {
      ...VISUAL_STATE_FIXTURES.partitionOperationEvent,
      stage: '正在写入 boot 分区 6',
    });

    assert.match(await $('[data-role="operation-progress"]').getText(), /快速刷写：正在写入 boot 分区 6/);
    await expect(log).toHaveText(expect.stringContaining('正在写入 boot 分区 6'));
    const scrollPosition = await browser.execute((element) => ({
      scrollTop: element.scrollTop,
    }), log);
    assert.equal(scrollPosition.scrollTop, 0);
  });

  it('blocks login with a no-bypass update dialog and retains the download URL', async () => {
    await emitTauriEvent('session:update-required', {
      generation: E2E_SESSION_GENERATION,
      message: '检测到新版本要求（最低 2.1.0），请更新后继续使用。',
      latest: '2.1.0',
      minVersion: '2.1.0',
      downloadUrl: 'https://example.com/nwflash-update',
    });

    const updateDialog = $('[role="dialog"][aria-label="奶蛙Flash 需要更新"]');
    await updateDialog.waitForDisplayed();
    assert.match(await updateDialog.getText(), /检测到新版本要求/);
    assert.equal(
      await $('[aria-label="下载新版本"]').getAttribute('href'),
      'https://example.com/nwflash-update',
    );
    assert.equal(await updateDialog.$('button=关闭').isExisting(), false);
    await expect($('[aria-label="点击登录"]')).toBeDisabled();
    await browser.saveScreenshot(path.join(screenshotDirectory, 'tauri-update-required.png'));
  });
});

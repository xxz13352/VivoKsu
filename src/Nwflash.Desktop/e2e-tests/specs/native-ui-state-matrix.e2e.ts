import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  NATIVE_UI_ACCEPTANCE_STATES,
  NATIVE_UI_ACCEPTANCE_SURFACES,
  VISUAL_STATE_FIXTURES,
} from '../../src/test/visual-state-fixtures';
import { authenticateE2eUser, prepareE2eLogin } from './authenticated-session';

const screenshotDirectory = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../../docs/migration-baselines/screenshots',
);

type PendingDirectMock = {
  mockPending: () => Promise<unknown>;
};

const loginPreparedSession = async () => {
  await $('[aria-label="账号"]').setValue('acceptance-user');
  await $('[aria-label="密码"]').setValue('test-password');
  await $('[aria-label="点击登录"]').click();
  await $('aside[aria-label="主导航"]').waitForDisplayed();
  await browser.setWindowSize(1256, 709);
};

const emitRunningOperation = async () => {
  const emitted = await browser.execute((payload) => {
    const runtime = window as Window & {
      __nwflash_wdio_emit_event__?: (event: string, value: unknown) => void;
    };
    runtime.__nwflash_wdio_emit_event__?.('operation:snapshot', payload);
    return Boolean(runtime.__nwflash_wdio_emit_event__);
  }, VISUAL_STATE_FIXTURES.partitionOperationEvent);
  assert.equal(emitted, true);
};

const openSurface = async (surface: (typeof NATIVE_UI_ACCEPTANCE_SURFACES)[number]) => {
  if (surface.pageId) {
    await $(`[data-page-id="${surface.pageId}"]`).click();
  }
  await $(surface.selector).waitForDisplayed();
};

const captureStateMatrix = async (
  state: (typeof NATIVE_UI_ACCEPTANCE_STATES)[number],
) => {
  for (const surface of NATIVE_UI_ACCEPTANCE_SURFACES) {
    await openSurface(surface);
    const operationLog = $('[data-role="operation-log-panel"]');
    const progress = $('[data-role="operation-progress"]');
    const stateText = state.key === 'running'
      ? `${await progress.getText()} ${await operationLog.getText()}`
      : await operationLog.getText();
    assert.match(stateText, new RegExp(state.expectedText.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));

    const screenshotPath = path.join(
      screenshotDirectory,
      `tauri-${surface.key}-${state.key}.png`,
    );
    if (surface.key === 'operationlog') {
      await operationLog.saveScreenshot(screenshotPath);
    } else {
      await browser.saveScreenshot(screenshotPath);
    }
  }
};

describe('奶蛙Flash native 11-surface state matrix', () => {
  it('declares ten workspaces plus the permanent OperationLog surface', () => {
    assert.equal(NATIVE_UI_ACCEPTANCE_SURFACES.length, 11);
    assert.deepEqual(
      NATIVE_UI_ACCEPTANCE_STATES.map((state) => state.key),
      ['loading', 'error', 'running'],
    );
  });

  it('captures the loading state across all 11 UI surfaces', async () => {
    await prepareE2eLogin();
    const operationLogs = await browser.tauri.mock('operation_logs_snapshot');
    await (operationLogs as unknown as PendingDirectMock).mockPending();
    await loginPreparedSession();
    await captureStateMatrix(NATIVE_UI_ACCEPTANCE_STATES[0]);
  });

  it('captures the error state across all 11 UI surfaces', async () => {
    await prepareE2eLogin();
    const operationLogs = await browser.tauri.mock('operation_logs_snapshot');
    await operationLogs.mockRejectedValue(new Error('验收日志读取失败'));
    await loginPreparedSession();
    await captureStateMatrix(NATIVE_UI_ACCEPTANCE_STATES[1]);
  });

  it('captures the running state across all 11 UI surfaces', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await browser.setWindowSize(1256, 709);
    await emitRunningOperation();
    await captureStateMatrix(NATIVE_UI_ACCEPTANCE_STATES[2]);
  });

  it('captures OperationLog idle as its own Tauri artifact', async () => {
    await authenticateE2eUser({ operationLogs: [] });
    await browser.setWindowSize(1256, 709);
    const operationLog = $('[data-role="operation-log-panel"]');
    await operationLog.waitForDisplayed();
    assert.match(await operationLog.getText(), /等待操作记录/);
    await operationLog.saveScreenshot(
      path.join(screenshotDirectory, 'tauri-operationlog-idle.png'),
    );
  });
});

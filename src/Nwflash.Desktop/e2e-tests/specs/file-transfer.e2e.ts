import assert from 'node:assert/strict';
import { authenticateE2eUser } from './authenticated-session';

type FileFixture = {
  sourcePath: string;
  destinationPath: string;
  apkPath: string;
  remotePath: string;
  deviceSnapshot: {
    connection_state: 'AdbConnected';
    serial: string;
    connection_label: string;
    model: string;
    android_version: string;
    battery_level: string;
  };
};

type LedgerEntry = {
  sequence: number;
  stage: string;
  outcome: string;
  timeoutMs: number | null;
  serialFromRust: boolean;
  transactionTemp: boolean;
};

type FileSnapshot = {
  scenario: string;
  entries: LedgerEntry[];
  operationBusy: boolean;
  remoteTempExists: boolean;
  remoteFinalExists: boolean;
  destinationState: string;
  localPartialCount: number;
  sourcePreserved: boolean;
  apkPreserved: boolean;
};

const nativeInvoke = async <T>(command: string, args?: Record<string, unknown>): Promise<T> => (
  await browser.execute(async (name, payload) => (
    await window.__TAURI_INTERNALS__.invoke(name, payload)
  ), command, args)
) as T;

const configure = async (scenario: string): Promise<FileFixture> => (
  await nativeInvoke<FileFixture>('file_e2e_configure', { scenario })
);

const snapshot = async (): Promise<FileSnapshot> => (
  await nativeInvoke<FileSnapshot>('file_e2e_snapshot')
);

const mockCommand = async (command: string, value: unknown) => {
  const mock = await browser.tauri.mock(command);
  await mock.mockResolvedValue(value);
  return mock;
};

const openFileManager = async (fixture: FileFixture) => {
  await mockCommand('files_list', [{
    name: 'payload.bin',
    full_path: fixture.remotePath,
    is_directory: false,
    size_bytes: 17,
  }]);
  await browser.execute((payload) => {
    const runtime = window as Window & {
      __nwflash_wdio_emit_event__?: (event: string, value: unknown) => void;
    };
    runtime.__nwflash_wdio_emit_event__?.('device:snapshot', payload);
  }, fixture.deviceSnapshot);
  await $('[data-page-id="FileManager"]').click();
  await $('[aria-label="文件管理"]').waitForDisplayed();
  await expect($('.nw-file-manager-connection')).toHaveText('ADB E2E 已连接');
};

const startedStages = (value: FileSnapshot) => value.entries
  .filter((entry) => entry.outcome === 'started')
  .map((entry) => entry.stage);

const terminalOutcomes = (value: FileSnapshot) => value.entries
  .filter((entry) => entry.stage === 'transaction')
  .map((entry) => entry.outcome);

const waitForSnapshot = async (
  predicate: (value: FileSnapshot) => boolean,
  timeoutMs = 10_000,
): Promise<FileSnapshot> => {
  const started = Date.now();
  let current = await snapshot();
  while (!predicate(current) && Date.now() - started < timeoutMs) {
    await browser.pause(25);
    current = await snapshot();
  }
  assert.ok(predicate(current), `timed out waiting for native ledger: ${JSON.stringify(current)}`);
  return current;
};

const assertNativePath = async () => {
  const directMockCalls = await browser.execute(() => {
    const runtime = window as Window & {
      __wdio_direct_mock_calls__?: Record<string, unknown[][]>;
    };
    return {
      upload: runtime.__wdio_direct_mock_calls__?.files_upload ?? [],
      download: runtime.__wdio_direct_mock_calls__?.files_download ?? [],
      install: runtime.__wdio_direct_mock_calls__?.files_install_apk ?? [],
    };
  });
  assert.deepEqual(directMockCalls, { upload: [], download: [], install: [] });
};

describe('native file transfer transaction chain', () => {
  beforeEach(async () => {
    await authenticateE2eUser();
  });

  afterEach(async () => {
    try {
      await nativeInvoke('file_e2e_reset');
    } catch {
      // A failed test can leave the deterministic pending fixture busy.  The
      // test failure already carries the ledger; do not hide it with teardown.
    }
  });

  it('routes upload success through Tauri serde, coordinator, temp push and promote', async () => {
    const fixture = await configure('upload-success');
    await mockCommand('plugin:dialog|open', fixture.sourcePath);
    await openFileManager(fixture);

    await $('.nw-test-file-upload').click();
    await expect($('.nw-file-manager-log p')).toHaveText('上传已完成');

    const state = await waitForSnapshot((value) => terminalOutcomes(value).includes('success'));
    assert.deepEqual(startedStages(state), ['push', 'promote']);
    assert.deepEqual(
      state.entries
        .filter((entry) => entry.outcome === 'started')
        .map((entry) => entry.timeoutMs),
      [1_800_000, 30_000],
    );
    assert.ok(state.entries.filter((entry) => entry.outcome === 'started')
      .every((entry) => entry.serialFromRust && entry.transactionTemp));
    assert.equal(state.remoteTempExists, false);
    assert.equal(state.remoteFinalExists, true);
    assert.equal(state.sourcePreserved, true);
    await assertNativePath();
  });

  it('cleans every upload failure class and permits a fail-once retry', async () => {
    const failureScenarios = ['upload-nonzero', 'upload-spawn', 'upload-timeout', 'upload-output'];
    for (const scenario of failureScenarios) {
      const fixture = await configure(scenario);
      await mockCommand('plugin:dialog|open', fixture.sourcePath);
      await openFileManager(fixture);
      await $('.nw-test-file-upload').click();
      await $('.nw-error-text').waitForDisplayed();
      const failed = await waitForSnapshot((value) => terminalOutcomes(value).includes('failed'));
      assert.deepEqual(startedStages(failed), ['push', 'cleanup'], scenario);
      assert.equal(failed.remoteTempExists, false, scenario);
      assert.equal(failed.remoteFinalExists, false, scenario);
      assert.equal(failed.sourcePreserved, true, scenario);
    }

    let fixture = await configure('upload-conflict');
    await mockCommand('plugin:dialog|open', fixture.sourcePath);
    await openFileManager(fixture);
    await $('.nw-test-file-upload').click();
    await $('.nw-error-text').waitForDisplayed();
    let failed = await waitForSnapshot((value) => terminalOutcomes(value).includes('failed'));
    assert.deepEqual(startedStages(failed), ['push', 'promote', 'cleanup']);
    assert.equal(failed.remoteTempExists, false);
    assert.equal(failed.remoteFinalExists, true);

    fixture = await configure('upload-cleanup-failure');
    await mockCommand('plugin:dialog|open', fixture.sourcePath);
    await openFileManager(fixture);
    await $('.nw-test-file-upload').click();
    await $('.nw-error-text').waitForDisplayed();
    failed = await waitForSnapshot((value) => terminalOutcomes(value).includes('failed'));
    assert.deepEqual(startedStages(failed), ['push', 'cleanup']);
    assert.equal(failed.remoteTempExists, true);
    assert.equal(failed.remoteFinalExists, false);

    const retryFixture = await configure('upload-fail-once');
    await mockCommand('plugin:dialog|open', retryFixture.sourcePath);
    await openFileManager(retryFixture);
    await $('.nw-test-file-upload').click();
    await $('.nw-error-text').waitForDisplayed();
    await $('.nw-test-file-upload').click();
    await expect($('.nw-file-manager-log p')).toHaveText('上传已完成');
    const retried = await waitForSnapshot((value) => terminalOutcomes(value).at(-1) === 'success');
    assert.deepEqual(startedStages(retried), ['push', 'cleanup', 'push', 'promote']);
    assert.equal(retried.remoteTempExists, false);
    assert.equal(retried.remoteFinalExists, true);
    assert.deepEqual(terminalOutcomes(retried), ['failed', 'success']);
    await assertNativePath();
  });

  it('holds the coordinator during a pending upload, rejects concurrency, then cancels and cleans', async () => {
    const fixture = await configure('upload-cancel');
    await mockCommand('plugin:dialog|open', fixture.sourcePath);
    await openFileManager(fixture);
    await $('.nw-test-file-upload').click();
    await waitForSnapshot((value) => value.operationBusy && startedStages(value).includes('push'));

    let concurrentError = '';
    try {
      await nativeInvoke('files_install_apk', { apkPath: fixture.apkPath });
    } catch (error) {
      concurrentError = String(error);
    }
    assert.match(concurrentError, /正在进行|InProgress|已有任务/i);
    let pending = await snapshot();
    assert.ok(terminalOutcomes(pending).includes('in-progress'));
    assert.ok(!startedStages(pending).includes('install'));

    await $('.nw-test-file-cancel').click();
    pending = await waitForSnapshot((value) => terminalOutcomes(value).at(-1) === 'canceled');
    await expect($('.nw-file-manager-log p')).toHaveText('操作已取消');
    assert.deepEqual(startedStages(pending), ['push', 'cleanup']);
    assert.equal(pending.remoteTempExists, false);
    assert.equal(pending.operationBusy, false);
    assert.equal((await nativeInvoke<FileFixture>('file_e2e_configure', { scenario: 'upload-success' })).sourcePath.length > 0, true);
    await assertNativePath();
  });

  it('downloads through a sibling temp and preserves an existing or racing destination', async () => {
    let fixture = await configure('download-success');
    await mockCommand('plugin:dialog|save', fixture.destinationPath);
    await openFileManager(fixture);
    await $('.nw-test-file-refresh').click();
    await $('.nw-test-file-download').click();
    await expect($('.nw-file-manager-log p')).toHaveText('下载已完成');
    let state = await waitForSnapshot((value) => terminalOutcomes(value).includes('success'));
    assert.deepEqual(startedStages(state), ['pull']);
    assert.equal(state.destinationState, 'downloaded');
    assert.equal(state.localPartialCount, 0);

    fixture = await configure('download-failure');
    await mockCommand('plugin:dialog|save', fixture.destinationPath);
    await openFileManager(fixture);
    await $('.nw-test-file-refresh').click();
    await $('.nw-test-file-download').click();
    await $('.nw-error-text').waitForDisplayed();
    state = await waitForSnapshot((value) => terminalOutcomes(value).includes('failed'));
    assert.deepEqual(startedStages(state), ['pull']);
    assert.equal(state.destinationState, 'absent');
    assert.equal(state.localPartialCount, 0);

    fixture = await configure('download-existing');
    await mockCommand('plugin:dialog|save', fixture.destinationPath);
    await openFileManager(fixture);
    await $('.nw-test-file-refresh').click();
    await $('.nw-test-file-download').click();
    await $('.nw-error-text').waitForDisplayed();
    state = await waitForSnapshot((value) => terminalOutcomes(value).includes('failed'));
    assert.deepEqual(startedStages(state), []);
    assert.equal(state.destinationState, 'old');
    assert.equal(state.localPartialCount, 0);

    fixture = await configure('download-race');
    await mockCommand('plugin:dialog|save', fixture.destinationPath);
    await openFileManager(fixture);
    await $('.nw-test-file-refresh').click();
    await $('.nw-test-file-download').click();
    await $('.nw-error-text').waitForDisplayed();
    state = await waitForSnapshot((value) => terminalOutcomes(value).includes('failed'));
    assert.deepEqual(startedStages(state), ['pull']);
    assert.equal(state.destinationState, 'old');
    assert.equal(state.localPartialCount, 0);
    await assertNativePath();
  });

  it('cancels download and exercises APK success, failure, and cancel without file promotion', async () => {
    let fixture = await configure('download-cancel');
    await mockCommand('plugin:dialog|save', fixture.destinationPath);
    await openFileManager(fixture);
    await $('.nw-test-file-refresh').click();
    await $('.nw-test-file-download').click();
    await waitForSnapshot((value) => value.operationBusy && startedStages(value).includes('pull'));
    await $('.nw-test-file-cancel').click();
    let state = await waitForSnapshot((value) => terminalOutcomes(value).includes('canceled'));
    await expect($('.nw-file-manager-log p')).toHaveText('操作已取消');
    assert.equal(state.destinationState, 'absent');
    assert.equal(state.localPartialCount, 0);

    for (const [scenario, terminal] of [
      ['install-success', 'success'],
      ['install-failure', 'failed'],
    ] as const) {
      fixture = await configure(scenario);
      await mockCommand('plugin:dialog|open', fixture.apkPath);
      await openFileManager(fixture);
      await $('.nw-test-file-install-apk').click();
      if (terminal === 'success') {
        await expect($('.nw-file-manager-log p')).toHaveText('APK 安装已完成');
      } else {
        await $('.nw-error-text').waitForDisplayed();
      }
      state = await waitForSnapshot((value) => terminalOutcomes(value).includes(terminal));
      assert.deepEqual(startedStages(state), ['install']);
      assert.equal(state.entries.find((entry) => entry.stage === 'install')?.timeoutMs, 300_000);
      assert.equal(state.apkPreserved, true);
    }

    fixture = await configure('install-cancel');
    await mockCommand('plugin:dialog|open', fixture.apkPath);
    await openFileManager(fixture);
    await $('.nw-test-file-install-apk').click();
    await waitForSnapshot((value) => value.operationBusy && startedStages(value).includes('install'));
    await $('.nw-test-file-cancel').click();
    state = await waitForSnapshot((value) => terminalOutcomes(value).includes('canceled'));
    await expect($('.nw-file-manager-log p')).toHaveText('操作已取消');
    assert.deepEqual(startedStages(state), ['install']);
    assert.equal(state.apkPreserved, true);
    await assertNativePath();
  });
});

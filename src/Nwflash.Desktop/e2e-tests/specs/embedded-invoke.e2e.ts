import assert from 'node:assert/strict';
import { VISUAL_STATE_FIXTURES } from '../../src/test/visual-state-fixtures';
import { authenticateE2eUser } from './authenticated-session';

describe('embedded direct invoke compatibility', () => {
  beforeEach(async () => {
    await authenticateE2eUser();
  });

  it('provides numeric callback identifiers for native Tauri IPC', async () => {
    const callbackId = await browser.execute(() => (
      window.__TAURI_INTERNALS__.transformCallback(() => {})
    ));

    assert.equal(typeof callbackId, 'number');
  });

  it('registers direct app invoke responses in the E2E mock registry', async () => {
    const listFiles = await browser.tauri.mock('files_list');
    await listFiles.mockResolvedValue(VISUAL_STATE_FIXTURES.fileEntries);

    const entries = await browser.execute(async () => {
      const runtime = window as Window & {
        __wdio_mocks__?: Record<string, (args: unknown) => unknown>;
      };
      return await runtime.__wdio_mocks__?.files_list({ remoteDirectory: '/sdcard' });
    });

    assert.deepEqual(entries, VISUAL_STATE_FIXTURES.fileEntries);
  });

  it('synchronizes direct app invoke arguments into the host mock call ledger', async () => {
    const listFiles = await browser.tauri.mock('files_list');
    await listFiles.mockResolvedValue(VISUAL_STATE_FIXTURES.fileEntries);

    await browser.execute(async () => {
      const runtime = window as Window & {
        __wdio_mocks__?: Record<string, (args: unknown) => unknown>;
      };
      await runtime.__wdio_mocks__?.files_list({ remoteDirectory: '/sdcard/Download' });
    });
    await listFiles.update();

    assert.deepEqual(listFiles.mock.calls, [[{ remoteDirectory: '/sdcard/Download' }]]);
  });

  it('does not register the legacy transfer command that accepts browser device authority', async () => {
    let invocationError = '';
    try {
      await browser.execute(async () => {
        await window.__TAURI_INTERNALS__.invoke('file_transfer_build_pull_command', {
          serial: 'BROWSER-SUPPLIED-SERIAL',
          devicePath: '/sdcard/Download/update.zip',
          localPath: 'C:\\browser-supplied\\update.zip',
        });
      });
    } catch (error) {
      invocationError = String(error);
    }

    assert.match(invocationError, /not found/i);
  });
});

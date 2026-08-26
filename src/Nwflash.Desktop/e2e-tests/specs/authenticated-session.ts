import { VISUAL_STATE_FIXTURES } from '../../src/test/visual-state-fixtures';

const mockCommand = async (command: string, value: unknown) => {
  const mock = await browser.tauri.mock(command);
  await mock.mockResolvedValue(value);
};

type E2eLoginOptions = {
  operationLogs?: unknown;
};

export const prepareE2eLogin = async ({ operationLogs }: E2eLoginOptions = {}): Promise<void> => {
  await browser.tauri.restoreAllMocks();
  const navigation = $('aside[aria-label="主导航"]');
  const requiresBootstrapReset = await navigation.isExisting()
    && !(await $('[data-role="logout-button"]').isEnabled());
  if (requiresBootstrapReset) {
    await browser.refresh();
  }

  await mockCommand('session_state', VISUAL_STATE_FIXTURES.signedOutSession);
  await mockCommand('auth_validate_token', null);
  await mockCommand('version_check', VISUAL_STATE_FIXTURES.versionAllowed);
  await mockCommand('auth_login', {
    name: '验收用户',
    username: 'acceptance-user',
    generation: 'generation-e2e',
  });
  await mockCommand('session_start', null);
  await mockCommand('session_stop', null);
  await mockCommand('auth_logout', null);
  await mockCommand('resource_inventory', VISUAL_STATE_FIXTURES.resourcesReady);
  await mockCommand('software_status', VISUAL_STATE_FIXTURES.softwareReady);
  if (operationLogs !== undefined) {
    await mockCommand('operation_logs_snapshot', operationLogs);
  }

  if (!requiresBootstrapReset && await navigation.isExisting()) {
    await $('[data-role="logout-button"]').click();
  }
  await $('[data-role="login-window"]').waitForDisplayed();
};

export const authenticateE2eUser = async (options: E2eLoginOptions = {}): Promise<void> => {
  await prepareE2eLogin(options);
  await $('[aria-label="账号"]').setValue('acceptance-user');
  await $('[aria-label="密码"]').setValue('test-password');
  await $('[aria-label="点击登录"]').click();
  await $('aside[aria-label="主导航"]').waitForDisplayed();
};

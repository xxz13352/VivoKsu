export * from '@nwflash/tauri-core-native';

import { invoke as nativeInvoke } from '@nwflash/tauri-core-native';
import type { InvokeArgs, InvokeOptions } from '@nwflash/tauri-core-native';

type WdioMock = (args: InvokeArgs | undefined) => unknown;

const E2E_BOOTSTRAP_RESPONSES: Readonly<Record<string, unknown>> = {
  version_check: {
    latest: '2.0.0',
    min_version: '1.0.0',
    download_url: null,
    update_required: false,
    force_update: false,
  },
  session_state: {
    has_token: false,
    healthy: false,
    running: false,
    session_id: null,
  },
};

const getMock = (command: string): WdioMock | undefined => {
  const runtime = window as Window & {
    __wdio_mocks__?: Record<string, WdioMock>;
  };
  return runtime.__wdio_mocks__?.[command];
};

// E2E builds must route direct `invoke` imports through the plugin's mock registry.
export const invoke = async <T>(
  command: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> => {
  const mock = getMock(command);
  if (mock) {
    return await mock(args) as T;
  }

  if (command in E2E_BOOTSTRAP_RESPONSES) {
    return E2E_BOOTSTRAP_RESPONSES[command] as T;
  }

  return nativeInvoke<T>(command, args, options);
};

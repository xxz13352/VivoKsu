type DirectMock = {
  mock: { calls: unknown[][] };
  mockRejectedValue: (reason: unknown) => Promise<DirectMock>;
  mockPending: () => Promise<DirectMock>;
  mockResolvedValue: (value: unknown) => Promise<DirectMock>;
  mockReset: () => Promise<DirectMock>;
  update: () => Promise<DirectMock>;
};

const installDirectMockBridge = (browser: WebdriverIO.Browser) => {
  const mocks = new Map<string, DirectMock>();
  const extendedBrowser = browser as WebdriverIO.Browser & {
    tauri?: { mock?: unknown; restoreAllMocks?: unknown };
  };
  const tauri = extendedBrowser.tauri ?? {};
  extendedBrowser.tauri = tauri;

  const clearBrowserMocks = async () => {
    await browser.execute(() => {
      const runtime = window as Window & {
        __wdio_direct_mock_calls__?: Record<string, unknown[][]>;
        __wdio_mocks__?: Record<string, unknown>;
      };
      runtime.__wdio_mocks__ = {};
      runtime.__wdio_direct_mock_calls__ = {};
    });
  };

  const createMock = (command: string): DirectMock => {
    const mock: DirectMock = {
      mock: { calls: [] },
      mockRejectedValue: async (reason) => {
        const message = reason instanceof Error ? reason.message : String(reason);
        await browser.execute((name, rejection) => {
          const runtime = window as Window & {
            __wdio_direct_mock_calls__?: Record<string, unknown[][]>;
            __wdio_mocks__?: Record<string, (args: unknown) => unknown>;
          };
          runtime.__wdio_mocks__ ??= {};
          runtime.__wdio_direct_mock_calls__ ??= {};
          runtime.__wdio_direct_mock_calls__[name] ??= [];
          runtime.__wdio_mocks__[name] = (args) => {
            runtime.__wdio_direct_mock_calls__?.[name].push([args]);
            return Promise.reject(new Error(rejection));
          };
        }, command, message);
        return mock;
      },
      mockPending: async () => {
        await browser.execute((name) => {
          const runtime = window as Window & {
            __wdio_direct_mock_calls__?: Record<string, unknown[][]>;
            __wdio_mocks__?: Record<string, (args: unknown) => unknown>;
          };
          runtime.__wdio_mocks__ ??= {};
          runtime.__wdio_direct_mock_calls__ ??= {};
          runtime.__wdio_direct_mock_calls__[name] ??= [];
          runtime.__wdio_mocks__[name] = (args) => {
            runtime.__wdio_direct_mock_calls__?.[name].push([args]);
            return new Promise(() => undefined);
          };
        }, command);
        return mock;
      },
      mockResolvedValue: async (value) => {
        await browser.execute((name, response) => {
          const runtime = window as Window & {
            __wdio_direct_mock_calls__?: Record<string, unknown[][]>;
            __wdio_mocks__?: Record<string, (args: unknown) => unknown>;
          };
          runtime.__wdio_mocks__ ??= {};
          runtime.__wdio_direct_mock_calls__ ??= {};
          runtime.__wdio_direct_mock_calls__[name] ??= [];
          runtime.__wdio_mocks__[name] = (args) => {
            runtime.__wdio_direct_mock_calls__?.[name].push([args]);
            return response;
          };
        }, command, value);
        return mock;
      },
      mockReset: async () => {
        await browser.execute((name) => {
          const runtime = window as Window & {
            __wdio_direct_mock_calls__?: Record<string, unknown[][]>;
            __wdio_mocks__?: Record<string, unknown>;
          };
          delete runtime.__wdio_mocks__?.[name];
          delete runtime.__wdio_direct_mock_calls__?.[name];
        }, command);
        mock.mock.calls.length = 0;
        return mock;
      },
      update: async () => {
        const calls = await browser.execute((name) => {
          const runtime = window as Window & {
            __wdio_direct_mock_calls__?: Record<string, unknown[][]>;
          };
          return runtime.__wdio_direct_mock_calls__?.[name] ?? [];
        }, command);
        mock.mock.calls.splice(0, mock.mock.calls.length, ...calls);
        return mock;
      },
    };
    return mock;
  };

  tauri.mock = async (command: string) => {
    const current = mocks.get(command) ?? createMock(command);
    mocks.set(command, current);
    await current.mockReset();
    return current;
  };
  tauri.restoreAllMocks = async () => {
    mocks.clear();
    await clearBrowserMocks();
  };
};

export const mochaHooks = {
  beforeEach: async () => {
    installDirectMockBridge(browser);
  },
};

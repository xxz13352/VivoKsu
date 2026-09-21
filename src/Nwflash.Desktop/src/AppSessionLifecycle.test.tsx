import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { App } from './app/App';
import { IPC_EVENTS } from './app/ipc-events';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

vi.mock('@tauri-apps/api/dpi', () => ({
  LogicalSize: class {
    constructor(public width: number, public height: number) {}
  },
}));

const windowApi = vi.hoisted(() => ({
  center: vi.fn().mockResolvedValue(undefined),
  close: vi.fn().mockResolvedValue(undefined),
  // 窗口过渡要先读当前尺寸再逐帧 setSize + center（见 app/window-transition.ts）。
  innerSize: vi.fn().mockResolvedValue({ toLogical: () => ({ width: 400, height: 564 }) }),
  minimize: vi.fn().mockResolvedValue(undefined),
  scaleFactor: vi.fn().mockResolvedValue(1),
  setResizable: vi.fn().mockResolvedValue(undefined),
  setSize: vi.fn().mockResolvedValue(undefined),
  toggleMaximize: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(() => windowApi),
}));

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

type Unmount = ReturnType<typeof createRoot>;
type SessionListenerMap = Map<string, (event: { payload: Record<string, unknown> }) => void>;

type TauriInvokePayload = Record<string, unknown>;

type InvokeCommand = string;

type InvokeStub = (command: InvokeCommand, payload?: TauriInvokePayload) => Promise<unknown>;

const hostListeners: SessionListenerMap = new Map();
let host: HTMLDivElement;
let root: Unmount;

const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const startedAt = Date.now();
  while (!predicate() && Date.now() - startedAt < timeoutMs) {
    await flushPromises();
  }
  if (!predicate()) {
    throw new Error('timeout waiting for async state update');
  }
};

const setupMocks = () => {
  (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(
    async (event: string, handler: (event: { payload: Record<string, unknown> }) => void) => {
      hostListeners.set(event, handler);
      return () => {};
    },
  );

  const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
  const impl: InvokeStub = async (command) => {
    if (command === 'session_state') {
      return {
        has_token: true,
        healthy: false,
        running: true,
        session_id: 'signed-session',
        generation: 'generation-active',
      };
    }

    if (command === 'auth_validate_token') {
      return 'admin';
    }

    if (command === 'auth_login') {
      return {
        username: 'admin',
        name: '管理员',
        generation: 'generation-active',
      };
    }

    if (command === 'session_stop' || command === 'auth_logout') {
      return {};
    }

    if (command === 'device_refresh') {
      return {
        connection_state: 'AdbConnected',
        serial: 'RF8T123',
        connection_label: 'ADB 已连接',
        model: 'V2318A',
        android_version: '15',
        battery_level: '78%',
        active_slot: 'a',
        bootloader_state: '已锁定',
        kernel_version: '6.1.75-android14',
        verified_boot_state: '已校验',
      };
    }

    return {};
  };

  invokeMock.mockImplementation(impl);
  return hostListeners;
};

const renderApp = () => {
  flushSync(() => {
    root.render(<App />);
  });
};

describe('会话事件联动', () => {
  beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
    hostListeners.clear();

    // 与生产一致：注入层 internals 携带 invoke 即视为 Tauri 环境
    // （__TAURI__ 全局绑定已随 withGlobalTauri=false 移除）。
    (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {
      invoke: () => Promise.resolve(),
    };
    setupMocks();
  });

  afterEach(() => {
    flushSync(() => {
      root.unmount();
    });
    host.remove();
    vi.clearAllMocks();
  });

  test('会话强退事件会切换为未登录并清空顶部状态', async () => {
    renderApp();
    await flushPromises();

    const logoutBefore = host.querySelector('[data-role="logout-button"]') as HTMLButtonElement;
    expect(logoutBefore).not.toBeNull();
    expect(logoutBefore.textContent).toBe('登出');
    expect(host.textContent ?? '').not.toContain('会话已退出');

    const onForceExit = hostListeners.get(IPC_EVENTS.sessionForceExit);
    expect(onForceExit).toBeDefined();

    onForceExit?.({
      payload: { generation: 'generation-active', reason: 'token 已失效' },
    });
    await waitUntil(() => host.querySelector('[aria-label="点击登录"]') !== null);

    const loginAfter = host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement;
    expect(loginAfter).not.toBeNull();
    expect(loginAfter.textContent).toBe('登 录');
    expect(host.textContent ?? '').toContain('会话已退出');
  });

  test('更新要求事件会提示更新并切换登录态', async () => {
    renderApp();
    await flushPromises();

    const onUpdateRequired = hostListeners.get(IPC_EVENTS.sessionUpdateRequired);
    expect(onUpdateRequired).toBeDefined();
    onUpdateRequired?.({
      payload: {
        generation: 'generation-active',
        message: '请更新到2.0以继续使用',
        latest: '2.0',
        minVersion: null,
        downloadUrl: 'https://example.com/nwflash-update',
      },
    });
    await flushPromises();

    const loginButton = host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement;
    expect(loginButton).not.toBeNull();
    expect(loginButton.textContent).toBe('登 录');
    expect(host.textContent ?? '').toContain('请更新到2.0以继续使用');
    expect(host.querySelector('[role="dialog"][aria-label="奶蛙Flash 需要更新"]')).not.toBeNull();
    expect(
      (host.querySelector('[aria-label="下载新版本"]') as HTMLAnchorElement).getAttribute('href'),
    ).toBe('https://example.com/nwflash-update');
  });

  test('启动恢复期间收到更新事件时不会因监听注册竞态而丢失门禁', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'session_state') {
        return {
          has_token: true,
          healthy: true,
          running: true,
          session_id: 'signed-session',
          generation: 'generation-active',
        };
      }
      if (command === 'auth_validate_token') {
        hostListeners.get(IPC_EVENTS.sessionUpdateRequired)?.({
          payload: {
            generation: 'generation-active',
            message: '恢复会话时发现版本已停用',
            latest: '2.0.0',
            minVersion: '2.0.0',
            downloadUrl: 'https://example.com/update',
          },
        });
        return 'admin';
      }
      return {};
    });

    renderApp();
    await waitUntil(() => host.querySelector('[role="dialog"][aria-label="奶蛙Flash 需要更新"]') !== null);
    expect(host.textContent).toContain('恢复会话时发现版本已停用');
  });

  test('点击退出会调用会话与认证退出命令', async () => {
    renderApp();
    await flushPromises();

    const logoutButton = host.querySelector('[data-role="logout-button"]') as HTMLButtonElement;
    expect(logoutButton).not.toBeNull();

    logoutButton.click();
    await flushPromises();

    const calls = (invoke as ReturnType<typeof vi.fn>).mock.calls as Array<[string, ...unknown[]]>;
    const hasSessionStop = calls.some(([command]) => command === 'session_stop');
    const hasAuthLogout = calls.some(([command]) => command === 'auth_logout');
    const hasSessionState = calls.some(([command]) => command === 'session_state');

    expect(hasSessionStop).toBe(true);
    expect(hasAuthLogout).toBe(true);
    expect(hasSessionState).toBe(true);
    expect(host.querySelector('[aria-label="点击登录"]')).not.toBeNull();
  });

  test('主窗口标题栏按钮调用当前 Tauri 窗口操作', async () => {
    renderApp();
    await flushPromises();

    (host.querySelector('[aria-label="最小化"]') as HTMLButtonElement).click();
    (host.querySelector('[aria-label="最大化"]') as HTMLButtonElement).click();
    (host.querySelector('.nw-titlebar-controls [aria-label="关闭"]') as HTMLButtonElement).click();
    await flushPromises();

    expect(windowApi.minimize).toHaveBeenCalledOnce();
    expect(windowApi.toggleMaximize).toHaveBeenCalledOnce();
    expect(windowApi.close).toHaveBeenCalledOnce();
  });

  test('关闭已登录主窗口前停止会话并清理认证状态', async () => {
    renderApp();
    await flushPromises();

    (host.querySelector('.nw-titlebar-controls [aria-label="关闭"]') as HTMLButtonElement).click();

    await waitUntil(() => windowApi.close.mock.calls.length === 1);
    const calls = (invoke as ReturnType<typeof vi.fn>).mock.calls as Array<[string, ...unknown[]]>;
    const sessionStopIndex = calls.findIndex(([command]) => command === 'session_stop');
    const authLogoutIndex = calls.findIndex(([command]) => command === 'auth_logout');
    expect(sessionStopIndex).toBeGreaterThan(-1);
    expect(authLogoutIndex).toBeGreaterThan(sessionStopIndex);
  });

  test('operation:snapshot 事件会更新右上角进度与登出禁用态', async () => {
    renderApp();
    await flushPromises();

    const onOperationSnapshot = hostListeners.get(IPC_EVENTS.operationSnapshot);
    expect(onOperationSnapshot).toBeDefined();
    onOperationSnapshot?.({
      payload: {
        kind: 'Flashing',
        operationId: 'op-1',
        title: '快速刷写',
        stage: '正在写入 boot',
        progress: 42,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      },
    });
    await flushPromises();

    const progress = host.querySelector('[data-role="operation-progress"]') as HTMLParagraphElement;
    expect(progress).not.toBeNull();
    expect(progress.textContent).toContain('快速刷写');
    expect(progress.textContent).toContain('正在写入 boot');

    const logout = host.querySelector('[data-role="logout-button"]') as HTMLButtonElement;
    expect(logout.disabled).toBe(true);
  });

  test('operation:snapshot 结束后恢复无任务状态', async () => {
    renderApp();
    await flushPromises();

    const onOperationSnapshot = hostListeners.get(IPC_EVENTS.operationSnapshot);
    expect(onOperationSnapshot).toBeDefined();
    onOperationSnapshot?.({
      payload: {
        kind: 'Flashing',
        operationId: 'op-1',
        title: '快速刷写',
        stage: '正在写入 boot',
        progress: 42,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      },
    });
    await flushPromises();

    onOperationSnapshot?.({
      payload: {
        kind: 'Completed',
        operationId: 'op-1',
        title: '快速刷写',
        stage: '完成',
        progress: 100,
        startedAt: 1700000000,
        isCancellable: false,
        isBusy: false,
      },
    });
    await flushPromises();

    const progress = host.querySelector('[data-role="operation-progress"]') as HTMLParagraphElement;
    await waitUntil(() => progress.textContent?.includes('无进行中的操作') ?? false);
    expect(progress.textContent).toContain('无进行中的操作');

    const logout = host.querySelector('[data-role="logout-button"]') as HTMLButtonElement;
    expect(logout.disabled).toBe(false);
  });

  test('device:snapshot 事件会将最新设备快照交给设备概览页', async () => {
    renderApp();
    await flushPromises();

    const onDeviceSnapshot = hostListeners.get(IPC_EVENTS.deviceSnapshot);
    expect(onDeviceSnapshot).toBeDefined();
    onDeviceSnapshot?.({
      payload: {
        connection_state: 'AdbConnected',
        serial: 'RF8T123',
        connection_label: 'ADB 已连接',
        model: 'V2318A',
        android_version: '15',
        battery_level: '78%',
      },
    });
    await waitUntil(() => (host.textContent ?? '').includes('RF8T123'));

    expect(host.textContent).toContain('RF8T123');
    expect(host.textContent).toContain('V2318A');
  });

  test('任务结束后主动重读设备，概览页立即恢复显示设备档案', async () => {
    renderApp();
    await flushPromises();

    const refreshCalls = () =>
      (invoke as ReturnType<typeof vi.fn>).mock.calls.filter(
        ([command]) => command === 'device_refresh',
      ).length;
    // 等启动期读取（外壳 + 概览页挂载）全部落地再取基线，
    // 否则会把挂载读取误判成任务结束后的重读。
    const settleRefreshCalls = async (): Promise<number> => {
      let previous = -1;
      while (previous !== refreshCalls()) {
        previous = refreshCalls();
        await flushPromises();
      }
      return previous;
    };
    await waitUntil(() => (host.textContent ?? '').includes('RF8T123'));
    const beforeBusy = await settleRefreshCalls();

    const onOperationSnapshot = hostListeners.get(IPC_EVENTS.operationSnapshot);
    expect(onOperationSnapshot).toBeDefined();
    onOperationSnapshot?.({
      payload: {
        kind: 'Flashing',
        operationId: 'op-1',
        title: '快速刷写',
        stage: '正在写入 boot',
        progress: 42,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      },
    });
    await flushPromises();
    // 任务进行中不重复探测：后端此刻也会暂停发现。
    expect(refreshCalls()).toBe(beforeBusy);

    onOperationSnapshot?.({
      payload: {
        kind: 'Completed',
        operationId: 'op-1',
        title: '快速刷写',
        stage: '完成',
        progress: 100,
        startedAt: 1700000000,
        isCancellable: false,
        isBusy: false,
      },
    });
    await waitUntil(() => refreshCalls() > beforeBusy);

    expect(host.textContent).toContain('RF8T123');
    expect(host.textContent).toContain('已锁定');
    expect(host.textContent).toContain('6.1.75-android14');
  });
});

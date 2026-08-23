import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { App, rememberBoundedTerminalGeneration } from './app/App';
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

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(() => ({
    close: vi.fn().mockResolvedValue(undefined),
    setResizable: vi.fn().mockResolvedValue(undefined),
    setSize: vi.fn().mockResolvedValue(undefined),
  })),
}));

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

type Unmount = ReturnType<typeof createRoot>;

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

const setInputValue = (input: HTMLInputElement, value: string) => {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
  setter?.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
};

const setupInvokeMocks = () => {
  const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
  invokeMock.mockImplementation(async (command: string) => {
    if (command === 'version_check') {
      return {
        latest: '2.0.0',
        min_version: '2.0.0',
        minVersion: '2.0.0',
        download_url: null,
        update_required: false,
        force_update: false,
      };
    }

    if (command === 'session_state') {
      return {
        has_token: false,
        healthy: false,
        running: false,
        session_id: null,
      };
    }

    if (command === 'auth_login') {
      return {
        username: 'admin',
        name: '管理员',
        generation: 'generation-default',
      };
    }

    return {};
  });

  (listen as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(() => {});
};

describe('登录态界面', () => {
  beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
    (window as Window & { __TAURI__?: unknown }).__TAURI__ = {};
    setupInvokeMocks();
  });

  afterEach(() => {
    flushSync(() => {
      root.unmount();
    });
    host.remove();
    vi.clearAllMocks();
  });

  test('未登录时只显示阻断式登录窗口而不暴露主壳体', async () => {
    flushSync(() => {
      root.render(<App />);
    });
    await flushPromises();

    const username = host.querySelector('[data-role="login-username"]') as HTMLInputElement;
    const password = host.querySelector('[data-role="login-password"]') as HTMLInputElement;
    const loginButton = host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement;

    expect(username).not.toBeNull();
    expect(password).not.toBeNull();
    expect(loginButton).not.toBeNull();
    expect(loginButton.disabled).toBe(true);
    expect(host.querySelector('[data-role="login-window"]')).not.toBeNull();
    expect(host.querySelector('img[alt="奶蛙Flash"]')).not.toBeNull();
    expect(host.querySelector('[aria-label="关闭"]')).not.toBeNull();
    expect(host.querySelector('.nw-login-password-toggle')?.textContent).toBe('\uE8D3');
    expect(host.querySelector('aside[aria-label="主导航"]')).toBeNull();
    expect(host.querySelector('.nw-shell')).toBeNull();
  });

  test('会话 token 校验失败会回退到登录页', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return {
          latest: '2.0.0',
          min_version: '2.0.0',
          minVersion: '2.0.0',
          download_url: null,
          update_required: false,
          force_update: false,
        };
      }

      if (command === 'session_state') {
        return {
          has_token: true,
          healthy: true,
          running: false,
          session_id: null,
        };
      }

      if (command === 'auth_validate_token') {
        throw new Error('token 失效');
      }

      return {};
    });

    flushSync(() => {
      root.render(<App />);
    });
    await flushPromises();

    const loginButton = host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement;
    expect(loginButton).not.toBeNull();
    expect(loginButton.disabled).toBe(true);
  });

  test('停止的签名会话不能仅凭 has_token 恢复主界面', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return {
          latest: '2.0.0',
          min_version: '1.0.0',
          download_url: null,
          update_required: false,
          force_update: false,
        };
      }
      if (command === 'session_state') {
        return {
          has_token: true,
          healthy: false,
          running: false,
          session_id: 'signed-session',
        };
      }
      if (command === 'auth_validate_token') {
        return '不应恢复的用户';
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(
      () => invokeMock.mock.calls.some(([command]) => command === 'session_state'),
    );
    await flushPromises();
    await flushPromises();

    expect(host.querySelector('.nw-shell')).toBeNull();
    expect(
      invokeMock.mock.calls.some(([command]) => command === 'auth_validate_token'),
    ).toBe(false);
  });

  test('旧启动校验在新登录成功后失败时不能登出新一代会话', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    let rejectStartupValidation: ((reason: unknown) => void) | undefined;
    const startupValidation = new Promise<never>((_resolve, reject) => {
      rejectStartupValidation = reject;
    });
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return { update_required: false, force_update: false };
      }
      if (command === 'session_state') {
        return {
          has_token: true,
          healthy: true,
          running: true,
          session_id: 'startup-session',
          generation: 'generation-startup',
        };
      }
      if (command === 'auth_validate_token') {
        return startupValidation;
      }
      if (command === 'auth_login') {
        return { username: 'new-user', name: 'New User', generation: 'generation-new' };
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(
      () => invokeMock.mock.calls.some(([command]) => command === 'auth_validate_token'),
    );
    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'new-user');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, 'secret');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-shell') !== null);

    rejectStartupValidation?.('old startup rejected');
    await flushPromises();
    await flushPromises();

    expect(host.querySelector('.nw-shell')).not.toBeNull();
    expect(host.textContent).toContain('New User');
  });

  test('未知代旧 update 先到且 session_state 后返回不同新代时不得污染新会话', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    const listeners = new Map<string, (event: { payload: Record<string, unknown> }) => void>();
    let resolveSessionState:
      | ((value: {
          has_token: boolean;
          healthy: boolean;
          running: boolean;
          session_id: string;
          generation: string;
        }) => void)
      | undefined;
    const pendingSessionState = new Promise<{
      has_token: boolean;
      healthy: boolean;
      running: boolean;
      session_id: string;
      generation: string;
    }>((resolve) => {
      resolveSessionState = resolve;
    });
    (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (event: string, handler: (event: { payload: Record<string, unknown> }) => void) => {
        listeners.set(event, handler);
        return () => {};
      },
    );
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return { update_required: false, force_update: false };
      }
      if (command === 'session_state') {
        return pendingSessionState;
      }
      if (command === 'auth_validate_token') {
        return 'Live User';
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(
      () =>
        listeners.has(IPC_EVENTS.sessionUpdateRequired) &&
        invokeMock.mock.calls.some(([command]) => command === 'session_state'),
    );
    listeners.get(IPC_EVENTS.sessionUpdateRequired)?.({
      payload: {
        generation: 'generation-old',
        message: 'stale old update',
        latest: '9.9.9',
        minVersion: '9.9.9',
        downloadUrl: null,
      },
    });
    resolveSessionState?.({
      has_token: true,
      healthy: true,
      running: true,
      session_id: 'live-session',
      generation: 'generation-live',
    });

    await waitUntil(() => host.querySelector('.nw-shell') !== null);
    expect(host.textContent).toContain('Live User');
    expect(host.textContent).not.toContain('stale old update');
    expect(host.querySelector('[role="dialog"][aria-label="奶蛙Flash 需要更新"]')).toBeNull();
  });

  test('终止代存储容量固定并按插入顺序淘汰最旧代', () => {
    const terminal = new Map<string, string>();
    for (let index = 0; index < 33; index += 1) {
      rememberBoundedTerminalGeneration(
        terminal,
        `generation-${index}`,
        `terminal-${index}`,
      );
    }

    expect(terminal.size).toBe(32);
    expect(terminal.has('generation-0')).toBe(false);
    expect(terminal.get('generation-32')).toBe('terminal-32');
  });

  test('启动版本检查命中更新要求时阻止登录并提示更新', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return {
          latest: '2.1.0',
          min_version: '2.1.0',
          minVersion: '2.1.0',
          download_url: 'https://example.com/update',
          update_required: true,
          force_update: false,
        };
      }

      if (command === 'session_state') {
        return {
          has_token: false,
          healthy: false,
          running: false,
          session_id: null,
        };
      }

      return {};
    });

    flushSync(() => {
      root.render(<App />);
    });
    await flushPromises();

    const username = host.querySelector('[data-role="login-username"]') as HTMLInputElement;
    const password = host.querySelector('[data-role="login-password"]') as HTMLInputElement;
    const loginButton = host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement;

    expect(username).not.toBeNull();
    expect(password).not.toBeNull();
    expect(loginButton).not.toBeNull();

    username.value = 'admin';
    username.dispatchEvent(new Event('change', { bubbles: true }));
    password.value = '123456';
    password.dispatchEvent(new Event('change', { bubbles: true }));

    await flushPromises();

    expect(loginButton.disabled).toBe(true);
    expect(host.textContent ?? '').toContain('检测到新版本要求');
    const updateDialog = host.querySelector('[role="dialog"][aria-label="奶蛙Flash 需要更新"]');
    const downloadLink = host.querySelector('[aria-label="下载新版本"]') as HTMLAnchorElement;
    expect(updateDialog).not.toBeNull();
    expect(downloadLink.getAttribute('href')).toBe('https://example.com/update');
    expect(
      (invoke as ReturnType<typeof vi.fn>).mock.calls.some(([command]) => command === 'auth_login'),
    ).toBe(false);
  });

  test('登录成功后依次执行资源与驱动就绪检查并显示对应提醒', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return {
          latest: '2.0.0',
          min_version: '1.0.0',
          download_url: null,
          update_required: false,
          force_update: false,
        };
      }
      if (command === 'session_state') {
        return { has_token: false, healthy: false, running: false, session_id: null };
      }
      if (command === 'auth_login') {
        return { username: 'admin', name: '管理员', generation: 'generation-login' };
      }
      if (command === 'resource_inventory') {
        return [
          { key: 'scrcpy', display_name: 'scrcpy 投屏', is_ready: false, default_selected: true },
        ];
      }
      if (command === 'software_status') {
        return { adb_driver_installed: false, fastboot_driver_installed: true };
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(() => host.querySelector('[aria-label="账号"]') !== null);

    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'admin');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, '123456');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();

    await waitUntil(
      () => host.querySelector('[role="dialog"][aria-label="内置组件检查"]') !== null,
    );
    let commands = invokeMock.mock.calls.map(([command]) => command);
    expect(commands).not.toContain('session_start');
    expect(commands).toContain('resource_inventory');
    expect(commands).not.toContain('software_status');

    (host.querySelector('.nw-test-resource-close') as HTMLButtonElement).click();
    await waitUntil(
      () => host.querySelector('[role="dialog"][aria-label="USB 驱动提醒"]') !== null,
    );

    commands = invokeMock.mock.calls.map(([command]) => command);
    expect(commands).toContain('software_status');
    expect(host.textContent).toContain('缺少手机 USB 驱动');
  });

  test('登录 invoke 启动后立即清空密码且失败时保持清空', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    let rejectLogin: ((reason: unknown) => void) | undefined;
    const deferredLogin = new Promise<never>((_resolve, reject) => {
      rejectLogin = reject;
    });
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return {
          latest: '1.0.1',
          min_version: '1.0.1',
          download_url: null,
          update_required: false,
          force_update: false,
        };
      }
      if (command === 'session_state') {
        return { has_token: false, healthy: false, running: false, session_id: null };
      }
      if (command === 'auth_login') {
        return deferredLogin;
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(() => host.querySelector('[aria-label="账号"]') !== null);

    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'test');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, 'one-use-secret');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();

    await waitUntil(
      () => invokeMock.mock.calls.some(([command]) => command === 'auth_login'),
    );
    const loginCall = invokeMock.mock.calls.find(([command]) => command === 'auth_login');
    expect(loginCall?.[1]).toEqual({ username: 'test', password: 'one-use-secret' });
    expect((host.querySelector('[aria-label="密码"]') as HTMLInputElement).value).toBe('');

    rejectLogin?.('登录失败');
    await waitUntil(() => host.querySelector('.nw-login-notice')?.textContent === '登录失败');
    expect((host.querySelector('[aria-label="密码"]') as HTMLInputElement).value).toBe('');
  });

  test('登录响应前同代 force-exit 到达时后续成功响应不能恢复登录', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    const listeners = new Map<string, (event: { payload: Record<string, unknown> }) => void>();
    let resolveLogin: ((value: { username: string; name: string; generation: string }) => void)
      | undefined;
    const deferredLogin = new Promise<{ username: string; name: string; generation: string }>(
      (resolve) => {
        resolveLogin = resolve;
      },
    );
    (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (event: string, handler: (event: { payload: Record<string, unknown> }) => void) => {
        listeners.set(event, handler);
        return () => {};
      },
    );
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return { update_required: false, force_update: false };
      }
      if (command === 'session_state') {
        return { has_token: false, healthy: false, running: false, session_id: null, generation: null };
      }
      if (command === 'auth_login') {
        return deferredLogin;
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(() => listeners.has(IPC_EVENTS.sessionForceExit));
    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'test');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, 'secret');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();
    await waitUntil(() => invokeMock.mock.calls.some(([command]) => command === 'auth_login'));

    listeners.get(IPC_EVENTS.sessionForceExit)?.({
      payload: { generation: 'generation-new', reason: 'terminal-before-response' },
    });
    resolveLogin?.({ username: 'test', name: 'Test', generation: 'generation-new' });
    await flushPromises();
    await flushPromises();

    expect(host.querySelector('.nw-shell')).toBeNull();
    expect(host.textContent).toContain('会话已退出');
  });

  test('登录响应前同代 update 到达时后续成功响应不能越过更新门禁', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    const listeners = new Map<string, (event: { payload: Record<string, unknown> }) => void>();
    let resolveLogin: ((value: { username: string; name: string; generation: string }) => void)
      | undefined;
    const deferredLogin = new Promise<{ username: string; name: string; generation: string }>(
      (resolve) => {
        resolveLogin = resolve;
      },
    );
    (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (event: string, handler: (event: { payload: Record<string, unknown> }) => void) => {
        listeners.set(event, handler);
        return () => {};
      },
    );
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return { update_required: false, force_update: false };
      }
      if (command === 'session_state') {
        return { has_token: false, healthy: false, running: false, session_id: null, generation: null };
      }
      if (command === 'auth_login') {
        return deferredLogin;
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(() => listeners.has(IPC_EVENTS.sessionUpdateRequired));
    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'test');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, 'secret');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();
    await waitUntil(() => invokeMock.mock.calls.some(([command]) => command === 'auth_login'));

    listeners.get(IPC_EVENTS.sessionUpdateRequired)?.({
      payload: {
        generation: 'generation-new',
        message: 'terminal-update-before-response',
        latest: '2.0.0',
        minVersion: '2.0.0',
        downloadUrl: null,
      },
    });
    resolveLogin?.({ username: 'test', name: 'Test', generation: 'generation-new' });
    await flushPromises();
    await flushPromises();

    expect(host.querySelector('.nw-shell')).toBeNull();
    expect(host.textContent).toContain('terminal-update-before-response');
  });

  test('新登录完成后延迟到达的旧代终止事件不能影响替换会话', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    const listeners = new Map<string, (event: { payload: Record<string, unknown> }) => void>();
    (listen as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (event: string, handler: (event: { payload: Record<string, unknown> }) => void) => {
        listeners.set(event, handler);
        return () => {};
      },
    );
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return { update_required: false, force_update: false };
      }
      if (command === 'session_state') {
        return { has_token: false, healthy: false, running: false, session_id: null, generation: null };
      }
      if (command === 'auth_login') {
        return { username: 'test', name: 'Test', generation: 'generation-new' };
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(() => listeners.has(IPC_EVENTS.sessionForceExit));
    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'test');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, 'secret');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-shell') !== null);

    listeners.get(IPC_EVENTS.sessionForceExit)?.({
      payload: { generation: 'generation-old', reason: 'stale-old-terminal' },
    });
    await flushPromises();

    expect(host.querySelector('.nw-shell')).not.toBeNull();
    expect(host.textContent).not.toContain('stale-old-terminal');
  });

  test('Rust 返回字符串错误时显示实际登录错误而不是伪装成密码错误', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return {
          latest: '1.0.1',
          min_version: '1.0.1',
          download_url: null,
          update_required: false,
          force_update: false,
        };
      }
      if (command === 'session_state') {
        return { has_token: false, healthy: false, running: false, session_id: null };
      }
      if (command === 'auth_login') {
        throw '网络错误: TLS 连接被服务器关闭';
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(() => host.querySelector('[aria-label="账号"]') !== null);

    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'test');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, 'test666');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('.nw-login-notice')?.textContent?.length !== 0);
    expect(host.querySelector('.nw-login-notice')?.textContent).toContain('TLS 连接被服务器关闭');
    expect(host.querySelector('.nw-login-notice')?.textContent).not.toContain('请检查账号和密码');
  });

  test('登录接口返回更新要求时进入强制更新门禁', async () => {
    const invokeMock = invoke as unknown as ReturnType<typeof vi.fn>;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === 'version_check') {
        return {
          latest: '1.0.1',
          min_version: '1.0.1',
          download_url: null,
          update_required: false,
          force_update: false,
        };
      }
      if (command === 'session_state') {
        return { has_token: false, healthy: false, running: false, session_id: null };
      }
      if (command === 'auth_login') {
        throw '需要更新: 请更新到 2.0.0 后继续使用';
      }
      return {};
    });

    flushSync(() => root.render(<App />));
    await waitUntil(() => host.querySelector('[aria-label="账号"]') !== null);

    setInputValue(host.querySelector('[aria-label="账号"]') as HTMLInputElement, 'test');
    setInputValue(host.querySelector('[aria-label="密码"]') as HTMLInputElement, 'test666');
    await waitUntil(
      () => !(host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).disabled,
    );
    (host.querySelector('[aria-label="点击登录"]') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('[role="dialog"][aria-label="奶蛙Flash 需要更新"]') !== null);
    expect(host.textContent).toContain('需要更新');
  });
});

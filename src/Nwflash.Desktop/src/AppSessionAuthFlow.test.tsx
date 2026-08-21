import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { App } from './app/App';

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
        token: 'jwt',
        username: 'admin',
        name: '管理员',
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
        return { token: 'jwt', username: 'admin', name: '管理员' };
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
      () => host.querySelector('[role="dialog"][aria-label="组件安装"]') !== null,
    );
    let commands = invokeMock.mock.calls.map(([command]) => command);
    expect(commands).toContain('session_start');
    const sessionStartCall = invokeMock.mock.calls.find(([command]) => command === 'session_start');
    expect(sessionStartCall?.[1]).toEqual({ sessionId: expect.any(String) });
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

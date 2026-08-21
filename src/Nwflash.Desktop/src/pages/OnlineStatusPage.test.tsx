import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { OnlineStatusPage } from './OnlineStatusPage';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';

type Unmount = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: Unmount;
const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const start = Date.now();
  while (!predicate() && Date.now() - start < timeoutMs) {
    await flushPromises();
  }
  if (!predicate()) {
    throw new Error('timeout waiting for async assertion');
  }
};

const renderOnlineStatus = () => {
  flushSync(() => {
    root.render(<OnlineStatusPage />);
  });
};

describe('OnlineStatusPage', () => {
  beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(() => {
    flushSync(() => {
      root.unmount();
    });
    host.remove();
    vi.clearAllMocks();
  });

  test('renders the WPF online-session workbench empty state', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    renderOnlineStatus();
    await waitUntil(() => host.querySelector('.nw-online-empty') !== null);

    expect(host.querySelector('.nw-online-heading .nw-page-eyebrow')?.textContent).toBe('ONLINE / SESSION');
    expect(host.querySelector('.nw-online-session-bar strong')?.textContent).toBe('在线会话');
    expect(host.querySelector('.nw-online-empty strong')?.textContent).toBe('暂无在线用户');
  });

  test('成功返回在线会话时展示列表与当前设备标识', async () => {
    const nowEpochSeconds = Math.floor(Date.now() / 1_000);
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        name: '张三',
        client_version: '1.0.0',
        connected_at: nowEpochSeconds - 360,
        last_seen_at: nowEpochSeconds,
        duration_seconds: 360,
        is_self: true,
      },
      {
        name: '李四',
        client_version: '1.0.1',
        connected_at: nowEpochSeconds - 120,
        last_seen_at: nowEpochSeconds,
        duration_seconds: 120,
        is_self: false,
      },
    ]);

    renderOnlineStatus();

    await waitUntil(() => {
      const items = host.querySelectorAll('.nw-test-online-item');
      return items.length === 2;
    });

    expect(invoke).toHaveBeenCalledWith('online_sessions');
    const selfMarker = host.querySelector('.nw-online-self-label') as HTMLSpanElement | null;
    expect(selfMarker).not.toBeNull();
    expect(host.textContent ?? '').toContain('张三');
    expect(host.textContent ?? '').toContain('李四');
    expect(host.textContent ?? '').toMatch(/在线时长：00:06:0[0-2]/);
    expect(host.textContent ?? '').toMatch(/在线时长：00:02:0[0-2]/);
  });

  test('按连接时间实时计算时长并投影心跳与刷新状态', async () => {
    const connectedAt = Math.floor(Date.now() / 1_000) - 62;
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation((command: string) => {
      if (command === 'online_sessions') {
        return Promise.resolve([
          {
            name: '张三',
            client_version: '1.0.1',
            connected_at: connectedAt,
            last_seen_at: connectedAt + 60,
            duration_seconds: 360,
            is_self: true,
          },
        ]);
      }
      if (command === 'session_state') {
        return Promise.resolve({ running: true, healthy: true });
      }
      return Promise.resolve(null);
    });

    renderOnlineStatus();

    await waitUntil(() => /在线时长：00:01:0[2-4]/.test(host.textContent ?? ''));

    expect(host.textContent).toContain('心跳:正常');
    expect(host.querySelector('.nw-online-last-updated')?.textContent).not.toBe('尚未刷新');
    expect(invoke).toHaveBeenCalledWith('session_state');
  });

  test('在线会话为空时展示空态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    renderOnlineStatus();
    await waitUntil(() => host.querySelector('.nw-empty-online') !== null);

    expect(host.querySelector('.nw-empty-online strong')?.textContent).toBe('暂无在线用户');
    expect(host.querySelectorAll('.nw-test-online-item').length).toBe(0);
  });

  test('接口异常时展示错误信息', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('rpc failed'));

    renderOnlineStatus();
    await waitUntil(() => host.querySelector('.nw-error-text') !== null);

    const error = host.querySelector('.nw-error-text') as HTMLParagraphElement;
    expect(error).not.toBeNull();
    expect(error.textContent).toBe('rpc failed');
  });

  test('点击刷新会再次触发查询', async () => {
    const mockFn = invoke as unknown as ReturnType<typeof vi.fn>;
    mockFn.mockResolvedValue([]);
    renderOnlineStatus();

    const onlineCallCount = () => mockFn.mock.calls.filter(([command]) => command === 'online_sessions').length;
    await waitUntil(() => onlineCallCount() === 1);
    await waitUntil(() => {
      const button = host.querySelector('.nw-test-online-refresh') as HTMLButtonElement | null;
      return button !== null && !button.disabled;
    });
    const button = host.querySelector('.nw-test-online-refresh') as HTMLButtonElement;
    button.click();

    await waitUntil(() => onlineCallCount() === 2);
    expect(onlineCallCount()).toBe(2);
  });
});

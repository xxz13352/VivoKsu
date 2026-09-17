import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { MirrorPage } from './MirrorPage';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';

type RootHandle = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: RootHandle;
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

const renderMirror = () => {
  flushSync(() => {
    root.render(<MirrorPage />);
  });
};

describe('MirrorPage', () => {
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

  test('加载成功后展示投屏状态且不再拉取操作日志', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      is_mirroring: true,
      auto_mirror_enabled: true,
    });

    renderMirror();

    await waitUntil(() => (host.textContent ?? '').includes('投屏运行中'));

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('mirror_status');
    expect(invoke).not.toHaveBeenCalledWith('operation_logs_snapshot');
    const toggle = host.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(toggle.checked).toBe(true);
  });

  test('使用 WPF 的 SCRCPY 控制台结构呈现空闲投屏状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      is_mirroring: false,
      auto_mirror_enabled: false,
    });

    renderMirror();

    await waitUntil(() => (host.textContent ?? '').includes('屏幕镜像控制台'));
    expect(host.querySelector('.nw-mirror-page')).not.toBeNull();
    expect(host.querySelector('.nw-mirror-console')).not.toBeNull();
    expect(host.textContent).toContain('屏幕镜像控制台');
    expect(host.textContent).toContain('手动投屏');
    expect(host.textContent).toContain('自动投屏');
    expect(host.textContent).toContain('设备传输');
    expect(host.textContent).toContain('镜像进程');
    expect(host.textContent).not.toContain('scrcpy 已就绪');
    expect(host.textContent).not.toContain('mirror-session');
  });

  test('刷新按钮重新读取投屏状态且不伪装成文件选择', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      is_mirroring: false,
      auto_mirror_enabled: false,
    });

    renderMirror();
    await waitUntil(() => (host.textContent ?? '').includes('屏幕镜像控制台'));

    const button = host.querySelector('.nw-test-mirror-refresh') as HTMLButtonElement;
    expect(button.textContent).toBe('刷新状态');
    button.click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length >= 2);
    expect((invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  test('mirror_status 返回非法结构时回退为未启动状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({ a: 1 });

    renderMirror();

    await waitUntil(() => (host.textContent ?? '').includes('镜像进程'));
    expect(host.textContent).toContain('投屏未启动');
    expect(host.querySelector('.nw-empty-log')).toBeNull();
  });

  test('读取失败时展示错误', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('mirror rpc failed'));

    renderMirror();

    await waitUntil(() => host.querySelector('.nw-error-text') !== null);
    const err = host.querySelector('.nw-error-text') as HTMLParagraphElement;
    expect(err).not.toBeNull();
    expect(err.textContent).toBe('mirror rpc failed');
  });

  test('开始投屏只调用受限 mirror_start 命令并显示运行状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ is_mirroring: false, auto_mirror_enabled: false })
      .mockResolvedValueOnce({ is_mirroring: true, auto_mirror_enabled: false });
    renderMirror();

    await waitUntil(() => (host.textContent ?? '').includes('投屏未启动'));
    (host.querySelector('.nw-test-mirror-start') as HTMLButtonElement).click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);
    await waitUntil(() => host.textContent?.includes('投屏运行中') ?? false);
    expect(invoke).toHaveBeenNthCalledWith(2, 'mirror_start');
    expect(host.textContent).not.toContain('--adb-path');
  });

  test('初始加载读取 mirror_status 并展示已有投屏状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      is_mirroring: true,
      auto_mirror_enabled: true,
    });

    renderMirror();

    await waitUntil(() => (host.textContent ?? '').includes('投屏运行中'));
    expect(invoke).toHaveBeenCalledWith('mirror_status');
    const toggle = host.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(toggle.checked).toBe(true);
  });
});

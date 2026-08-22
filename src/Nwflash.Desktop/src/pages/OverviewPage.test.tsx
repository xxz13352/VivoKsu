import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { OverviewPage } from './OverviewPage';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';

type Root = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: Root;
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

const adbSnapshot = {
  connection_state: 'AdbConnected',
  serial: 'RF8T123',
  connection_label: 'ADB 已连接',
  model: 'V2318A',
  android_version: '15',
  battery_level: '78%',
};

const renderOverview = () => {
  flushSync(() => {
    root.render(<OverviewPage />);
  });
};

describe('OverviewPage', () => {
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

  test('加载成功后展示设备连接快照', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(adbSnapshot);

    renderOverview();

    await waitUntil(() => (host.textContent ?? '').includes('RF8T123'));

    expect(invoke).toHaveBeenCalledWith('device_refresh');
    expect(host.textContent).toContain('ADB 已连接');
    expect(host.textContent).toContain('V2318A');
    expect(host.textContent).toContain('78%');
    expect(host.querySelector('.nw-overview-identity .nw-page-eyebrow')?.textContent).toBe(
      '已连接设备',
    );
    expect(host.querySelectorAll('.nw-device-indicator.is-connected')).toHaveLength(2);
  });

  test('使用 WPF 设备档案和启动控制结构呈现断开连接的空闲状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      connection_state: 'Disconnected',
      serial: '--',
      connection_label: '等待连接',
      model: '未检测到设备',
      android_version: '--',
      battery_level: '--',
    });

    renderOverview();
    await waitUntil(() => (host.textContent ?? '').includes('READ-ONLY DEVICE PROFILE'));

    expect(host.querySelector('.nw-overview-page')).not.toBeNull();
    expect(host.querySelector('.nw-overview-device-profile')).not.toBeNull();
    expect(host.querySelectorAll('.nw-overview-detail').length).toBe(6);
    expect(host.textContent).toContain('DEVICE / OVERVIEW');
    expect(host.textContent).toContain('连接信息、引导状态与系统标识');
    expect(host.textContent).toContain('设备参数由 ADB / Fastboot 会话实时读取');
    expect(host.textContent).toContain('REBOOT CONTROL');
    expect(host.querySelector('.nw-overview-identity .nw-page-eyebrow')?.textContent).toBe(
      '未检测到设备',
    );
    expect(host.querySelector('.nw-device-indicator.is-connected')).toBeNull();
    expect(host.querySelector('[aria-label="重启设备"]')?.textContent).toBe('重启');
    expect(host.querySelector('[aria-label="进入 Bootloader"]')?.textContent).toBe('进入');
    expect(host.querySelector('[aria-label="进入 Fastboot"]')?.textContent).toBe('进入');
  });

  test('ADB 已连接时重启系统不会传递用户控制的序列号', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(adbSnapshot)
      .mockResolvedValueOnce(undefined);

    renderOverview();
    await waitUntil(() => (host.textContent ?? '').includes('RF8T123'));

    (host.querySelector('.nw-test-reboot-system') as HTMLButtonElement).click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);
    expect(invoke).toHaveBeenLastCalledWith('device_reboot_system');
  });

  test('Fastboot 已连接时仍允许启动控制并调用受限重启命令', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({
        connection_state: 'FastbootConnected',
        serial: 'FB8T123',
        connection_label: 'Fastboot 已连接',
        model: 'V2318A',
        android_version: '--',
        battery_level: '--',
      })
      .mockResolvedValueOnce(undefined);

    renderOverview();
    await waitUntil(() => (host.textContent ?? '').includes('FB8T123'));

    const button = host.querySelector('[aria-label="进入 Bootloader"]') as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    expect(host.querySelectorAll('.nw-device-indicator.is-connected')).toHaveLength(2);
    button.click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);
    expect(invoke).toHaveBeenLastCalledWith('device_reboot_bootloader');
  });

  test('命令异常时展示设备检测错误', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('adb unavailable'));

    renderOverview();

    await waitUntil(() => host.querySelector('.nw-error-text') !== null);
    expect(host.querySelector('.nw-error-text')?.textContent).toBe('adb unavailable');
  });

  test('有效设备快照到达后清除过期的设备检测错误', async () => {
    let rejectRefresh: ((error: Error) => void) | undefined;
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      () => new Promise<never>((_resolve, reject) => {
        rejectRefresh = reject;
      }),
    );

    renderOverview();
    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length > 0);

    flushSync(() => {
      root.render(<OverviewPage snapshot={adbSnapshot} />);
    });
    await waitUntil(() => (host.textContent ?? '').includes('RF8T123'));

    rejectRefresh?.(new Error('adb unavailable'));
    await flushPromises();

    expect(host.textContent).toContain('ADB 已连接');
    expect(host.querySelector('.nw-error-text')).toBeNull();
  });
});

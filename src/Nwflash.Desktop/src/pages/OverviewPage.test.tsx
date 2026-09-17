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
    await waitUntil(() => (host.textContent ?? '').includes('启动控制'));

    expect(host.querySelector('.nw-overview-page')).not.toBeNull();
    expect(host.querySelector('.nw-overview-device-profile')).not.toBeNull();
    expect(host.querySelectorAll('.nw-overview-detail').length).toBe(6);
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

  test('Fastboot 连接时展示后端读到的槽位/引导加载器/内核/验证启动', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      connection_state: 'FastbootConnected',
      serial: 'FB8T123',
      connection_label: 'Fastboot 已连接（Bootloader 已解锁）',
      model: 'PD2307',
      android_version: '--',
      battery_level: '--',
      active_slot: 'b',
      bootloader_state: '已解锁',
      kernel_version: '--',
      verified_boot_state: '--',
    });

    renderOverview();
    await waitUntil(() => (host.textContent ?? '').includes('FB8T123'));

    expect(host.querySelector('.nw-overview-detail-slot dd')?.textContent).toBe('b');
    expect(host.querySelector('.nw-overview-detail-bootloader dd')?.textContent).toBe('已解锁');
    // fastboot 下系统版本确实读不到：必须显示 --，不能拿槽位冒充。
    expect(host.querySelector('.nw-overview-detail-system dd')?.textContent).toBe('--');
    expect(host.querySelector('.nw-overview-detail-battery dd')?.textContent).toBe('--');
  });

  test('ADB 连接时展示槽位/内核版本/验证启动', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      ...adbSnapshot,
      active_slot: 'a',
      bootloader_state: '已锁定',
      kernel_version: '6.1.75-android14',
      verified_boot_state: '已校验',
    });

    renderOverview();
    await waitUntil(() => (host.textContent ?? '').includes('RF8T123'));

    expect(host.querySelector('.nw-overview-detail-slot dd')?.textContent).toBe('a');
    expect(host.querySelector('.nw-overview-detail-bootloader dd')?.textContent).toBe('已锁定');
    expect(host.querySelector('.nw-overview-detail-kernel dd')?.textContent).toBe(
      '6.1.75-android14',
    );
    expect(host.querySelector('.nw-overview-detail-verified dd')?.textContent).toBe('已校验');
  });

  test('任务进行中的刷新失败不渲染错误，也不丢掉已连接设备', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValue(
      new Error('设备刷新已跳过（denied:flashing）。'),
    );

    flushSync(() => {
      root.render(<OverviewPage snapshot={adbSnapshot} busy />);
    });
    await flushPromises();

    expect(host.querySelector('.nw-error-text')).toBeNull();
    expect(host.textContent).toContain('RF8T123');
    expect(host.querySelector('.nw-overview-paused')?.textContent).toContain('任务进行中');
  });

  test('发现失败的快照渲染成「设备检测失败」，不冒充「未检测到设备」', async () => {
    const errorSnapshot = {
      connection_state: 'Error',
      serial: '--',
      connection_label: '设备检测失败',
      model: '未检测到设备',
      android_version: '--',
      battery_level: '--',
      active_slot: '--',
      bootloader_state: '--',
      kernel_version: '--',
      verified_boot_state: '--',
    };
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(errorSnapshot);

    flushSync(() => {
      root.render(<OverviewPage snapshot={errorSnapshot} />);
    });
    await flushPromises();

    const eyebrow = host.querySelector('.nw-page-eyebrow')?.textContent;
    expect(eyebrow).toBe('设备检测失败');
    expect(host.textContent).not.toContain('未检测到设备');
    // 身份行不能显示「未检测到设备」这种结论性文案，也不能显示空白单元格。
    expect(host.querySelector('.nw-overview-identity strong')?.textContent).toBe('--');
  });

  test('本页读到的权威快照会交回外壳', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(adbSnapshot);
    const onSnapshot = vi.fn();

    flushSync(() => {
      root.render(<OverviewPage onSnapshot={onSnapshot} />);
    });
    await waitUntil(() => onSnapshot.mock.calls.length > 0);

    expect(onSnapshot).toHaveBeenCalledWith(expect.objectContaining({ serial: 'RF8T123' }));
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

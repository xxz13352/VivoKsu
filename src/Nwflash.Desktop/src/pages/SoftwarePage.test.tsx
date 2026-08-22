import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { SoftwarePage } from './SoftwarePage';

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

const renderSoftware = () => {
  flushSync(() => {
    root.render(<SoftwarePage />);
  });
};

describe('SoftwarePage', () => {
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

  test('成功加载版本与日志信息', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      latest: '3.0.0',
      min_version: '2.0.0',
      download_url: 'https://example.com/nwflash.msi',
      update_required: false,
      force_update: false,
    });
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce([
      {
        timestamp_utc: 1760000000,
        level: 'Success',
        message: '更新完成',
        operation_id: null,
      },
    ]);

    renderSoftware();
    await waitUntil(() => {
      const version = host.querySelector('.nw-software-version') as HTMLDivElement | null;
      return version !== null && version.textContent?.includes('3.0.0');
    });

    expect(invoke).toHaveBeenCalledWith('version_check');
    expect(invoke).toHaveBeenCalledWith('operation_logs_snapshot');
    const logItem = host.querySelector('.nw-test-software-log-item') as HTMLLIElement;
    expect(logItem).not.toBeNull();
    expect(logItem.textContent ?? '').toContain('更新完成');
  });

  test('展示 Rust 返回的组件和驱动就绪状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({
        latest: '3.0.0',
        min_version: '2.0.0',
        download_url: null,
        update_required: false,
        force_update: false,
      })
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce({
        adb_ready: true,
        fastboot_ready: true,
        scrcpy_ready: false,
        payload_dumper_ready: true,
        adb_driver_installed: true,
        fastboot_driver_installed: false,
        mediatek_driver_installed: true,
      });

    renderSoftware();

    await waitUntil(() => (host.textContent ?? '').includes('ADB（WinUSB）'));

    expect(invoke).toHaveBeenCalledWith('software_status');
    expect(host.textContent).toContain('ADB（WinUSB）');
    expect(host.textContent).toContain('Fastboot（fastbootd 刷写）');
    expect(host.textContent).toContain('MediaTek（联发科 / BROM 救砖）');
    expect(host.textContent).toContain('未检测到 scrcpy.exe');
    expect(host.textContent).not.toContain('ADB 工具：');
  });

  test('按 WPF 软件页分组渲染组件状态而不展示迁移期 API 说明', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({
        latest: '3.0.0',
        min_version: '2.0.0',
        download_url: null,
        update_required: false,
        force_update: false,
      })
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce({
        adb_ready: true,
        fastboot_ready: true,
        app_version: '1.0.1',
        scrcpy_ready: true,
        payload_dumper_ready: true,
        adb_driver_installed: true,
        fastboot_driver_installed: true,
        mediatek_driver_installed: true,
      });

    renderSoftware();
    await waitUntil(() => host.querySelector('[aria-label="组件状态"]') !== null);

    const components = host.querySelector('[aria-label="组件状态"]') as HTMLElement;
    expect(components.textContent).toContain('奶蛙Flash 客户端');
    expect(components.textContent).toContain('版本 v1.0.1');
    expect(components.textContent).toContain('手机 USB 驱动');
    expect(components.textContent).toContain('任缺一类启动即提醒');
    expect(components.textContent).toContain('scrcpy 投屏工具');
    expect(components.textContent).toContain('投屏所需 scrcpy.exe（发布内置）');
    expect(components.textContent).toContain('scrcpy 已就绪');
    expect(components.textContent).toContain('payload_dumper 解包工具');
    expect(host.textContent).not.toContain('从 version_check 与操作日志构建软件页真实状态面板。');
    expect(host.textContent).toContain('需要帮助?');
    expect(host.textContent).toContain('驱动未安装时,启动会自动弹出驱动提醒');
  });

  test('刷新按钮再次获取数据', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      latest: '2.1.0',
      min_version: '1.5.0',
      download_url: null,
      update_required: true,
      force_update: true,
    });
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    renderSoftware();
    await waitUntil(() => host.querySelector('.nw-software-version') !== null);

    const button = host.querySelector('.nw-test-software-refresh') as HTMLButtonElement;
    button.click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length >= 4);
    expect((invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBeGreaterThanOrEqual(4);
  });

  test('非法返回结构回退空日志与未知版本', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce('bad version check');
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce('bad logs');

    renderSoftware();

    await waitUntil(() => {
      const version = host.querySelector('.nw-software-version') as HTMLDivElement | null;
      return version !== null && version.textContent?.includes('未知');
    });

    const version = host.querySelector('.nw-software-version') as HTMLDivElement;
    expect(version).not.toBeNull();
    expect(version.textContent ?? '').toContain('未知');
    expect(host.querySelector('.nw-empty-log')).not.toBeNull();
    expect(invoke).toHaveBeenCalledWith('version_check');
    expect(invoke).toHaveBeenCalledWith('operation_logs_snapshot');
  });

  test('异常返回展示错误', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('software rpc failed'));

    renderSoftware();

    await waitUntil(() => host.querySelector('.nw-error-text') !== null);
    const err = host.querySelector('.nw-error-text') as HTMLParagraphElement;
    expect(err).not.toBeNull();
    expect(err.textContent).toBe('software rpc failed');
  });

  test('版本或日志诊断失败时仍展示独立的软件组件状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation((command: string) => {
      if (command === 'version_check') return Promise.reject(new Error('version service unavailable'));
      if (command === 'operation_logs_snapshot') return Promise.reject(new Error('log service unavailable'));
      if (command === 'software_status') {
        return Promise.resolve({
          app_version: '1.0.1',
          adb_ready: true,
          fastboot_ready: true,
          scrcpy_ready: true,
          payload_dumper_ready: true,
          adb_driver_installed: true,
          fastboot_driver_installed: true,
          mediatek_driver_installed: true,
        });
      }
      return Promise.resolve(null);
    });

    renderSoftware();

    await waitUntil(() => host.querySelector('[aria-label="组件状态"]') !== null);
    expect(host.textContent).toContain('版本 v1.0.1');
    expect(host.querySelector('.nw-error-text')).toBeNull();
  });

  test('重新安装驱动后刷新组件状态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation((command: string) => {
      if (command === 'version_check') {
        return Promise.resolve({
          latest: '3.0.0',
          min_version: '2.0.0',
          download_url: null,
          update_required: false,
          force_update: false,
        });
      }
      if (command === 'operation_logs_snapshot') return Promise.resolve([]);
      if (command === 'software_status') {
        return Promise.resolve({
          adb_ready: true,
          fastboot_ready: true,
          scrcpy_ready: true,
          payload_dumper_ready: true,
          adb_driver_installed: false,
          fastboot_driver_installed: false,
          mediatek_driver_installed: false,
        });
      }
      if (command === 'driver_reinstall') return Promise.resolve({ exit_code: 0 });
      return Promise.resolve(null);
    });

    renderSoftware();
    await waitUntil(() => host.querySelector('.nw-test-driver-reinstall-open') !== null);

    (host.querySelector('.nw-test-driver-reinstall-open') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-driver-reinstall-confirm') !== null);
    (host.querySelector('.nw-test-driver-reinstall-confirm') as HTMLButtonElement).click();

    await waitUntil(() =>
      (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.some(
        ([command]) => command === 'driver_reinstall',
      ),
    );
    await waitUntil(() => host.querySelector('.nw-test-driver-reinstall-confirm') === null);
    expect(invoke).toHaveBeenCalledWith('driver_reinstall');
  });

  test('组件安装完成后关闭模态并刷新软件状态', async () => {
    const inventory = [
      { key: 'scrcpy', display_name: 'scrcpy 投屏', is_ready: false, default_selected: true },
      { key: 'payload', display_name: 'payload_dumper', is_ready: true, default_selected: false },
    ];
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation((command: string) => {
      if (command === 'version_check') {
        return Promise.resolve({
          latest: '3.0.0',
          min_version: '2.0.0',
          download_url: null,
          update_required: false,
          force_update: false,
        });
      }
      if (command === 'operation_logs_snapshot') return Promise.resolve([]);
      if (command === 'software_status') {
        return Promise.resolve({
          adb_ready: true,
          fastboot_ready: true,
          scrcpy_ready: false,
          payload_dumper_ready: true,
          adb_driver_installed: true,
          fastboot_driver_installed: true,
          mediatek_driver_installed: true,
        });
      }
      if (command === 'resource_inventory') return Promise.resolve(inventory);
      if (command === 'resource_install') return Promise.resolve(['scrcpy']);
      return Promise.resolve(null);
    });

    renderSoftware();
    await waitUntil(() => {
      const button = host.querySelector('.nw-test-resource-install-open') as HTMLButtonElement | null;
      return button !== null && !button.disabled;
    });
    (host.querySelector('.nw-test-resource-install-open') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('.nw-test-resource-install') !== null);
    (host.querySelector('.nw-test-resource-install') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('.nw-test-resource-install') === null);
    expect(invoke).toHaveBeenCalledWith('resource_install', { keys: ['scrcpy'] });
    expect(
      (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.filter(
        ([command]) => command === 'software_status',
      ).length,
    ).toBeGreaterThanOrEqual(2);
  });
});

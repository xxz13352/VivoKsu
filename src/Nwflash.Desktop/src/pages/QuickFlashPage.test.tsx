import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { QuickFlashPage } from './QuickFlashPage';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

type RootHandle = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: RootHandle;
const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const start = Date.now();
  while (!predicate() && Date.now() - start < timeoutMs) await flushPromises();
  if (!predicate()) throw new Error('timeout waiting for async assertion');
};

describe('QuickFlashPage', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(() => {
    flushSync(() => root.unmount());
    host.remove();
  });

  test('选择镜像后仅调用受限检查命令并显示安全元数据', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\boot.img');
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      path: 'C:\\chosen\\boot.img',
      size_bytes: 8192,
    });
    flushSync(() => root.render(<QuickFlashPage />));

    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('8.0 KB') ?? false);

    expect(invoke).toHaveBeenCalledWith('quick_flash_inspect_image', {
      imagePath: 'C:\\chosen\\boot.img',
    });
    expect(host.textContent).toContain('镜像已就绪');
    expect(host.textContent).not.toContain('C:\\chosen\\boot.img');
    expect(host.querySelector('.nw-command-output')).toBeNull();
  });

  test('每个预设独立保留镜像，切换预设不复用另一个槽位', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\boot.img');
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      path: 'C:\\chosen\\boot.img',
      size_bytes: 8192,
    });
    flushSync(() => root.render(<QuickFlashPage />));

    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('8.0 KB') ?? false);
    const initBootButton = host.querySelector('.nw-test-quick-flash-preset-InitBoot') as HTMLButtonElement;
    initBootButton.click();
    await waitUntil(() => initBootButton.getAttribute('aria-pressed') === 'true');

    const rows = host.querySelectorAll('.nw-quick-flash-preset-row');
    expect(rows[0]?.textContent).toContain('镜像已就绪');
    expect(rows[1]?.textContent).toContain('未选择镜像');
  });

  test('批量确认只执行已选择的预设镜像', async () => {
    (open as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce('C:\\chosen\\boot.img')
      .mockResolvedValueOnce('C:\\chosen\\init_boot.img');
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ path: 'C:\\chosen\\boot.img', size_bytes: 8192 })
      .mockResolvedValueOnce({ path: 'C:\\chosen\\init_boot.img', size_bytes: 16384 })
      .mockResolvedValueOnce({ command_count: 3, executed_count: 3 });
    flushSync(() => root.render(<QuickFlashPage />));

    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelectorAll('.nw-quick-flash-preset-row')[0]?.textContent?.includes('镜像已就绪') ?? false);
    const initBootButton = host.querySelector('.nw-test-quick-flash-preset-InitBoot') as HTMLButtonElement;
    initBootButton.click();
    await waitUntil(() => initBootButton.getAttribute('aria-pressed') === 'true');
    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelectorAll('.nw-quick-flash-preset-row')[1]?.textContent?.includes('镜像已就绪') ?? false);

    const batchButton = host.querySelector('.nw-test-quick-flash-execute-boot') as HTMLButtonElement;
    expect(batchButton.disabled).toBe(false);
    batchButton.click();
    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    expect(host.textContent).toContain('将刷写 2 个分区');

    (host.querySelector('.nw-test-quick-flash-confirm-boot') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已完成刷写 2 个预设分区') ?? false);
    expect(invoke).toHaveBeenLastCalledWith('quick_flash_execute_preset_images', {
      requests: [
        { imagePath: 'C:\\chosen\\boot.img', partition: 'Boot' },
        { imagePath: 'C:\\chosen\\init_boot.img', partition: 'InitBoot' },
      ],
      autoReboot: true,
      waitForDevice: true,
      flashBothSlots: false,
      switchSlotAfterFlash: false,
    });
  });

  test('使用 WPF 预置刷写面板呈现四个空镜像槽', () => {
    flushSync(() => root.render(<QuickFlashPage />));

    expect(host.querySelector('.nw-quick-flash-page')).not.toBeNull();
    expect(host.querySelector('.nw-quick-flash-preset-panel')).not.toBeNull();
    expect(host.querySelectorAll('.nw-quick-flash-preset-row').length).toBe(4);
    expect(host.textContent).toContain('FLASH / PRESET');
    expect(host.textContent).toContain('fastbootd 支持分区写入，完成后自动重启设备');
    expect(host.textContent).toContain('开始刷入');
    expect(host.textContent).toContain('自动重启');
    expect(host.textContent).toContain('等待 FB 设备');
    expect(host.textContent).toContain('未选择镜像');
  });

  test('取消镜像选择时不调用 Rust 命令', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(null);
    flushSync(() => root.render(<QuickFlashPage />));

    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await flushPromises();

    expect(invoke).not.toHaveBeenCalled();
  });

  test('单预设确认后才执行并提交用户选择的重启和等待选项', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\boot.img');
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ path: 'C:\\chosen\\boot.img', size_bytes: 8192 })
      .mockResolvedValueOnce({ command_count: 1, executed_count: 1 });
    flushSync(() => root.render(<QuickFlashPage />));

    const checkboxes = host.querySelectorAll('input[type="checkbox"]');
    (checkboxes[0] as HTMLInputElement).click();
    (checkboxes[1] as HTMLInputElement).click();
    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('镜像已就绪') ?? false);
    (host.querySelector('.nw-test-quick-flash-prepare-boot') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    expect(invoke).toHaveBeenCalledTimes(1);
    (host.querySelector('.nw-test-quick-flash-confirm-boot') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已完成刷写 1 个预设分区') ?? false);
    expect(invoke).toHaveBeenLastCalledWith('quick_flash_execute_preset_images', {
      requests: [{ imagePath: 'C:\\chosen\\boot.img', partition: 'Boot' }],
      autoReboot: false,
      waitForDevice: false,
      flashBothSlots: false,
      switchSlotAfterFlash: false,
    });
    expect(host.textContent).not.toContain('C:\\chosen\\boot.img');
  });

  test('init_boot 单预设将双槽和切槽选项传入受限批量入口', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\init_boot.img');
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ path: 'C:\\chosen\\init_boot.img', size_bytes: 8192 })
      .mockResolvedValueOnce({ command_count: 4, executed_count: 4 });
    flushSync(() => root.render(<QuickFlashPage />));

    (host.querySelector('.nw-test-quick-flash-dual-slot') as HTMLInputElement).click();
    (host.querySelector('.nw-test-quick-flash-switch-slot') as HTMLInputElement).click();
    const initBootButton = host.querySelector('.nw-test-quick-flash-preset-InitBoot') as HTMLButtonElement;
    initBootButton.click();
    await waitUntil(() => initBootButton.getAttribute('aria-pressed') === 'true');
    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelectorAll('.nw-quick-flash-preset-row')[1]?.textContent?.includes('镜像已就绪') ?? false);
    (host.querySelector('.nw-test-quick-flash-prepare-boot') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    expect(host.textContent).toContain('双槽刷入');
    expect(host.textContent).toContain('刷完切换槽位');
    (host.querySelector('.nw-test-quick-flash-confirm-boot') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已完成刷写 1 个预设分区') ?? false);
    expect(invoke).toHaveBeenLastCalledWith('quick_flash_execute_preset_images', {
      requests: [{ imagePath: 'C:\\chosen\\init_boot.img', partition: 'InitBoot' }],
      autoReboot: true,
      waitForDevice: true,
      flashBothSlots: true,
      switchSlotAfterFlash: true,
    });
  });

  test('刷写执行中提供取消当前操作并调用统一取消命令', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\boot.img');
    let resolveExecution: (() => void) | undefined;
    command.mockResolvedValueOnce({ path: 'C:\\chosen\\boot.img', size_bytes: 8192 });
    command.mockImplementationOnce(() => new Promise<void>((resolve) => {
      resolveExecution = resolve;
    }));
    command.mockResolvedValueOnce(undefined);
    flushSync(() => root.render(<QuickFlashPage />));

    (host.querySelector('.nw-test-quick-flash-select-image') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('镜像已就绪') ?? false);
    (host.querySelector('.nw-test-quick-flash-prepare-boot') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-quick-flash-confirm-boot') !== null);
    (host.querySelector('.nw-test-quick-flash-confirm-boot') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-quick-flash-cancel') !== null);

    (host.querySelector('.nw-test-quick-flash-cancel') as HTMLButtonElement).click();
    await waitUntil(() => command.mock.calls.some(([name]) => name === 'operation_cancel'));
    expect(command).toHaveBeenCalledWith('operation_cancel');
    resolveExecution?.();
  });

});

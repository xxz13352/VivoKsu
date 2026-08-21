import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { SafeFlashPage } from './SafeFlashPage';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { invoke } from '@tauri-apps/api/core';

let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
const wait = () => new Promise((resolve) => setTimeout(resolve, 0));

describe('SafeFlashPage', () => {
  beforeEach(() => { host = document.createElement('div'); document.body.appendChild(host); root = createRoot(host); });
  afterEach(() => { flushSync(() => root.unmount()); host.remove(); vi.clearAllMocks(); });

  test('以 WPF VIVO LINE FLASH 工作台呈现设备摘要、刷写选项和状态栏', () => {
    flushSync(() => root.render(<SafeFlashPage />));

    expect(host.querySelector('.nw-safe-flash-workspace')).not.toBeNull();
    expect(host.querySelector('.nw-safe-flash-heading .nw-page-eyebrow')?.textContent).toBe('VIVO LINE FLASH');
    expect(host.querySelector('.nw-safe-flash-device-summary')?.textContent).toContain('目标设备');
    expect(host.querySelector('.nw-safe-flash-options')?.textContent).toContain('刷写选项');
    expect(host.querySelector('.nw-safe-flash-statusbar')).not.toBeNull();
    expect(host.textContent).toContain('查询设备');
    expect(host.textContent).toContain('下载 OTA');
  });

  test('在线预检只提交选项，ROM 标识由 Rust 从已连接设备读取，且不渲染敏感执行数据', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue({ session_id: 'safe-1', source_label: '在线 OTA', partition_count: 3, safe_partition_count: 2, requires_confirmation: true });
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();
    expect(invoke).toHaveBeenCalledWith('safe_flash_prepare_online', { options: { is_safe_flash: true, is_keep_root: false, wipe_data: false, slot_mode: 'CurrentSlot' } });
    expect(host.textContent).not.toContain('https://');
    expect(host.textContent).not.toContain('fastboot.exe');
    expect(host.querySelector('[name="serial"], [name="imagePath"], [name="wipeDataImagePath"], [name="localSourcePath"]')).toBeNull();
    expect(host.querySelector('[name="pd"], [name="version"]')).toBeNull();
  });

  test('预检准备期间锁定所有来源与刷写选项', async () => {
    const invokeMock = invoke as ReturnType<typeof vi.fn>;
    let resolvePreparation: ((value: unknown) => void) | undefined;
    invokeMock.mockImplementationOnce(() => new Promise((resolve) => { resolvePreparation = resolve; }));
    flushSync(() => root.render(<SafeFlashPage />));

    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();

    expect((host.querySelector('.nw-test-safe-source-mode') as HTMLSelectElement).disabled).toBe(true);
    expect((host.querySelector('input[type="checkbox"]') as HTMLInputElement).disabled).toBe(true);
    expect((host.querySelector('select:not(.nw-test-safe-source-mode)') as HTMLSelectElement).disabled).toBe(true);
    resolvePreparation?.({ session_id: 'safe-preparing', source_label: '在线 OTA', partition_count: 1, safe_partition_count: 1, requires_confirmation: true });
  });

  test('预检准备中可通过统一操作取消命令停止', async () => {
    const invokeMock = invoke as ReturnType<typeof vi.fn>;
    let resolvePreparation: ((value: unknown) => void) | undefined;
    try {
      invokeMock.mockImplementationOnce(() => new Promise((resolve) => {
        resolvePreparation = resolve;
      }));
      invokeMock.mockResolvedValueOnce(undefined);
      flushSync(() => root.render(<SafeFlashPage />));

      (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
      await wait();
      const stopButton = host.querySelector('[aria-label="停止操作"]') as HTMLButtonElement | null;
      expect(stopButton).not.toBeNull();
      stopButton?.click();
      await wait();

      expect(invoke).toHaveBeenLastCalledWith('operation_cancel');
      resolvePreparation?.({ session_id: 'safe-cancelled', source_label: '在线 OTA', partition_count: 1, safe_partition_count: 1, requires_confirmation: true });
    } finally {
      invokeMock.mockReset();
    }
  });

  test('执行只消费 Rust 维护的不透明预检 ID', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValueOnce({ session_id: 'safe-1', source_label: '在线 OTA', partition_count: 2, safe_partition_count: 2, requires_confirmation: true }).mockResolvedValueOnce({ flashed_partition_count: 2, skipped_partition_count: 0, status: '已刷入 2 个分区' });
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();
    Array.from(host.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'))
      .find((button) => button.textContent === '确认刷写')?.click();
    await wait();
    expect(invoke).toHaveBeenLastCalledWith('safe_flash_execute_prepared', { sessionId: 'safe-1' });
    expect(host.textContent).toContain('已刷入 2 个分区');
  });

  test('预检确认期间锁定选项，并通过 Rust 取消 command 丢弃会话', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValueOnce({ session_id: 'safe-1', source_label: '在线 OTA', partition_count: 2, safe_partition_count: 2, requires_confirmation: true }).mockResolvedValueOnce(undefined);
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();
    expect((host.querySelector('.nw-test-safe-source-mode') as HTMLSelectElement).disabled).toBe(true);
    expect((host.querySelector('input[type="checkbox"]') as HTMLInputElement).disabled).toBe(true);
    const cancelButton = Array.from(host.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find((button) => button.textContent === '取消');
    cancelButton?.click();
    await wait();
    expect(invoke).toHaveBeenLastCalledWith('safe_flash_cancel_prepared', { sessionId: 'safe-1' });
    expect(host.querySelector('[role="dialog"]')).toBeNull();
  });

  test('块式 OTA 预检在确认窗提示仅刷写直接镜像分区', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue({ session_id: 'safe-block', source_label: '在线 OTA', partition_count: 2, safe_partition_count: 2, has_block_based_content: true, requires_confirmation: true });
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();
    expect(host.querySelector('[role="dialog"]')?.textContent).toContain('仅刷写可直接镜像分区');
  });

  test('本地预检不从前端获取或提交文件路径', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue({ session_id: 'safe-local', source_label: '本地固件', partition_count: 2, safe_partition_count: 2, requires_confirmation: true });
    flushSync(() => root.render(<SafeFlashPage />));

    const sourceMode = host.querySelector('.nw-test-safe-source-mode') as HTMLSelectElement;
    sourceMode.value = 'Local';
    sourceMode.dispatchEvent(new Event('change', { bubbles: true }));
    await wait();
    (host.querySelector('button[type="button"]') as HTMLButtonElement).click();
    await wait();

    expect(invoke).toHaveBeenCalledWith('safe_flash_prepare_local_source', {
      options: { is_safe_flash: true, is_keep_root: false, wipe_data: false, slot_mode: 'CurrentSlot' },
    });
  });

  test('已解包目录通过 Rust 文件夹对话框进入预检，前端不提交路径', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue({ session_id: 'safe-directory', source_label: '本地固件', partition_count: 2, safe_partition_count: 2, requires_confirmation: true });
    flushSync(() => root.render(<SafeFlashPage />));

    const sourceMode = host.querySelector('.nw-test-safe-source-mode') as HTMLSelectElement;
    sourceMode.value = 'Local';
    sourceMode.dispatchEvent(new Event('change', { bubbles: true }));
    await wait();
    const directoryButton = Array.from(host.querySelectorAll('button')).find((button) => button.textContent === '选择解包文件夹');
    expect(directoryButton).toBeDefined();
    directoryButton?.click();
    await wait();

    expect(invoke).toHaveBeenCalledWith('safe_flash_prepare_local_directory', {
      options: { is_safe_flash: true, is_keep_root: false, wipe_data: false, slot_mode: 'CurrentSlot' },
    });
  });
});

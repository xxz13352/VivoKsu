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
    expect(host.querySelector('.nw-safe-flash-device-summary')?.textContent).toContain('目标设备');
    expect(host.querySelector('.nw-safe-flash-options')?.textContent).toContain('刷写选项');
    expect(host.querySelector('.nw-safe-flash-statusbar')).not.toBeNull();
    expect(host.textContent).toContain('VIVO 线刷');
  });

  test('在线预检只提交选项，ROM 标识由 Rust 从已连接设备读取，且不渲染敏感执行数据', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue({ session_id: 'safe-1', source_label: '在线固件', partition_count: 3, safe_partition_count: 2, requires_confirmation: true });
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
    resolvePreparation?.({ session_id: 'safe-preparing', source_label: '在线固件', partition_count: 1, safe_partition_count: 1, requires_confirmation: true });
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
      resolvePreparation?.({ session_id: 'safe-cancelled', source_label: '在线固件', partition_count: 1, safe_partition_count: 1, requires_confirmation: true });
    } finally {
      invokeMock.mockReset();
    }
  });

  test('执行只消费 Rust 维护的不透明预检 ID', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValueOnce({ session_id: 'safe-1', source_label: '在线固件', partition_count: 2, safe_partition_count: 2, requires_confirmation: true }).mockResolvedValueOnce({ flashed_partition_count: 2, skipped_partition_count: 0, status: '已刷入 2 个分区' });
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();
    Array.from(host.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'))
      .find((button) => button.textContent === '确认刷写')?.click();
    await wait();
    expect(invoke).toHaveBeenLastCalledWith('safe_flash_execute_prepared', { sessionId: 'safe-1' });
    expect(host.textContent).toContain('已刷入 2 个分区');
  });

  test('点击确认刷写后弹窗立即关闭，刷写仍在进行', async () => {
    const invokeMock = invoke as ReturnType<typeof vi.fn>;
    invokeMock.mockResolvedValueOnce({ session_id: 'safe-1', source_label: '在线固件', partition_count: 2, safe_partition_count: 2, requires_confirmation: true });
    let resolveExecution: (() => void) | undefined;
    invokeMock.mockImplementationOnce(() => new Promise<void>((resolve) => { resolveExecution = resolve; }));
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();

    Array.from(host.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'))
      .find((button) => button.textContent === '确认刷写')?.click();
    await wait();

    // 确认后弹窗必须立即消失：执行期间弹窗停留会遮住进度面板，且分区失败
    // 决策弹窗会叠在它上面（用户可见「弹两次」），执行中这层还永远点不掉。
    expect(host.querySelector('[role="dialog"]')).toBeNull();
    expect(invoke).toHaveBeenLastCalledWith('safe_flash_execute_prepared', { sessionId: 'safe-1' });
    resolveExecution?.();
    await wait();
  });

  test('执行失败后恢复确认弹窗，可直接重试或取消，不会卡死', async () => {
    const invokeMock = invoke as ReturnType<typeof vi.fn>;
    invokeMock.mockResolvedValueOnce({ session_id: 'safe-1', source_label: '在线固件', partition_count: 2, safe_partition_count: 2, requires_confirmation: true })
      .mockRejectedValueOnce(new Error('设备不可用，请检查连接后重试。'))
      .mockResolvedValueOnce(undefined);
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();
    Array.from(host.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'))
      .find((button) => button.textContent === '确认刷写')?.click();
    await wait();

    // 后端在执行失败时保留已预检会话（staging 在盘），恢复弹窗允许直接重试。
    expect(host.querySelector('[role="dialog"]')).not.toBeNull();
    expect(host.textContent).toContain('设备不可用');

    Array.from(host.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'))
      .find((button) => button.textContent === '取消')?.click();
    await wait();
    expect(invoke).toHaveBeenLastCalledWith('safe_flash_cancel_prepared', { sessionId: 'safe-1' });
    expect(host.querySelector('[role="dialog"]')).toBeNull();
  });

  test('取消命令被后端拒绝时弹窗仍立即收起，不再卡死', async () => {
    const invokeMock = invoke as ReturnType<typeof vi.fn>;
    invokeMock.mockResolvedValueOnce({ session_id: 'safe-1', source_label: '在线固件', partition_count: 2, safe_partition_count: 2, requires_confirmation: true })
      .mockRejectedValueOnce(new Error('当前会话已失效，请重新完成线刷预检。'));
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();

    Array.from(host.querySelectorAll<HTMLButtonElement>('[role="dialog"] button'))
      .find((button) => button.textContent === '取消')?.click();
    await wait();

    // 乐观收起：会话已被作废时取消命令必然失败，但弹窗必须已经关闭，
    // 失败原因只作为错误提示展示——否则取消/确认/关闭三条路全死，永久卡死。
    expect(invoke).toHaveBeenLastCalledWith('safe_flash_cancel_prepared', { sessionId: 'safe-1' });
    expect(host.querySelector('[role="dialog"]')).toBeNull();
    expect(host.textContent).toContain('当前会话已失效');
  });

  test('预检确认期间锁定选项，并通过 Rust 取消 command 丢弃会话', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValueOnce({ session_id: 'safe-1', source_label: '在线固件', partition_count: 2, safe_partition_count: 2, requires_confirmation: true }).mockResolvedValueOnce(undefined);
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

  test('块式固件预检在确认窗提示部分分区保持原样', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue({ session_id: 'safe-block', source_label: '在线固件', partition_count: 2, safe_partition_count: 2, has_block_based_content: true, requires_confirmation: true });
    flushSync(() => root.render(<SafeFlashPage />));
    (host.querySelector('.nw-test-safe-flash-form') as HTMLFormElement).requestSubmit();
    await wait();
    expect(host.querySelector('[role="dialog"]')?.textContent).toContain('这些分区将保持原样');
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

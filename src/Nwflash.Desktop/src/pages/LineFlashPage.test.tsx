import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { LineFlashPage } from './LineFlashPage';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

type RootHandle = ReturnType<typeof createRoot>;

const fastbootSnapshot = {
  serial: 'FAST-1',
  transport: 'Fastboot',
  active_slot: 'a',
  partitions: [
    { name: 'boot_a', device_path: 'boot_a', size_bytes: 67108864, slot: 'a', is_mounted: false, is_high_risk: false, can_backup: true },
    { name: 'super', device_path: 'super', size_bytes: 8589934592, slot: '', is_mounted: false, is_high_risk: true, can_backup: true },
  ],
};

let host: HTMLDivElement;
let root: RootHandle;
const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const start = Date.now();
  while (!predicate() && Date.now() - start < timeoutMs) await flushPromises();
  if (!predicate()) throw new Error('timeout waiting for async assertion');
};

const renderLineFlash = () => flushSync(() => root.render(<LineFlashPage />));
const refresh = async () => {
  (host.querySelector('.nw-test-line-partitions-refresh') as HTMLButtonElement).click();
  await waitUntil(() => host.textContent?.includes('super') ?? false);
};

describe('LineFlashPage', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(null);
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(() => {
    flushSync(() => root.unmount());
    host.remove();
  });

  test('以 WPF PARTITION WORKSPACE 呈现真实可视刷写工作区，不保留 ZIP 固件 DOM', () => {
    renderLineFlash();

    expect(host.querySelector('.nw-line-flash-workspace')).not.toBeNull();
    expect(host.querySelector('.nw-line-flash-partition-empty')).not.toBeNull();
    expect(host.querySelector('.nw-test-line-partitions-refresh')).not.toBeNull();
    expect((host.querySelector('.nw-test-line-partitions-refresh') as HTMLButtonElement).disabled).toBe(false);
    expect(host.textContent).toContain('PARTITION / WORKSPACE');
    expect(host.textContent).toContain('传输通道');
    expect(host.textContent).toContain('读取分区表后开始');
    expect(host.querySelector('.nw-line-flash-empty-icon')).not.toBeNull();
    expect(host.textContent).not.toContain('ZIP 固件包');
    expect(host.querySelector('.nw-line-flash-legacy')).toBeNull();
  });

  test('只在用户点击读取分区表时请求快照，并只投影安全分区信息', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValueOnce(null).mockResolvedValueOnce(fastbootSnapshot);
    renderLineFlash();

    expect(command).toHaveBeenCalledWith('partitions_cached_snapshot');
    expect(command).not.toHaveBeenCalledWith('partitions_refresh', expect.anything());
    await refresh();

    expect(command).toHaveBeenCalledWith('partitions_refresh', {
      requestedTransport: 'Automatic',
    });
    expect(host.textContent).toContain('boot_a');
    expect(host.textContent).toContain('高风险');
    expect(host.textContent).not.toContain('FAST-1');
    expect(host.textContent).not.toContain('device_path');
    expect(host.querySelector('.nw-command-output')).toBeNull();
  });

  test('页面重新挂载时恢复已有分区表缓存，不重新读取设备', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValue(fastbootSnapshot);

    renderLineFlash();
    await waitUntil(() => host.textContent?.includes('已恢复 2 个分区') ?? false);
    expect(command).toHaveBeenCalledTimes(1);
    expect(command).toHaveBeenCalledWith('partitions_cached_snapshot');
    expect(command).not.toHaveBeenCalledWith('partitions_refresh', expect.anything());

    flushSync(() => root.render(null));
    renderLineFlash();
    await waitUntil(() => host.textContent?.includes('已恢复 2 个分区') ?? false);

    expect(command).toHaveBeenCalledTimes(2);
    expect(command).not.toHaveBeenCalledWith('partitions_refresh', expect.anything());
  });

  test('分区单击不改变选择，双击分区行才切换选择状态', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValueOnce(null).mockResolvedValueOnce(fastbootSnapshot);
    renderLineFlash();
    await refresh();

    const checkbox = host.querySelector('.nw-test-line-partition-select-boot_a') as HTMLInputElement;
    const row = checkbox.closest('li') as HTMLLIElement;
    checkbox.click();
    await flushPromises();
    expect(checkbox.checked).toBe(false);

    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await waitUntil(() => checkbox.checked);
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await waitUntil(() => !checkbox.checked);
  });

  test('重新读取失败时保留上一次成功读取的分区表', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(fastbootSnapshot)
      .mockRejectedValueOnce(new Error('read failed'));
    renderLineFlash();
    await refresh();

    (host.querySelector('.nw-test-line-partitions-refresh') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('read failed') ?? false);

    expect(host.textContent).toContain('boot_a');
    expect(host.textContent).toContain('super');
  });

  test('选择 ADB Root 后以受限枚举请求对应的分区快照', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValueOnce(null).mockResolvedValueOnce({ ...fastbootSnapshot, transport: 'AdbRoot' });
    renderLineFlash();

    (host.querySelector('.nw-test-line-transport-AdbRoot') as HTMLButtonElement).click();
    (host.querySelector('.nw-test-line-partitions-refresh') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已读取 2 个分区') ?? false);

    expect(command).toHaveBeenCalledWith('partitions_refresh', {
      requestedTransport: 'AdbRoot',
    });
    expect(host.textContent).not.toContain('FAST-1');
  });

  test('分区筛选只显示匹配名称，且不会改变已经读取的快照', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValueOnce(null).mockResolvedValueOnce(fastbootSnapshot);
    renderLineFlash();
    await refresh();

    const filter = host.querySelector('.nw-test-line-partition-filter') as HTMLInputElement;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(filter, 'boot');
    filter.dispatchEvent(new Event('input', { bubbles: true }));

    await waitUntil(() => !(host.textContent?.includes('super') ?? false));
    expect(host.textContent).toContain('boot_a');
    expect(command).toHaveBeenCalledTimes(2);
  });

  test('映射镜像只提交选择结果给 Rust，且不将本地路径投影到页面', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValue(['C:\\private\\boot.img']);
    command
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(fastbootSnapshot)
      .mockResolvedValueOnce({ mapped_count: 1 });
    renderLineFlash();
    await refresh();
    (host.querySelector('.nw-test-line-partitions-select-images') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('已映射 1 个镜像') ?? false);
    expect(command).toHaveBeenNthCalledWith(3, 'partitions_map_images', {
      imagePaths: ['C:\\private\\boot.img'],
    });
    expect(host.textContent).not.toContain('C:\\private\\boot.img');
  });

  test('擦除在确认前只请求摘要，确认后才执行所选分区', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(fastbootSnapshot)
      .mockResolvedValueOnce({ task_count: 1, high_risk_count: 1, mounted_count: 0 })
      .mockResolvedValueOnce({ command_count: 1, executed_count: 1 });
    renderLineFlash();
    await refresh();
    const selectedSuper = host.querySelector('.nw-test-line-partition-select-super') as HTMLInputElement;
    selectedSuper.closest('li')?.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await waitUntil(() => selectedSuper.checked);
    (host.querySelector('.nw-test-line-partitions-prepare-erase') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    expect(command).toHaveBeenNthCalledWith(3, 'partitions_prepare_erase', { selectedNames: ['super'] });
    expect(command).not.toHaveBeenCalledWith('partitions_execute_erase', expect.anything());
    (host.querySelector('.nw-test-line-partitions-confirm-erase') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('分区擦除已完成') ?? false);
    expect(command).toHaveBeenNthCalledWith(4, 'partitions_execute_erase', { selectedNames: ['super'] });
  });

  test('写入在确认前只请求摘要，确认后才执行所选分区', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(fastbootSnapshot)
      .mockResolvedValueOnce({ task_count: 1, high_risk_count: 0, mounted_count: 0 })
      .mockResolvedValueOnce({ command_count: 1, executed_count: 1 });
    renderLineFlash();
    await refresh();
    const selectedBoot = host.querySelector('.nw-test-line-partition-select-boot_a') as HTMLInputElement;
    selectedBoot.closest('li')?.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await waitUntil(() => selectedBoot.checked);
    (host.querySelector('.nw-test-line-partitions-prepare-write') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    expect(command).toHaveBeenNthCalledWith(3, 'partitions_prepare_write', { selectedNames: ['boot_a'] });
    expect(command).not.toHaveBeenCalledWith('partitions_execute_write', expect.anything());
    (host.querySelector('.nw-test-line-partitions-confirm-write') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('分区写入已完成') ?? false);
    expect(command).toHaveBeenNthCalledWith(4, 'partitions_execute_write', { selectedNames: ['boot_a'] });
  });

  test('备份选择目录后直接执行，不再请求预检或显示二次确认', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValue('C:\\private\\backups');
    command
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(fastbootSnapshot)
      .mockResolvedValueOnce({ command_count: 1, executed_count: 1 });
    renderLineFlash();
    await refresh();
    const selectedBoot = host.querySelector('.nw-test-line-partition-select-boot_a') as HTMLInputElement;
    selectedBoot.closest('li')?.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await waitUntil(() => selectedBoot.checked);
    (host.querySelector('.nw-test-line-partitions-backup') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('分区备份已完成') ?? false);
    expect(command).toHaveBeenNthCalledWith(3, 'partitions_execute_backup', {
      selectedNames: ['boot_a'],
      outputDirectory: 'C:\\private\\backups',
    });
    expect(host.textContent).not.toContain('C:\\private\\backups');
    expect(command).not.toHaveBeenCalledWith('partitions_prepare_backup', expect.anything());
    expect(host.querySelector('[role="dialog"]')).toBeNull();
  });

  test('停止仅请求取消当前 coordinator 操作', async () => {
    renderLineFlash();
    (host.querySelector('.nw-test-line-partitions-cancel') as HTMLButtonElement).click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.some(([name]) => name === 'operation_cancel'));
    expect(invoke).toHaveBeenCalledWith('operation_cancel');
  });
});

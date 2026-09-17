import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { PartitionFailureDialog } from './PartitionFailureDialog';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { invoke } from '@tauri-apps/api/core';

let host: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
const wait = () => new Promise((resolve) => setTimeout(resolve, 0));
const onResolved = vi.fn();

const failure = {
  sessionId: 'safe-1',
  partitionName: 'boot',
  errorMessage: '退出码 1。\nFAILED (remote: partition write failed)',
};

describe('PartitionFailureDialog', () => {
  beforeEach(() => { host = document.createElement('div'); document.body.appendChild(host); root = createRoot(host); });
  afterEach(() => { flushSync(() => root.unmount()); host.remove(); vi.clearAllMocks(); });

  test('无失败提示时不渲染弹窗内容', () => {
    flushSync(() => root.render(<PartitionFailureDialog failure={null} onResolved={onResolved} />));
    expect(host.querySelector('.nw-partition-failure-dialog')).toBeNull();
  });

  test('展示失败分区名与具体报错日志，并提供重试/继续/中止三种处置', () => {
    flushSync(() => root.render(<PartitionFailureDialog failure={failure} onResolved={onResolved} />));

    expect(host.querySelector('[role="alertdialog"]')?.getAttribute('aria-label')).toBe('分区刷写失败');
    expect(host.querySelector('.nw-partition-failure-partition')?.textContent).toContain('boot');
    expect(host.querySelector('.nw-partition-failure-log')?.textContent).toContain('partition write failed');
    const buttons = Array.from(host.querySelectorAll('button')).map((button) => button.textContent);
    expect(buttons).toContain('重试当前分区');
    expect(buttons).toContain('继续刷写剩余分区');
    expect(buttons).toContain('中止本次线刷');
  });

  test('选择重试当前分区时把 retry 决策发回后端', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);
    flushSync(() => root.render(<PartitionFailureDialog failure={failure} onResolved={onResolved} />));
    Array.from(host.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => button.textContent === '重试当前分区')?.click();
    await wait();

    expect(invoke).toHaveBeenCalledWith('safe_flash_resolve_partition_failure', {
      resolution: { session_id: 'safe-1', decision: 'retry' },
    });
    expect(onResolved).toHaveBeenCalledWith(failure);
  });

  test('选择继续刷写时把 continue 决策发回后端', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);
    flushSync(() => root.render(<PartitionFailureDialog failure={failure} onResolved={onResolved} />));
    Array.from(host.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => button.textContent === '继续刷写剩余分区')?.click();
    await wait();

    expect(invoke).toHaveBeenCalledWith('safe_flash_resolve_partition_failure', {
      resolution: { session_id: 'safe-1', decision: 'continue' },
    });
    expect(onResolved).toHaveBeenCalledWith(failure);
  });

  test('选择中止时把 abort 决策发回后端', async () => {
    (invoke as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);
    flushSync(() => root.render(<PartitionFailureDialog failure={failure} onResolved={onResolved} />));
    Array.from(host.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => button.textContent === '中止本次线刷')?.click();
    await wait();

    expect(invoke).toHaveBeenCalledWith('safe_flash_resolve_partition_failure', {
      resolution: { session_id: 'safe-1', decision: 'abort' },
    });
    expect(onResolved).toHaveBeenCalledWith(failure);
  });

  test('决策未送达（提示已被后端收尾）时不报错打扰用户', async () => {
    const consoleDebug = vi.spyOn(console, 'debug').mockImplementation(() => {});
    (invoke as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('没有正在等待决策的线刷会话。'));
    flushSync(() => root.render(<PartitionFailureDialog failure={failure} onResolved={onResolved} />));
    const abortButton = Array.from(host.querySelectorAll<HTMLButtonElement>('button'))
      .find((button) => button.textContent === '中止本次线刷');
    abortButton?.click();
    await wait();

    expect(consoleDebug).toHaveBeenCalled();
    expect(onResolved).toHaveBeenCalledWith(failure);
    expect(host.querySelector('.nw-partition-failure-dialog')).not.toBeNull();
    consoleDebug.mockRestore();
  });
});

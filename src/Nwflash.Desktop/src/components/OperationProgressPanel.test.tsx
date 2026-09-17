import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import { OperationProgressPanel } from './OperationProgressPanel';
import type { OperationSnapshotPayload, PartitionTaskSnapshotPayload } from '../app/ipc-events';

type RootHandle = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: RootHandle;

const renderPanel = (
  operations: ReadonlyArray<{ kind: 'quick' | 'lineFlash' | 'safeFlash' | 'firmwareExtract' | 'device'; message: string }>,
  operationSnapshot?: OperationSnapshotPayload | null,
) => {
  flushSync(() => {
    root.render(<OperationProgressPanel operations={operations} operationSnapshot={operationSnapshot} />);
  });
};

const partitionTask = (
  name: string,
  state: PartitionTaskSnapshotPayload['state'],
  overallProgress: number,
): PartitionTaskSnapshotPayload => ({
  partition_name: name,
  state,
  overall_progress: overallProgress,
});

describe('OperationProgressPanel', () => {
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
  });

  test('空闲时显示无进行中的操作且不渲染进度条', () => {
    renderPanel([]);

    const panel = host.querySelector('[data-role="operation-progress"]') as HTMLElement;
    expect(panel.textContent).toContain('操作进度');
    expect(panel.textContent).toContain('无进行中的操作');
    expect(host.querySelectorAll('.nw-progress-bar')).toHaveLength(0);
  });

  test('有任务但无快照时显示单个流动进度条', () => {
    renderPanel([{ kind: 'device', message: '正在探测设备' }]);

    const panel = host.querySelector('[data-role="operation-progress"]') as HTMLElement;
    expect(panel.textContent).toContain('设备操作：正在探测设备');
    expect(host.querySelectorAll('.nw-progress-bar.is-indeterminate')).toHaveLength(1);
    expect(host.querySelectorAll('.nw-progress-row')).toHaveLength(0);
  });

  test('分区任务渲染当前分区与总进度两条进度条', () => {
    renderPanel(
      [{ kind: 'quick', message: '正在刷写分区' }],
      {
        kind: 'Flashing',
        operationId: 'op-1',
        title: '快速刷写',
        stage: '正在刷写 boot',
        progress: 0.35,
        startedAt: 1,
        isCancellable: true,
        partitionTask: partitionTask('boot', 'Running', 0.35),
        partitionTasks: [
          partitionTask('boot', 'Running', 0.35),
          partitionTask('init_boot', 'Succeeded', 1),
        ],
        isBusy: true,
      },
    );

    const rows = [...host.querySelectorAll('.nw-progress-row')];
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain('当前分区');
    expect(rows[0].textContent).toContain('boot');
    expect(rows[0].textContent).toContain('35%');
    expect(rows[1].textContent).toContain('总进度');
    // 总进度 = (0.35 + 1) / 2 ≈ 68%
    expect(rows[1].textContent).toContain('68%');

    const bars = [...host.querySelectorAll('.nw-progress-bar')];
    expect(bars).toHaveLength(2);
    expect(bars[0].classList.contains('is-indeterminate')).toBe(false);
    expect((bars[0].querySelector('span') as HTMLElement).style.width).toBe('35%');
    expect((bars[1].querySelector('span') as HTMLElement).style.width).toBe('68%');
  });

  test('分区运行中进度为零时当前分区条显示流动动画', () => {
    renderPanel(
      [{ kind: 'safeFlash', message: '正在写入分区' }],
      {
        kind: 'Flashing',
        operationId: 'op-2',
        title: 'VIVO 线刷',
        stage: '正在写入 boot',
        progress: 0,
        startedAt: 1,
        isCancellable: true,
        partitionTask: partitionTask('boot', 'Running', 0),
        partitionTasks: [partitionTask('boot', 'Running', 0)],
        isBusy: true,
      },
    );

    const bars = [...host.querySelectorAll('.nw-progress-bar')];
    expect(bars).toHaveLength(2);
    expect(bars[0].classList.contains('is-indeterminate')).toBe(true);
    expect((bars[0].querySelector('span') as HTMLElement).style.width).toBe('');
  });

  test('无分区任务但有进度值时显示单条总进度条', () => {
    renderPanel(
      [{ kind: 'firmwareExtract', message: '正在提取固件' }],
      {
        kind: 'Transferring',
        operationId: 'op-3',
        title: '固件提取',
        stage: '正在提取 payload',
        progress: 0.42,
        startedAt: 1,
        isCancellable: true,
        partitionTask: null,
        partitionTasks: [],
        isBusy: true,
      },
    );

    const rows = [...host.querySelectorAll('.nw-progress-row')];
    expect(rows).toHaveLength(1);
    expect(rows[0].textContent).toContain('总进度');
    expect(rows[0].textContent).toContain('42%');

    const bars = [...host.querySelectorAll('.nw-progress-bar')];
    expect(bars).toHaveLength(1);
    expect(bars[0].classList.contains('is-indeterminate')).toBe(false);
    expect((bars[0].querySelector('span') as HTMLElement).style.width).toBe('42%');
  });

  test('进度为 null 时显示流动进度条', () => {
    renderPanel(
      [{ kind: 'device', message: '正在重启设备' }],
      {
        kind: 'Rebooting',
        operationId: 'op-4',
        title: '设备操作',
        stage: '正在重启设备',
        progress: null,
        startedAt: 1,
        isCancellable: false,
        partitionTask: null,
        partitionTasks: [],
        isBusy: true,
      },
    );

    expect(host.querySelectorAll('.nw-progress-bar.is-indeterminate')).toHaveLength(1);
    expect(host.querySelectorAll('.nw-progress-row')).toHaveLength(0);
  });
});

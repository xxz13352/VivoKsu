import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { OperationLogPanel } from './OperationLogPanel';
import type { OperationLogEntry } from './OperationLogPanel';
import type { OperationSnapshotPayload } from '../app/ipc-events';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';

type Unmount = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: Unmount;

const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const start = Date.now();
  while (!predicate() && Date.now() - start < timeoutMs) {
    await flushPromises();
  }
  if (!predicate()) {
    throw new Error('timeout waiting for panel render');
  }
};

describe('OperationLogPanel', () => {
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

  test('按发生顺序从上到下展示日志，并自动滚到最新记录', async () => {
    let resolveSnapshot!: (entries: unknown) => void;
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(() => new Promise((resolve) => {
      resolveSnapshot = resolve;
    }));

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    const panel = host.querySelector('[data-role="operation-log-panel"]') as HTMLElement;
    let scrollTop = 0;
    Object.defineProperty(panel, 'scrollTop', {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });
    Object.defineProperty(panel, 'scrollHeight', { configurable: true, value: 240 });
    resolveSnapshot([
      {
        timestamp_utc: 1760000000,
        level: 'Info',
        message: '示例一',
        operation_id: 'op-1',
      },
      {
        timestamp_utc: 1760000001,
        level: 'Success',
        message: '示例二',
        operation_id: null,
      },
    ]);

    await waitUntil(() => host.querySelectorAll('.nw-operation-log-preview li').length === 2);
    await flushPromises();
    expect(invoke).toHaveBeenCalledWith('operation_logs_snapshot');

    const items = host.querySelectorAll('.nw-operation-log-preview li');
    expect(items.length).toBe(2);
    expect(items[0]?.textContent ?? '').toContain('示例一');
    expect(items[1]?.textContent ?? '').toContain('示例二');
    expect(panel.scrollTop).toBe(240);
  });

  test('保留完整内存日志集合供右侧轨道滚动查看', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(
      Array.from({ length: 6 }, (_, index) => ({
        timestamp_utc: 1760000000 + index,
        level: 'Info',
        message: `日志 ${index + 1}`,
        operation_id: `op-${index + 1}`,
      })),
    );

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    await waitUntil(() => host.querySelectorAll('.nw-operation-log-preview li').length === 6);
    expect(host.textContent).toContain('日志 1');
    expect(host.textContent).toContain('日志 6');
  });

  test('无日志时展示空态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    await waitUntil(() => host.querySelector('.nw-empty-log') !== null);
    const empty = host.querySelector('.nw-empty-log') as HTMLParagraphElement;
    expect(empty.textContent).toBe('会话活动将显示在这里');
  });

  test('无日志时保留 WPF 的活动日志标题、会话空态和底部说明', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    await waitUntil(() => host.querySelector('.nw-empty-log') !== null);

    expect(host.querySelector('.nw-operation-log-heading')?.textContent).toBe('操作日志');
    expect(host.querySelector('.nw-operation-log-count')?.textContent).toBe('0 条记录');
    expect(host.querySelector('.nw-operation-log-empty')?.textContent).toContain('等待操作记录');
  });

  test('清空操作日志调用 Rust 内存清理命令并显示会话空态', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation((command: string) => {
      if (command === 'operation_logs_snapshot') {
        return Promise.resolve([
          {
            timestamp_utc: 1760000000,
            level: 'Info',
            message: '待清空日志',
            operation_id: null,
          },
        ]);
      }
      if (command === 'operation_logs_clear') return Promise.resolve();
      return Promise.resolve();
    });

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    await waitUntil(() => host.querySelector('.nw-operation-log-preview li') !== null);
    (host.querySelector('[aria-label="清空操作日志"]') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('.nw-operation-log-empty') !== null);
    expect(invoke).toHaveBeenCalledWith('operation_logs_clear');
    expect(host.querySelector('.nw-operation-log-preview')).toBeNull();
  });

  test('服务端返回非法结构时安全回退空列表', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      entries: [],
    });

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    await waitUntil(() => host.querySelector('.nw-empty-log') !== null);
    const empty = host.querySelector('.nw-empty-log') as HTMLParagraphElement;
    expect(empty.textContent).toBe('会话活动将显示在这里');
    expect(host.textContent ?? '').not.toContain('示例');
  });

  test('操作事件到达后展示日志，即使初始快照读取失败', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('快照不可用'));

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });
    await waitUntil(() => host.textContent?.includes('快照不可用') ?? false);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Flashing',
        operationId: 'operation-1',
        title: '快速刷写',
        stage: '正在写入 boot 分区',
        progress: 0.5,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });

    await waitUntil(() => host.querySelectorAll('.nw-operation-log-preview li').length === 1);
    expect(host.textContent).not.toContain('快照不可用');
  });

  test('实时服务器探测会写入操作日志', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Hashing',
        operationId: 'operation-ota',
        title: '检测服务器 OTA',
        stage: '正在解析服务器 OTA',
        progress: 0,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });

    await flushPromises();
    await flushPromises();
    expect(host.textContent).toContain('正在解析服务器 固件');
  });

  test('完成的服务器探测也会写入操作日志', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Completed',
        operationId: 'operation-ota-completed',
        title: '检测服务器固件',
        stage: '检测服务器固件完成。',
        progress: 1,
        startedAt: 1700000000,
        isCancellable: false,
        isBusy: false,
      }} />);
    });

    await flushPromises();
    await flushPromises();
    expect(host.textContent).toContain('检测服务器固件完成。');
  });

  test('隐藏空日志和 VIVO 线刷准备标题', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        timestamp_utc: 1760000000,
        level: 'Info',
        message: '',
        operation_id: null,
      },
      {
        timestamp_utc: 1760000001,
        level: 'Info',
        message: '准备 VIVO 线刷',
        operation_id: null,
      },
      {
        timestamp_utc: 1760000002,
        level: 'Info',
        message: '正在检查本地固件',
        operation_id: null,
      },
    ]);

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    await waitUntil(() => host.textContent?.includes('正在检查本地固件') ?? false);
    const items = host.querySelectorAll('.nw-operation-log-preview li');
    expect(items.length).toBe(1);
    expect(host.textContent).not.toContain('准备 VIVO 线刷');
  });

  test('保留旧会话中已归一化的服务器检测完成日志', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      {
        timestamp_utc: 1760000000,
        level: 'Info',
        message: '检测服务器 OTA完成。',
        operation_id: null,
      },
      {
        timestamp_utc: 1760000001,
        level: 'Info',
        message: '检测服务器 OTA已取消。',
        operation_id: null,
      },
      {
        timestamp_utc: 1760000002,
        level: 'Info',
        message: '正在检查本地固件',
        operation_id: null,
      },
    ]);

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });

    await waitUntil(() => host.textContent?.includes('正在检查本地固件') ?? false);
    expect(host.querySelectorAll('.nw-operation-log-preview li').length).toBe(3);
    expect(host.textContent).toContain('检测服务器 固件完成。');
    expect(host.textContent).toContain('检测服务器 固件已取消。');
  });

  test('空闲操作快照不会追加空白日志行', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Idle',
        operationId: null,
        title: '',
        stage: '',
        progress: null,
        startedAt: null,
        isCancellable: false,
        isBusy: false,
      }} />);
    });

    await flushPromises();
    await flushPromises();
    expect(host.querySelector('.nw-operation-log-preview')).toBeNull();
    expect(host.querySelector('.nw-empty-log')).not.toBeNull();
  });

  test('运行中快照带进度时渲染实时进度行', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Transferring',
        operationId: 'operation-download',
        title: '下载设备文件',
        stage: '下载设备文件',
        progress: 0.45,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });

    await flushPromises();
    const progressLine = host.querySelector('.nw-operation-log-progress');
    expect(progressLine).not.toBeNull();
    expect(progressLine?.textContent).toContain('下载设备文件 45%');
  });

  test('进度推进时进度行原地刷新且不产生新日志条目', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    const downloadSnapshot = (progress: number): OperationSnapshotPayload => ({
      kind: 'Transferring',
      operationId: 'operation-download',
      title: '下载设备文件',
      stage: '下载设备文件',
      progress,
      startedAt: 1700000000,
      isCancellable: true,
      isBusy: true,
    });

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={downloadSnapshot(0.45)} />);
    });
    await flushPromises();
    expect(host.querySelectorAll('.nw-operation-log-progress')).toHaveLength(1);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={downloadSnapshot(0.78)} />);
    });
    await flushPromises();

    const progressLines = host.querySelectorAll('.nw-operation-log-progress');
    expect(progressLines).toHaveLength(1);
    expect(progressLines[0].textContent).toContain('下载设备文件 78%');
    // 进度刷新只走快照派生：起始条目在首次快照时已记录，推进进度不再新增。
    expect(host.querySelectorAll('.nw-operation-log-preview li')).toHaveLength(1);
  });

  test('操作完成后进度行消失并显示完成日志', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Transferring',
        operationId: 'operation-download',
        title: '下载设备文件',
        stage: '下载设备文件',
        progress: 0.9,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });
    await flushPromises();
    expect(host.querySelector('.nw-operation-log-progress')).not.toBeNull();

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Completed',
        operationId: 'operation-download',
        title: '下载设备文件',
        stage: '下载设备文件完成。',
        progress: 1,
        startedAt: 1700000000,
        isCancellable: false,
        isBusy: false,
      }} />);
    });

    await waitUntil(() => host.textContent?.includes('下载设备文件完成。') ?? false);
    expect(host.querySelector('.nw-operation-log-progress')).toBeNull();
  });

  test('运行中但进度为 null 时不渲染进度行', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Rebooting',
        operationId: 'operation-reboot',
        title: '设备操作',
        stage: '正在重启设备',
        progress: null,
        startedAt: 1700000000,
        isCancellable: false,
        isBusy: true,
      }} />);
    });

    await flushPromises();
    expect(host.querySelector('.nw-operation-log-progress')).toBeNull();
  });

  test('被隐藏的运行阶段不渲染进度行', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Discovering',
        operationId: 'operation-safe-flash',
        title: '准备 VIVO 线刷',
        stage: '准备 VIVO 线刷环境',
        progress: 0.3,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });

    await flushPromises();
    expect(host.querySelector('.nw-operation-log-progress')).toBeNull();
  });

  test('操作快速失败时（终态与 Idle 几乎同时到达）仍能看到后端写入的失败日志', async () => {
    const persisted: OperationLogEntry[] = [
      {
        timestamp_utc: 1700000000,
        level: 'Info',
        message: '读取 ADB Root 分区表',
        operation_id: 'op-fast-fail',
      },
      {
        timestamp_utc: 1700000001,
        level: 'Error',
        message: '当前操作无法完成，请检查设备和所选内容后重试。',
        operation_id: 'op-fast-fail',
      },
      {
        timestamp_utc: 1700000001,
        level: 'Warning',
        message: '失败详情：ADB 设备未授予 Root 权限（su 不可用或已拒绝），退出码 255：',
        operation_id: 'op-fast-fail',
      },
    ];
    const mockedInvoke = invoke as unknown as ReturnType<typeof vi.fn>;
    // 挂载时操作尚未开始，后端还没有相关记录。
    mockedInvoke.mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });
    await flushPromises();

    // 操作过程中后端已把失败原因落盘。
    mockedInvoke.mockResolvedValue(persisted);

    const base = {
      operationId: 'op-fast-fail',
      title: '读取 ADB Root 分区表',
      startedAt: 1700000000,
      isCancellable: true,
      isBusy: true,
      progress: 0,
    };

    // 同一批内连续投递开始 -> 阶段 -> Failed -> Idle：React 会合并这批更新，
    // 只有最后一次（Idle）会真正渲染，阶段与终态快照都不会触发 effect。
    root.render(<OperationLogPanel operationSnapshot={{
      ...base,
      kind: 'Discovering',
      stage: '读取 ADB Root 分区表',
    }} />);
    root.render(<OperationLogPanel operationSnapshot={{
      ...base,
      kind: 'Discovering',
      stage: '检查 ADB Root 并读取分区表',
    }} />);
    root.render(<OperationLogPanel operationSnapshot={{
      ...base,
      kind: 'Failed',
      stage: '当前操作无法完成，请检查设备和所选内容后重试。',
      isBusy: false,
    }} />);
    root.render(<OperationLogPanel operationSnapshot={{
      kind: 'Idle',
      operationId: null,
      title: '',
      stage: '',
      progress: null,
      startedAt: null,
      isCancellable: false,
      isBusy: false,
    }} />);

    await waitUntil(() => host.textContent?.includes('失败详情') === true);
    expect(host.textContent).toContain('当前操作无法完成');
  });
});

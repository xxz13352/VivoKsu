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

    // 真正的滚动宿主是 .nw-operation-log-body（面板自身高度自适应，滚动它
    // 没有任何效果——这正是「日志不自动滚动」的老 bug）。
    const scrollHost = host.querySelector('.nw-operation-log-body') as HTMLElement;
    let scrollTop = 0;
    Object.defineProperty(scrollHost, 'scrollTop', {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });
    Object.defineProperty(scrollHost, 'scrollHeight', { configurable: true, value: 240 });
    Object.defineProperty(scrollHost, 'clientHeight', { configurable: true, value: 100 });
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
    expect(scrollTop).toBe(240);
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

  /// 给滚动宿主装上可观测的 scrollTop/scrollHeight/clientHeight。
  const instrumentScrollHost = (scrollHeight: number, clientHeight: number) => {
    const scrollHost = host.querySelector('.nw-operation-log-body') as HTMLElement;
    let scrollTop = 0;
    Object.defineProperty(scrollHost, 'scrollTop', {
      configurable: true,
      get: () => scrollTop,
      set: (value: number) => {
        scrollTop = value;
      },
    });
    Object.defineProperty(scrollHost, 'scrollHeight', { configurable: true, value: scrollHeight });
    Object.defineProperty(scrollHost, 'clientHeight', { configurable: true, value: clientHeight });
    return {
      get scrollTop() {
        return scrollTop;
      },
      scrollTo: (top: number) => {
        scrollTop = top;
        scrollHost.dispatchEvent(new Event('scroll'));
      },
    };
  };

  test('新日志到达时自动滚动到底部', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      { timestamp_utc: 1760000000, level: 'Info', message: '刷写分区[1/38]', operation_id: 'op-1' },
    ]);

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });
    await waitUntil(() => host.querySelectorAll('.nw-operation-log-preview li').length === 1);

    const scrollHost = instrumentScrollHost(600, 200);
    // 重新渲染触发贴合底部：新日志（或进度行）到达即跟到最新一行。
    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Flashing',
        operationId: 'op-1',
        title: 'VIVO 线刷',
        stage: '刷写分区[2/38]',
        progress: 0.4,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });
    await flushPromises();

    expect(scrollHost.scrollTop).toBe(600);
  });

  test('用户向上翻阅历史时不再自动滚动，回到底部后恢复跟随', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      { timestamp_utc: 1760000000, level: 'Info', message: '刷写分区[1/38]', operation_id: 'op-1' },
    ]);

    flushSync(() => {
      root.render(<OperationLogPanel />);
    });
    await waitUntil(() => host.querySelectorAll('.nw-operation-log-preview li').length === 1);

    const scrollHost = instrumentScrollHost(600, 200);
    // 用户滚到顶部：离底部 400px，远超跟随阈值。
    scrollHost.scrollTo(0);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Flashing',
        operationId: 'op-1',
        title: 'VIVO 线刷',
        stage: '刷写分区[2/38]',
        progress: 0.4,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });
    await flushPromises();
    expect(scrollHost.scrollTop).toBe(0);

    // 用户回到底部：自动跟随立即恢复。
    scrollHost.scrollTo(415);
    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Flashing',
        operationId: 'op-1',
        title: 'VIVO 线刷',
        stage: '刷写分区[3/38]',
        progress: 0.5,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });
    await flushPromises();
    expect(scrollHost.scrollTop).toBe(600);
  });

  test('刷写分区进度行只显示 i/n，不带省略号或结论文案', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Flashing',
        operationId: 'operation-safe-flash',
        title: 'VIVO 线刷',
        stage: '刷写分区[28/38]',
        progress: 28 / 38,
        startedAt: 1700000000,
        isCancellable: true,
        isBusy: true,
      }} />);
    });
    await flushPromises();

    const progressLine = host.querySelector('.nw-operation-log-progress');
    expect(progressLine).not.toBeNull();
    expect(progressLine?.textContent).toContain('刷写分区[28/38]');
    expect(progressLine?.textContent).not.toContain('...');
    expect(progressLine?.textContent).not.toContain('OK');
  });

  test('分区刷写日志各带各的时间，不会全被钉在操作起点', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    // 操作起点固定在 10:00:00（= 1700000000）。日志时间必须是快照到达的
    // 真实时刻：原先借 `startedAt` 渲染，整轮刷写的每一行都会显示同一个
    // 操作起点——这正是「客户端时间全是第一个分区的时间」的根因。
    const stamps: string[] = [];
    const write = (stage: string, arrivedAtSeconds: number) => {
      const realNow = Date.now;
      Date.now = () => arrivedAtSeconds * 1000;
      try {
        flushSync(() => {
          root.render(<OperationLogPanel operationSnapshot={{
            kind: 'Flashing',
            operationId: 'operation-safe-flash',
            title: 'VIVO 线刷',
            stage,
            progress: 0.1,
            startedAt: 1700000000,
            isCancellable: true,
            isBusy: true,
          }} />);
        });
      } finally {
        Date.now = realNow;
      }
    };

    write('刷写分区[1/38]', 1700000000);
    await flushPromises();
    write('刷写分区[2/38]', 1700000000 + 37);
    await flushPromises();
    write('刷写分区[3/38]', 1700000000 + 95);
    await flushPromises();

    await waitUntil(() => host.querySelectorAll('.nw-operation-log-preview li').length === 3);
    const lines = [...host.querySelectorAll('.nw-operation-log-preview li')].map(
      (item) => item.textContent ?? '',
    );
    expect(lines[0]).toContain('刷写分区[1/38]');
    expect(lines[1]).toContain('刷写分区[2/38]');
    expect(lines[2]).toContain('刷写分区[3/38]');

    for (const line of lines) {
      stamps.push(line.slice(line.indexOf('[') + 1, line.indexOf(']')));
    }
    // 三行时间互不相同，且都不等于操作起点——借 startedAt 会三者全等。
    expect(new Set(stamps).size).toBe(3);
    expect(stamps[0]).not.toBe(stamps[2]);
    expect(stamps[0]).toBe(
      new Date(1700000000 * 1000).toLocaleTimeString('zh-CN', { hour12: false }),
    );
    expect(stamps[2]).toBe(
      new Date((1700000000 + 95) * 1000).toLocaleTimeString('zh-CN', { hour12: false }),
    );
  });

  test('同一条刷写阶段重复广播时只保留一条并推进到最后一次观测', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    const write = (arrivedAtSeconds: number) => {
      const realNow = Date.now;
      Date.now = () => arrivedAtSeconds * 1000;
      try {
        flushSync(() => {
          root.render(<OperationLogPanel operationSnapshot={{
            kind: 'Flashing',
            operationId: 'operation-safe-flash',
            title: 'VIVO 线刷',
            stage: '刷写分区[1/38]',
            progress: 0.1,
            startedAt: 1700000000,
            isCancellable: true,
            isBusy: true,
          }} />);
        });
      } finally {
        Date.now = realNow;
      }
    };

    write(1700000000);
    await flushPromises();
    // 同一条 stage 因进度推进反复广播：不得刷屏，时间要推进到最后一次观测。
    write(1700000000 + 5);
    await flushPromises();
    write(1700000000 + 9);
    await flushPromises();

    await waitUntil(() => host.querySelectorAll('.nw-operation-log-preview li').length === 1);
    const line = host.querySelectorAll('.nw-operation-log-preview li')[0]?.textContent ?? '';
    expect(line).toContain('刷写分区[1/38]');
    const stamp = line.slice(line.indexOf('[') + 1, line.indexOf(']'));
    expect(stamp).toBe(
      new Date((1700000000 + 9) * 1000).toLocaleTimeString('zh-CN', { hour12: false }),
    );
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

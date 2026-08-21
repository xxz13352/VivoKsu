import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { OperationLogPanel } from './OperationLogPanel';

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

    expect(host.querySelector('.nw-operation-log-eyebrow')?.textContent).toBe('ACTIVITY LOG');
    expect(host.querySelector('.nw-operation-log-heading')?.textContent).toBe('操作日志');
    expect(host.querySelector('.nw-operation-log-count')?.textContent).toBe('0 条记录');
    expect(host.querySelector('.nw-operation-log-empty')?.textContent).toContain('SESSION LOG');
    expect(host.querySelector('.nw-operation-log-empty')?.textContent).toContain('等待操作记录');
    expect(host.querySelector('.nw-operation-log-footer')?.textContent).toBe('实时记录当前会话的设备操作');
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

    await waitUntil(() => host.textContent?.includes('正在写入 boot 分区') ?? false);
    expect(host.textContent).not.toContain('快照不可用');
  });

  test('实时服务器探测不会写入操作日志', async () => {
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
    expect(host.textContent).not.toContain('正在请求服务器');
    expect(host.textContent).not.toContain('OTA');
    expect(host.querySelector('.nw-empty-log')).not.toBeNull();
  });

  test('完成的服务器探测也不会写入操作日志', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);

    flushSync(() => {
      root.render(<OperationLogPanel operationSnapshot={{
        kind: 'Completed',
        operationId: 'operation-ota-completed',
        title: '检测服务器 OTA',
        stage: '检测服务器 OTA完成。',
        progress: 1,
        startedAt: 1700000000,
        isCancellable: false,
        isBusy: false,
      }} />);
    });

    await flushPromises();
    await flushPromises();
    expect(host.textContent).not.toContain('检测服务器 固件完成。');
    expect(host.querySelector('.nw-empty-log')).not.toBeNull();
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

  test('隐藏旧会话中已归一化的服务器检测完成日志', async () => {
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
    expect(host.querySelectorAll('.nw-operation-log-preview li').length).toBe(1);
    expect(host.textContent).not.toContain('检测服务器 固件完成。');
    expect(host.textContent).not.toContain('检测服务器 固件已取消。');
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
});

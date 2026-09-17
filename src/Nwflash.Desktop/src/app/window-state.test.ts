import { describe, expect, test } from 'vitest';
import { NO_OPERATION_TEXT, PROGRESS_CHANNEL_ORDER, resolveProgressText, type BusyOperationItem } from './window-state';

describe('window-state', () => {
  test('无任务时返回空闲提示', () => {
    expect(resolveProgressText([])).toBe(NO_OPERATION_TEXT);
  });

  test('任务优先级为 快速刷写 > 可视刷写 > VIVO线刷 > 固件提取 > 设备操作', () => {
    const operations: readonly BusyOperationItem[] = [
      { kind: 'device', message: '连接检测中' },
      { kind: 'lineFlash', message: '刷写清单加载中' },
      { kind: 'firmwareExtract', message: '解包中' },
      { kind: 'quick', message: '正在刷写分区' },
      { kind: 'safeFlash', message: '检测到 fastbootd' },
    ];

    const line = resolveProgressText(operations);
    expect(line).toContain('快速刷写');
    expect(line).toContain('正在刷写分区');
  });

  test('优先顺序列表必须是计划定义顺序', () => {
    expect(PROGRESS_CHANNEL_ORDER).toEqual([
      'quick',
      'lineFlash',
      'safeFlash',
      'firmwareExtract',
      'device',
    ]);
  });
});

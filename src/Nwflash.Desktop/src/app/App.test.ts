import { describe, expect, test } from 'vitest';
import { formatCurrentTime, resolveBusyKind } from './App';
import type { OperationSnapshotPayload, OperationKind } from './ipc-events';

const createBusySnapshot = (
  kind: OperationKind,
  title: string,
  isBusy = true,
): OperationSnapshotPayload => ({
  kind,
  operationId: 'op-1',
  title,
  stage: title,
  progress: null,
  startedAt: null,
  isCancellable: false,
  isBusy,
});

describe('formatCurrentTime', () => {
  test('使用 MM-dd HH:mm:ss 格式', () => {
    const fixedDate = new Date('2026-08-16T07:04:05.000+08:00');
    expect(formatCurrentTime(fixedDate)).toBe('08-16 07:04:05');
  });
});

describe('resolveBusyKind', () => {
  test('非忙碌快照返回 null', () => {
    expect(
      resolveBusyKind(
        createBusySnapshot('Flashing', '正在刷写分区', false),
      ),
    ).toBeNull();
  });

  test('可视刷写类快照映射到 quick（kind 优先）', () => {
    expect(resolveBusyKind(createBusySnapshot('Flashing', '可视刷写进行中'))).toBe('quick');
  });

  test('刷写类快照映射到 quick', () => {
    expect(resolveBusyKind(createBusySnapshot('Flashing', '正在刷写系统分区'))).toBe('quick');
  });

  test('完成/取消/失败快照不再视作忙态', () => {
    expect(resolveBusyKind(createBusySnapshot('Completed', '完成', false))).toBeNull();
    expect(resolveBusyKind(createBusySnapshot('Canceled', '已取消', false))).toBeNull();
    expect(resolveBusyKind(createBusySnapshot('Failed', '失败', false))).toBeNull();
  });

  test('由 kind 直接映射 hash/安装/传输到独立进度通道', () => {
    expect(resolveBusyKind(createBusySnapshot('Hashing', '文件哈希中'))).toBe('firmwareExtract');
    expect(resolveBusyKind(createBusySnapshot('Installing', '刷入安装包'))).toBe('safeFlash');
    expect(resolveBusyKind(createBusySnapshot('Transferring', '传输文件中'))).toBe('lineFlash');
  });

  test('由 kind 映射设备通道', () => {
    expect(resolveBusyKind(createBusySnapshot('Mirroring', '镜像显示'))).toBe('device');
    expect(resolveBusyKind(createBusySnapshot('Rebooting', '重启中'))).toBe('device');
    expect(resolveBusyKind(createBusySnapshot('Discovering', '设备扫描中'))).toBe('device');
  });
});

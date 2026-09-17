import { errorMessage } from '../app/error';
import { FC, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { deviceDetailValue, isConnectedDeviceSnapshot } from '../app/ipc-events';
import type { DeviceSnapshotPayload } from '../app/ipc-events';

export type { DeviceSnapshotPayload } from '../app/ipc-events';

const disconnectedSnapshot = (): DeviceSnapshotPayload => ({
  connection_state: 'Disconnected',
  serial: '--',
  connection_label: '等待连接',
  model: '未检测到设备',
  android_version: '--',
  battery_level: '--',
  active_slot: '--',
  bootloader_state: '--',
  kernel_version: '--',
  verified_boot_state: '--',
});

const normalizeDeviceSnapshot = (value: unknown): DeviceSnapshotPayload => {
  if (!value || typeof value !== 'object') {
    return disconnectedSnapshot();
  }

  const raw = value as Record<string, unknown>;
  const text = (key: string, fallback: string): string =>
    typeof raw[key] === 'string' && (raw[key] as string).trim()
      ? (raw[key] as string)
      : fallback;

  return {
    connection_state: text('connection_state', 'Disconnected'),
    serial: text('serial', '--'),
    connection_label: text('connection_label', '等待连接'),
    model: text('model', '未检测到设备'),
    android_version: text('android_version', '--'),
    battery_level: text('battery_level', '--'),
    active_slot: text('active_slot', '--'),
    bootloader_state: text('bootloader_state', '--'),
    kernel_version: text('kernel_version', '--'),
    verified_boot_state: text('verified_boot_state', '--'),
  };
};

/** 后端因任务占用/刷新互斥而跳过一次刷新：这是调度结果，不是设备故障。 */
const isTransientRefreshSkip = (message: string): boolean =>
  message.includes('设备刷新已跳过') || message.includes('设备刷新正在进行中');

type OverviewPageProps = {
  snapshot?: DeviceSnapshotPayload | null;
  /** 是否有任务在跑。任务期间后端暂停设备发现，界面不应把暂停渲染成检测失败。 */
  busy?: boolean;
  /** 把本页读到的权威快照交回外壳，避免标题栏与设备面板各持一份旧状态。 */
  onSnapshot?: (snapshot: DeviceSnapshotPayload) => void;
};

export const OverviewPage: FC<OverviewPageProps> = ({ snapshot, busy = false, onSnapshot }) => {
  const initialSnapshot = snapshot ? normalizeDeviceSnapshot(snapshot) : disconnectedSnapshot();
  const [deviceSnapshot, setDeviceSnapshot] = useState<DeviceSnapshotPayload>(initialSnapshot);
  const deviceSnapshotRef = useRef(initialSnapshot);
  const snapshotRevisionRef = useRef(0);
  const busyRef = useRef(busy);
  const [errorText, setErrorText] = useState('');
  const [loading, setLoading] = useState(true);
  const [rebooting, setRebooting] = useState(false);

  busyRef.current = busy;

  const applySnapshot = (nextSnapshot: DeviceSnapshotPayload) => {
    deviceSnapshotRef.current = nextSnapshot;
    setDeviceSnapshot(nextSnapshot);
    onSnapshot?.(nextSnapshot);
  };

  const loadDevice = async () => {
    const requestRevision = snapshotRevisionRef.current;
    setLoading(true);
    setErrorText('');

    try {
      const response = await invoke<unknown>('device_refresh');
      if (requestRevision !== snapshotRevisionRef.current) {
        return;
      }

      applySnapshot(normalizeDeviceSnapshot(response));
    } catch (error) {
      const message = errorMessage(error, '设备检测失败');
      if (
        requestRevision !== snapshotRevisionRef.current ||
        isConnectedDeviceSnapshot(deviceSnapshotRef.current) ||
        // 任务进行中的刷新失败是「检测被暂停」，不是设备故障：
        // 保留上一次已知状态，不弹错误文案。
        busyRef.current ||
        isTransientRefreshSkip(message)
      ) {
        return;
      }

      setErrorText(message);
    } finally {
      setLoading(false);
    }
  };

  const reboot = async (command: 'device_reboot_system' | 'device_reboot_bootloader' | 'device_reboot_fastboot') => {
    setRebooting(true);
    setErrorText('');
    try {
      await invoke<void>(command);
    } catch (error) {
      setErrorText(errorMessage(error, '设备重启失败'));
    } finally {
      setRebooting(false);
    }
  };

  useEffect(() => {
    void loadDevice();
  }, []);

  useEffect(() => {
    if (snapshot) {
      snapshotRevisionRef.current += 1;
      const nextSnapshot = normalizeDeviceSnapshot(snapshot);
      deviceSnapshotRef.current = nextSnapshot;
      setDeviceSnapshot(nextSnapshot);
      setLoading(false);
      if (nextSnapshot.connection_state !== 'Error') {
        setErrorText('');
      }
    }
  }, [snapshot]);

  const canReboot =
    isConnectedDeviceSnapshot(deviceSnapshot) &&
    !loading &&
    !rebooting;
  const isConnected = isConnectedDeviceSnapshot(deviceSnapshot);
  // 「发现失败」（后端 Error 态：命令失败 / adb 与 fastboot 并存等）不等于
  // 「没有设备」——把它渲染成「未检测到设备」会让用户以为线没插好。
  // 与「任务暂停」一样，这是调度/工具结果，必须如实区分。
  const detectionFailed = deviceSnapshot.connection_state === 'Error';
  const deviceIndicatorClassName = `nw-device-indicator${isConnected ? ' is-connected' : ''}`;
  const deviceDetails = [
    ['当前槽位', deviceDetailValue(deviceSnapshot.active_slot), 'nw-overview-detail-slot'],
    ['引导加载器', deviceDetailValue(deviceSnapshot.bootloader_state), 'nw-overview-detail-bootloader'],
    ['系统版本', deviceDetailValue(deviceSnapshot.android_version), 'nw-overview-detail-system'],
    ['电池电量', deviceDetailValue(deviceSnapshot.battery_level), 'nw-overview-detail-battery'],
    ['内核版本', deviceDetailValue(deviceSnapshot.kernel_version), 'nw-overview-detail-kernel'],
    ['验证启动', deviceDetailValue(deviceSnapshot.verified_boot_state), 'nw-overview-detail-verified'],
  ] as const;

  return (
    <section className="nw-overview-page" aria-label="设备概览">
      <header className="nw-overview-heading">
        <div>
          <h1>设备概览</h1>
        </div>
        <p className="nw-overview-connection">
          <span className={deviceIndicatorClassName} aria-hidden="true" />当前连接状态
        </p>
      </header>

      {loading && !errorText ? <p className="nw-overview-load">正在检测设备...</p> : null}
      {busy && !loading && !errorText ? (
        <p className="nw-overview-paused">任务进行中，设备检测已暂停，任务结束后自动刷新。</p>
      ) : null}
      {errorText ? <p className="nw-error-text">{errorText}</p> : null}

      <section className="nw-overview-device-profile" aria-label="只读设备档案">
        <div className="nw-overview-identity">
          <p className="nw-page-eyebrow">
            {isConnected
              ? '已连接设备'
              : busy
                ? '检测暂停中'
                : detectionFailed
                  ? '设备检测失败'
                  : '未检测到设备'}
          </p>
          <p className="nw-overview-identity-label">设备身份</p>
          <strong>{detectionFailed ? '--' : deviceSnapshot.model}</strong>
          <p className="nw-overview-serial">{deviceSnapshot.serial}</p>
          <p className="nw-overview-connection-chip">
            <span className={deviceIndicatorClassName} aria-hidden="true" />
            {deviceSnapshot.connection_label}
          </p>
        </div>
        <dl className="nw-overview-details">
          {deviceDetails.map(([label, value, className]) => (
            <div className={`nw-overview-detail ${className}`} key={label}>
              <dt>{label}</dt>
              <dd className={value.length > 18 ? 'nw-overview-value-long' : undefined}>{value}</dd>
            </div>
          ))}
        </dl>
      </section>

      <header className="nw-overview-reboot-heading">
        <h2>启动控制</h2>
      </header>
      <section className="nw-overview-reboot-controls" aria-label="设备重启操作">
        <article>
          <div>
            <h3>重启设备</h3>
            <span>返回系统</span>
          </div>
          <button type="button" className="nw-test-reboot-system" aria-label="重启设备" onClick={() => void reboot('device_reboot_system')} disabled={!canReboot}>
            重启
          </button>
        </article>
        <article>
          <div>
            <h3>进入 Bootloader</h3>
            <span>维护引导环境</span>
          </div>
          <button type="button" aria-label="进入 Bootloader" onClick={() => void reboot('device_reboot_bootloader')} disabled={!canReboot}>
            进入
          </button>
        </article>
        <article className="nw-overview-fastboot-control">
          <div>
            <h3>进入 Fastboot</h3>
            <span>准备分区写入</span>
          </div>
          <button type="button" aria-label="进入 Fastboot" onClick={() => void reboot('device_reboot_fastboot')} disabled={!canReboot}>
            进入
          </button>
        </article>
      </section>
    </section>
  );
};

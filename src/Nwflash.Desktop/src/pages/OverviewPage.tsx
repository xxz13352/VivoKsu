import { errorMessage } from '../app/error';
import { FC, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { isConnectedDeviceSnapshot } from '../app/ipc-events';
import type { DeviceSnapshotPayload } from '../app/ipc-events';

export type { DeviceSnapshotPayload } from '../app/ipc-events';

const disconnectedSnapshot = (): DeviceSnapshotPayload => ({
  connection_state: 'Disconnected',
  serial: '--',
  connection_label: '等待连接',
  model: '未检测到设备',
  android_version: '--',
  battery_level: '--',
});

const normalizeDeviceSnapshot = (value: unknown): DeviceSnapshotPayload => {
  if (!value || typeof value !== 'object') {
    return disconnectedSnapshot();
  }

  const raw = value as Record<string, unknown>;
  return {
    connection_state:
      typeof raw.connection_state === 'string' ? raw.connection_state : 'Disconnected',
    serial: typeof raw.serial === 'string' && raw.serial.trim() ? raw.serial : '--',
    connection_label:
      typeof raw.connection_label === 'string' && raw.connection_label.trim()
        ? raw.connection_label
        : '等待连接',
    model: typeof raw.model === 'string' && raw.model.trim() ? raw.model : '未检测到设备',
    android_version:
      typeof raw.android_version === 'string' && raw.android_version.trim()
        ? raw.android_version
        : '--',
    battery_level:
      typeof raw.battery_level === 'string' && raw.battery_level.trim()
        ? raw.battery_level
        : '--',
  };
};

export const OverviewPage: FC<{ snapshot?: DeviceSnapshotPayload | null }> = ({ snapshot }) => {
  const initialSnapshot = snapshot ? normalizeDeviceSnapshot(snapshot) : disconnectedSnapshot();
  const [deviceSnapshot, setDeviceSnapshot] = useState<DeviceSnapshotPayload>(initialSnapshot);
  const deviceSnapshotRef = useRef(initialSnapshot);
  const snapshotRevisionRef = useRef(0);
  const [errorText, setErrorText] = useState('');
  const [loading, setLoading] = useState(true);
  const [rebooting, setRebooting] = useState(false);

  const loadDevice = async () => {
    const requestRevision = snapshotRevisionRef.current;
    setLoading(true);
    setErrorText('');

    try {
      const response = await invoke<unknown>('device_refresh');
      if (requestRevision !== snapshotRevisionRef.current) {
        return;
      }

      const nextSnapshot = normalizeDeviceSnapshot(response);
      deviceSnapshotRef.current = nextSnapshot;
      setDeviceSnapshot(nextSnapshot);
    } catch (error) {
      if (
        requestRevision !== snapshotRevisionRef.current ||
        isConnectedDeviceSnapshot(deviceSnapshotRef.current)
      ) {
        return;
      }

      setErrorText(errorMessage(error, '设备检测失败'));
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
  const deviceIndicatorClassName = `nw-device-indicator${isConnected ? ' is-connected' : ''}`;
  const deviceDetails = [
    ['当前槽位', '--', 'nw-overview-detail-slot'],
    ['引导加载器', '--', 'nw-overview-detail-bootloader'],
    ['系统版本', deviceSnapshot.android_version, 'nw-overview-detail-system'],
    ['内核版本', '--', 'nw-overview-detail-kernel'],
    ['版本信息', deviceSnapshot.battery_level, 'nw-overview-detail-firmware'],
    ['验证启动', '--', 'nw-overview-detail-verified'],
  ] as const;

  return (
    <section className="nw-overview-page" aria-label="设备概览">
      <header className="nw-overview-heading">
        <div>
          <p className="nw-page-eyebrow">DEVICE / OVERVIEW</p>
          <h1>设备概览</h1>
          <p className="nw-overview-subtitle">连接信息、引导状态与系统标识</p>
        </div>
        <p className="nw-overview-connection">
          <span className={deviceIndicatorClassName} aria-hidden="true" />当前连接状态
        </p>
      </header>

      {loading && !errorText ? <p className="nw-overview-load">正在检测设备...</p> : null}
      {errorText ? <p className="nw-error-text">{errorText}</p> : null}

      <section className="nw-overview-device-profile" aria-label="只读设备档案">
        <div className="nw-overview-identity">
          <p className="nw-page-eyebrow">{isConnected ? '已连接设备' : '未检测到设备'}</p>
          <p className="nw-overview-identity-label">设备身份</p>
          <strong>{deviceSnapshot.model}</strong>
          <p className="nw-overview-serial">SERIAL&nbsp;&nbsp;{deviceSnapshot.serial}</p>
          <p className="nw-overview-connection-chip">
            <span className={deviceIndicatorClassName} aria-hidden="true" />
            {deviceSnapshot.connection_label}
          </p>
        </div>
        <dl className="nw-overview-details">
          {deviceDetails.map(([label, value, className]) => (
            <div className={`nw-overview-detail ${className}`} key={label}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
        <footer>
          <span>设备参数由 ADB / Fastboot 会话实时读取</span>
          <span>READ-ONLY DEVICE PROFILE</span>
        </footer>
      </section>

      <header className="nw-overview-reboot-heading">
        <h2>启动控制</h2>
        <p>REBOOT CONTROL</p>
      </header>
      <section className="nw-overview-reboot-controls" aria-label="设备重启操作">
        <article>
          <div>
            <p>ANDROID SYSTEM</p>
            <h3>重启设备</h3>
            <span>返回系统</span>
          </div>
          <button type="button" className="nw-test-reboot-system" aria-label="重启设备" onClick={() => void reboot('device_reboot_system')} disabled={!canReboot}>
            重启
          </button>
        </article>
        <article>
          <div>
            <p>BOOTLOADER</p>
            <h3>进入 Bootloader</h3>
            <span>维护引导环境</span>
          </div>
          <button type="button" aria-label="进入 Bootloader" onClick={() => void reboot('device_reboot_bootloader')} disabled={!canReboot}>
            进入
          </button>
        </article>
        <article className="nw-overview-fastboot-control">
          <div>
            <p>FASTBOOT</p>
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

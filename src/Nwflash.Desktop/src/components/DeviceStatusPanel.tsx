import { FC } from 'react';
import { isConnectedDeviceSnapshot } from '../app/ipc-events';
import type { DeviceSnapshotPayload } from '../app/ipc-events';

type DeviceStatusPanelProps = {
  deviceSnapshot?: DeviceSnapshotPayload | null;
};

export const DeviceStatusPanel: FC<DeviceStatusPanelProps> = ({
  deviceSnapshot = null,
}) => {
  const isConnected = isConnectedDeviceSnapshot(deviceSnapshot);
  const deviceIndicatorClassName = `nw-device-indicator${isConnected ? ' is-connected' : ''}`;

  return (
    <section className="nw-device-status-panel">
      <div className="nw-device-status-eyebrow">DEVICE STATUS</div>
      <strong className="nw-device-status-title">
        {deviceSnapshot?.model || '未检测到设备'}
      </strong>
      <div className="nw-device-status-connection">
        <span className={deviceIndicatorClassName} aria-hidden="true" />
        {deviceSnapshot?.connection_label || '等待连接'}
      </div>
      <div className="nw-device-status-details">
        <span>SN&nbsp;&nbsp;{deviceSnapshot?.serial || '--'}</span>
        <span>OS&nbsp;&nbsp;{deviceSnapshot?.android_version || '--'}&nbsp;&nbsp; BAT&nbsp;&nbsp;{deviceSnapshot?.battery_level || '--'}</span>
      </div>
    </section>
  );
};

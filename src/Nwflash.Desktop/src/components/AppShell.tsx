import { FC, ReactNode } from 'react';
import { AppPageId, PageNavGroup } from '../app/pageManifest';
import { isConnectedDeviceSnapshot } from '../app/ipc-events';
import type { DeviceSnapshotPayload, OperationSnapshotPayload } from '../app/ipc-events';
import { BusyOperationItem, isShellBusy, resolveProgressText } from '../app/window-state';
import { DeviceStatusPanel } from './DeviceStatusPanel';
import { ModalLayer } from './ModalLayer';
import { OperationLogPanel } from './OperationLogPanel';
import { OperationProgressPanel } from './OperationProgressPanel';
import { Sidebar } from './Sidebar';
import logoUrl from '../assets/logo.jpg';

type AppShellProps = {
  appTitle: string;
  navGroups: ReadonlyArray<PageNavGroup>;
  currentPage: AppPageId;
  onSelectPage: (page: AppPageId) => void;
  operations: ReadonlyArray<BusyOperationItem>;
  operationSnapshot?: OperationSnapshotPayload | null;
  username: string;
  currentTime: string;
  isLoggedIn: boolean;
  deviceSnapshot?: DeviceSnapshotPayload | null;
  onRefreshDevice?: () => void;
  onLogout: () => void;
  isBusyAction?: boolean;
  onMinimize?: () => void;
  onMaximize?: () => void;
  onClose?: () => void;
  modalOpen?: boolean;
  modalTitle?: string;
  modalChildren?: ReactNode;
  children: ReactNode;
};

export const AppShell: FC<AppShellProps> = ({
  appTitle,
  navGroups,
  currentPage,
  onSelectPage,
  operations,
  operationSnapshot,
  username,
  currentTime,
  isLoggedIn,
  deviceSnapshot = null,
  onRefreshDevice,
  onLogout,
  isBusyAction = false,
  onMinimize,
  onMaximize,
  onClose,
  modalOpen,
  modalTitle,
  modalChildren,
  children,
}) => {
  const progressText = resolveProgressText(operations);
  const isBusy = isShellBusy(operations) || Boolean(isBusyAction);
  const deviceIndicatorClassName = `nw-device-indicator${
    isConnectedDeviceSnapshot(deviceSnapshot) ? ' is-connected' : ''
  }`;

  return (
    <div className="nw-shell">
      <header className="nw-titlebar" data-tauri-drag-region>
        <div className="nw-titlebar-brand" data-tauri-drag-region>
          <img src={logoUrl} alt="" data-tauri-drag-region />
          <div data-tauri-drag-region>
            <strong data-tauri-drag-region>{appTitle}</strong>
            <span data-tauri-drag-region>奶蛙FLASH · ANDROID DEVICE WORKBENCH</span>
          </div>
        </div>
        <div className="nw-titlebar-connection" data-tauri-drag-region>
          <span className={deviceIndicatorClassName} aria-hidden="true" data-tauri-drag-region />
          {deviceSnapshot?.connection_label || '等待连接'}
        </div>
        <button
          type="button"
          className="nw-titlebar-refresh"
          onClick={onRefreshDevice}
          aria-label="刷新设备"
          disabled={isBusy}
        >
          刷新设备
        </button>
        <span className="nw-titlebar-divider" aria-hidden="true" />
        <div className="nw-titlebar-controls">
          <button type="button" onClick={onMinimize} aria-label="最小化">
            —
          </button>
          <button type="button" onClick={onMaximize} aria-label="最大化">
            ❐
          </button>
          <button type="button" onClick={onClose} aria-label="关闭">
            ✕
          </button>
        </div>
      </header>
      <div className="nw-shell-layout">
        <Sidebar
          navGroups={navGroups}
          currentPage={currentPage}
          onSelectPage={onSelectPage}
          username={username}
          currentTime={currentTime}
          logoutDisabled={isBusy}
          onLogout={onLogout}
        />
        <section className="nw-page-card">{children}</section>
        <section className="nw-status-rail">
          <DeviceStatusPanel deviceSnapshot={deviceSnapshot} />
          <OperationProgressPanel progressText={progressText} />
          <OperationLogPanel operationSnapshot={operationSnapshot} />
        </section>
      </div>
      <ModalLayer isVisible={Boolean(modalOpen)} title={modalTitle || null}>
        {modalChildren}
      </ModalLayer>
    </div>
  );
};

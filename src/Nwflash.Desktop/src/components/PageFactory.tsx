import { FC } from 'react';
import { AppPageId } from '../app/pageManifest';
import type { DeviceSnapshotPayload, OperationSnapshotPayload } from '../app/ipc-events';
import { FirmwareExtractPage } from '../pages/FirmwareExtractPage';
import { FileManagerPage } from '../pages/FileManagerPage';
import { LineFlashPage } from '../pages/LineFlashPage';
import { MirrorPage } from '../pages/MirrorPage';
import { OnlineStatusPage } from '../pages/OnlineStatusPage';
import { OperationLogPage } from '../pages/OperationLogPage';
import { OverviewPage } from '../pages/OverviewPage';
import { QuickFlashPage } from '../pages/QuickFlashPage';
import { RootPage } from '../pages/RootPage';
import { SafeFlashPage } from '../pages/SafeFlashPage';
import { SoftwarePage } from '../pages/SoftwarePage';

export const PageFactory: FC<{
  page: AppPageId;
  deviceSnapshot?: DeviceSnapshotPayload | null;
  operationSnapshot?: OperationSnapshotPayload | null;
  onDeviceSnapshot?: (snapshot: DeviceSnapshotPayload) => void;
}> = ({ page, deviceSnapshot, operationSnapshot, onDeviceSnapshot }) => {
  switch (page) {
    case 'Overview':
      return (
        <OverviewPage
          snapshot={deviceSnapshot}
          busy={Boolean(operationSnapshot?.isBusy)}
          onSnapshot={onDeviceSnapshot}
        />
      );
    case 'QuickFlash':
      return <QuickFlashPage operationSnapshot={operationSnapshot} />;
    case 'Mirror':
      return <MirrorPage deviceSnapshot={deviceSnapshot} />;
    case 'FileManager':
      return <FileManagerPage deviceSnapshot={deviceSnapshot} />;
    case 'LineFlash':
      return <LineFlashPage operationSnapshot={operationSnapshot} />;
    case 'FirmwareExtract':
      return <FirmwareExtractPage />;
    case 'SafeFlash':
      return <SafeFlashPage deviceSnapshot={deviceSnapshot} operationSnapshot={operationSnapshot} />;
    case 'Root':
      return <RootPage />;
    case 'Online':
      return <OnlineStatusPage />;
    case 'OperationLog':
      return <OperationLogPage />;
    case 'Software':
      return <SoftwarePage />;
    default:
      return (
        <OverviewPage
          snapshot={deviceSnapshot}
          busy={Boolean(operationSnapshot?.isBusy)}
          onSnapshot={onDeviceSnapshot}
        />
      );
  }
};

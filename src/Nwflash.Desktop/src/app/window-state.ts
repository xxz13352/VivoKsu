import { AppPageId } from './pageManifest';

export const APP_OPERATION_LABELS = {
  quick: '快速刷写',
  lineFlash: '可视刷写',
  safeFlash: 'VIVO 线刷',
  firmwareExtract: '固件提取',
  device: '设备操作',
} as const;

export const PROGRESS_CHANNEL_ORDER = [
  'quick',
  'lineFlash',
  'safeFlash',
  'firmwareExtract',
  'device',
] as const;

export type BusyOperationKind = (typeof PROGRESS_CHANNEL_ORDER)[number];

export interface BusyOperationItem {
  kind: BusyOperationKind;
  message: string;
}

export interface AppWindowState {
  currentPage: AppPageId;
  isLoggedIn: boolean;
  operations: ReadonlyArray<BusyOperationItem>;
  username: string;
  currentTime: string;
}

export const NO_OPERATION_TEXT = '无进行中的操作';

export const resolveProgressText = (operations: ReadonlyArray<BusyOperationItem>): string => {
  for (const kind of PROGRESS_CHANNEL_ORDER) {
    const match = operations.find((operation) => operation.kind === kind);
    if (match) {
      return `${APP_OPERATION_LABELS[match.kind]}：${match.message}`;
    }
  }

  return NO_OPERATION_TEXT;
};

export const isShellBusy = (operations: ReadonlyArray<BusyOperationItem>): boolean =>
  operations.length > 0;

export const blankWindowState = (page: AppPageId, time = ''): AppWindowState => ({
  currentPage: page,
  isLoggedIn: true,
  operations: [],
  username: 'admin',
  currentTime: time || new Date().toLocaleTimeString('zh-CN', { hour12: false }),
});

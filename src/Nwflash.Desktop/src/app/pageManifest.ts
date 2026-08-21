export type AppPageId =
  | 'Overview'
  | 'QuickFlash'
  | 'Mirror'
  | 'FileManager'
  | 'LineFlash'
  | 'FirmwareExtract'
  | 'SafeFlash'
  | 'Root'
  | 'Online'
  | 'Software'
  | 'OperationLog';

export type PageNavGroup = {
  title: string;
  pages: ReadonlyArray<{ id: AppPageId; label: string }>;
};

export const NWFLASH_APP_PAGES: ReadonlyArray<PageNavGroup> = [
  {
    title: '设备',
    pages: [
      { id: 'Overview', label: '设备概览' },
      { id: 'FileManager', label: '文件管理' },
      { id: 'Mirror', label: 'ADB 投屏' },
    ],
  },
  {
    title: '刷机',
    pages: [
      { id: 'QuickFlash', label: '快速刷写' },
      { id: 'LineFlash', label: '可视刷写' },
      { id: 'SafeFlash', label: 'VIVO 线刷' },
      { id: 'FirmwareExtract', label: '固件提取' },
      { id: 'Root', label: 'Vivo ROOT' },
    ],
  },
  {
    title: '状态',
    pages: [
      { id: 'Online', label: '在线状态' },
      { id: 'Software', label: '软件' },
    ],
  },
];

export const PAGE_TITLES: Record<AppPageId, string> = {
  Overview: '设备概览',
  QuickFlash: '快速刷写',
  Mirror: 'ADB 投屏',
  FileManager: '文件管理',
  LineFlash: '可视刷写',
  FirmwareExtract: '固件提取',
  SafeFlash: 'VIVO 线刷',
  Root: 'Vivo ROOT',
  Online: '在线状态',
  Software: '软件',
  OperationLog: '操作日志',
};

import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { FileManagerPage } from './FileManagerPage';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
  save: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';

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
    throw new Error('timeout waiting for async assertion');
  }
};

const renderFileManager = () => {
  flushSync(() => {
    root.render(<FileManagerPage />);
  });
};

describe('FileManagerPage', () => {
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

  test('下载设备文件使用保存对话框选择的精确路径且不提交串号', async () => {
    (save as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\update.zip');
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce([{ name: 'update.zip', full_path: '/sdcard/update.zip', is_directory: false, size_bytes: 512 }])
      .mockResolvedValueOnce(undefined);
    renderFileManager();

    (host.querySelector('.nw-test-file-refresh') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('update.zip') ?? false);
    (host.querySelector('.nw-test-file-download') as HTMLButtonElement).click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);
    expect(save).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenNthCalledWith(2, 'files_download', {
      remotePath: '/sdcard/update.zip',
      destinationPath: 'C:\\chosen\\update.zip',
    });
    expect(host.textContent).not.toContain('C:\\chosen\\update.zip');
  });

  test('选中文件后工具栏传出按钮执行该文件的下载', async () => {
    (save as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\update.zip');
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce([
        { name: 'update.zip', full_path: '/sdcard/update.zip', is_directory: false, size_bytes: 512 },
      ])
      .mockResolvedValueOnce(undefined);
    renderFileManager();

    (host.querySelector('.nw-test-file-refresh') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('update.zip') ?? false);
    (host.querySelector('.nw-test-file-entry') as HTMLButtonElement).click();
    await waitUntil(() => !(host.querySelector('.nw-test-file-toolbar-download') as HTMLButtonElement).disabled);

    const download = host.querySelector('.nw-test-file-toolbar-download') as HTMLButtonElement | null;
    expect(download).not.toBeNull();
    expect(download?.disabled).toBe(false);
    download?.click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);
    expect(invoke).toHaveBeenNthCalledWith(2, 'files_download', {
      remotePath: '/sdcard/update.zip',
      destinationPath: 'C:\\chosen\\update.zip',
    });
  });

  test('选中条目后工具栏删除按钮保留确认步骤', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      { name: 'old.zip', full_path: '/sdcard/old.zip', is_directory: false, size_bytes: 512 },
    ]);
    renderFileManager();

    (host.querySelector('.nw-test-file-refresh') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('old.zip') ?? false);
    (host.querySelector('.nw-test-file-entry') as HTMLButtonElement).click();
    await waitUntil(() => !(host.querySelector('.nw-test-file-toolbar-delete') as HTMLButtonElement).disabled);

    const remove = host.querySelector('.nw-test-file-toolbar-delete') as HTMLButtonElement | null;
    expect(remove).not.toBeNull();
    expect(remove?.disabled).toBe(false);
    remove?.click();

    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  test('上传只将对话框选择的文件发送到当前远端目录', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\update.zip');
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([]);
    renderFileManager();

    (host.querySelector('.nw-test-file-upload') as HTMLButtonElement).click();
    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);

    expect(invoke).toHaveBeenCalledWith('files_upload', {
      sourcePath: 'C:\\chosen\\update.zip',
      remoteDirectory: '/sdcard',
    });
    expect(host.textContent).not.toContain('C:\\chosen\\update.zip');
  });

  test('上传完成后重新读取当前设备目录', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\update.zip');
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([]);
    renderFileManager();

    (host.querySelector('.nw-test-file-upload') as HTMLButtonElement).click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);
    expect(invoke).toHaveBeenNthCalledWith(1, 'files_upload', {
      sourcePath: 'C:\\chosen\\update.zip',
      remoteDirectory: '/sdcard',
    });
    expect(invoke).toHaveBeenNthCalledWith(2, 'files_list', { remoteDirectory: '/sdcard' });
  });

  test('安装仅将对话框选中的 APK 路径传递给受限命令', async () => {
    (open as unknown as ReturnType<typeof vi.fn>).mockResolvedValue('C:\\chosen\\manager.apk');
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);
    renderFileManager();

    (host.querySelector('.nw-test-file-install-apk') as HTMLButtonElement).click();
    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 1);

    expect(invoke).toHaveBeenCalledWith('files_install_apk', { apkPath: 'C:\\chosen\\manager.apk' });
    expect(host.textContent).not.toContain('C:\\chosen\\manager.apk');
  });

  test('刷新设备目录时仅调用受限 files_list 并呈现 Rust 返回的条目', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([
      { name: 'Download', full_path: '/sdcard/Download', is_directory: true, size_bytes: 4096 },
      { name: 'update.zip', full_path: '/sdcard/update.zip', is_directory: false, size_bytes: 2048 },
    ]);
    renderFileManager();

    (host.querySelector('.nw-test-file-refresh') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('update.zip') ?? false);
    expect(invoke).toHaveBeenCalledWith('files_list', { remoteDirectory: '/sdcard' });
    expect(host.textContent).toContain('Download');
    expect(host.textContent).toContain('2.0 KB');
  });

  test('使用 WPF 的设备文件工作台结构呈现空目录状态', () => {
    renderFileManager();

    expect(host.querySelector('.nw-file-manager-page')).not.toBeNull();
    expect(host.querySelector('.nw-file-manager-workbench')).not.toBeNull();
    expect(host.querySelector('.nw-file-manager-toolbar')).not.toBeNull();
    expect(host.querySelector('.nw-file-manager-directory-summary')).not.toBeNull();
    expect(host.querySelector('.nw-file-manager-entry-grid')).not.toBeNull();
    expect(host.querySelector('.nw-file-manager-log')).not.toBeNull();
    expect(host.textContent).toContain('ADB / DEVICE FILES');
    expect(host.textContent).toContain('以设备目录为中心的可视化传输工作台');
    expect(host.textContent).toContain('传入文件到手机');
    expect(host.textContent).toContain('传出文件到电脑');
    expect(host.textContent).toContain('文件日志');
    expect(host.textContent).toContain('ADB 文件传输');
  });

  test('进入远端目录后读取目标路径', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce([
        { name: 'Download', full_path: '/sdcard/Download', is_directory: true, size_bytes: 4096 },
      ])
      .mockResolvedValueOnce([]);
    renderFileManager();

    (host.querySelector('.nw-test-file-refresh') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-file-entry') !== null);
    (host.querySelector('.nw-test-file-entry') as HTMLButtonElement).click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 2);
    expect(invoke).toHaveBeenLastCalledWith('files_list', { remoteDirectory: '/sdcard/Download' });
    await waitUntil(() => host.textContent?.includes('/sdcard/Download') ?? false);
    expect(host.textContent).toContain('/sdcard/Download');
  });

  test('返回设备根目录后禁用上级按钮', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    renderFileManager();

    const up = host.querySelector('.nw-test-file-remote-up') as HTMLButtonElement;
    expect(up.disabled).toBe(false);
    up.click();

    await waitUntil(() => host.textContent?.includes('/') ?? false);
    expect(invoke).toHaveBeenCalledWith('files_list', { remoteDirectory: '/' });
    expect((host.querySelector('.nw-test-file-remote-up') as HTMLButtonElement).disabled).toBe(true);
  });

  test('删除设备文件必须捕获目标并在确认后才调用受限命令', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce([
        { name: 'old file.zip', full_path: '/sdcard/old file.zip', is_directory: false, size_bytes: 512 },
      ])
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([]);
    renderFileManager();

    (host.querySelector('.nw-test-file-refresh') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('old file.zip') ?? false);
    (host.querySelector('.nw-test-file-delete') as HTMLButtonElement).click();

    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    const dialog = host.querySelector('[role="dialog"]') as HTMLElement;
    expect(dialog.textContent).toContain('old file.zip');
    expect(invoke).not.toHaveBeenCalledWith('files_delete', expect.anything());

    (host.querySelector('.nw-test-file-delete-confirm') as HTMLButtonElement).click();
    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length === 3);
    expect(invoke).toHaveBeenNthCalledWith(2, 'files_delete', {
      remotePath: '/sdcard/old file.zip',
    });
    expect(invoke).toHaveBeenNthCalledWith(3, 'files_list', { remoteDirectory: '/sdcard' });
  });
});

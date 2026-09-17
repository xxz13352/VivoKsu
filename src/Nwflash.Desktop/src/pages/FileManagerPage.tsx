import { errorMessage } from '../app/error';
import { FC, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import type { DeviceSnapshotPayload } from '../app/ipc-events';
import { ModalLayer } from '../components/ModalLayer';

type DeviceFileEntry = {
  name: string;
  full_path: string;
  is_directory: boolean;
  size_bytes: number;
};

const formatSize = (sizeBytes: number) => {
  if (sizeBytes < 1024) return `${sizeBytes} B`;
  if (sizeBytes < 1024 * 1024) return `${(sizeBytes / 1024).toFixed(1)} KB`;
  return `${(sizeBytes / (1024 * 1024)).toFixed(1)} MB`;
};

const parentDirectory = (path: string) => {
  if (path === '/') return '/';
  const trimmed = path.replace(/\/+$/, '');
  const index = trimmed.lastIndexOf('/');
  return index <= 0 ? '/' : trimmed.slice(0, index);
};

type FileOperationKind = 'download' | 'upload' | 'install' | 'delete';

type ActiveFileOperation = {
  id: number;
  kind: FileOperationKind;
};

const isCancellationError = (error: unknown): boolean => {
  const message = errorMessage(error, '');
  return /(?:操作已取消|已取消|用户取消|cancelled|canceled)/i.test(message);
};

export const FileManagerPage: FC<{
  deviceSnapshot?: DeviceSnapshotPayload | null;
}> = ({ deviceSnapshot }) => {
  const [currentRemotePath, setCurrentRemotePath] = useState('/sdcard');
  const [remoteEntries, setRemoteEntries] = useState<readonly DeviceFileEntry[]>([]);
  const [selectedRemote, setSelectedRemote] = useState<DeviceFileEntry | null>(null);
  const [isRefreshingRemote, setIsRefreshingRemote] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<DeviceFileEntry | null>(null);
  const [operationStatus, setOperationStatus] = useState('');
  const [errorText, setErrorText] = useState('');
  const [activeOperation, setActiveOperation] = useState<ActiveFileOperation | null>(null);
  const [isCanceling, setIsCanceling] = useState(false);
  const operationSequence = useRef(0);
  const activeOperationRef = useRef<ActiveFileOperation | null>(null);
  const cancelRequestedRef = useRef(false);
  const hasAdbConnection = deviceSnapshot?.connection_state === 'AdbConnected';
  const fileOperationBusy = activeOperation !== null;
  const connectionLabel = hasAdbConnection
    ? deviceSnapshot?.connection_label || '设备已连接'
    : deviceSnapshot?.connection_state === 'FastbootConnected'
      ? 'Fastboot 模式，文件管理不可用'
      : '等待连接';

  const refreshRemote = async (path = currentRemotePath) => {
    if (!hasAdbConnection || activeOperationRef.current) return;
    setErrorText('');
    setIsRefreshingRemote(true);
    try {
      const entries = await invoke<readonly DeviceFileEntry[]>('files_list', { remoteDirectory: path });
      setCurrentRemotePath(path);
      setRemoteEntries(Array.isArray(entries) ? entries : []);
    } catch (error) {
      setErrorText(errorMessage(error, '读取设备目录失败'));
    } finally {
      setIsRefreshingRemote(false);
    }
  };

  const beginFileOperation = (kind: FileOperationKind): number | null => {
    if (activeOperationRef.current) {
      return null;
    }
    const operation = { id: ++operationSequence.current, kind };
    activeOperationRef.current = operation;
    cancelRequestedRef.current = false;
    setActiveOperation(operation);
    setIsCanceling(false);
    return operation.id;
  };

  const finishFileOperation = (operationId: number) => {
    if (activeOperationRef.current?.id !== operationId) {
      return;
    }
    activeOperationRef.current = null;
    cancelRequestedRef.current = false;
    setActiveOperation(null);
    setIsCanceling(false);
  };

  const runFileOperation = async (
    kind: FileOperationKind,
    action: () => Promise<void>,
    failureFallback: string,
  ): Promise<boolean> => {
    const operationId = beginFileOperation(kind);
    if (operationId === null) {
      return false;
    }
    setErrorText('');
    try {
      await action();
      if (activeOperationRef.current?.id === operationId) {
        setErrorText('');
      }
      return true;
    } catch (error) {
      if (activeOperationRef.current?.id !== operationId) {
        return false;
      }
      if (isCancellationError(error)) {
        setErrorText('');
      } else {
        setErrorText(errorMessage(error, failureFallback));
      }
      return false;
    } finally {
      finishFileOperation(operationId);
    }
  };

  const cancelFileOperation = async () => {
    const operation = activeOperationRef.current;
    if (!operation || cancelRequestedRef.current || !hasAdbConnection) {
      return;
    }
    cancelRequestedRef.current = true;
    setIsCanceling(true);
    setErrorText('');
    try {
      await invoke('operation_cancel');
    } catch (error) {
      if (activeOperationRef.current?.id !== operation.id) {
        return;
      }
      cancelRequestedRef.current = false;
      setIsCanceling(false);
      setErrorText(errorMessage(error, '停止文件操作失败'));
    }
  };

  const downloadEntry = async (entry: DeviceFileEntry) => {
    if (!hasAdbConnection || activeOperationRef.current) return;
    const destinationPath = await save({ defaultPath: entry.name });
    if (typeof destinationPath !== 'string') return;
    await runFileOperation(
      'download',
      async () => {
        await invoke('files_download', {
          remotePath: entry.full_path,
          destinationPath,
          // 远端文件大小用于下载进度（临时文件字节数/总字节）；目录或未知
          // 大小传 0，后端保持无进度的原有行为。
          remoteSize: entry.is_directory ? 0 : entry.size_bytes,
        });
      },
      '下载设备文件失败',
    );
  };

  const uploadFile = async () => {
    if (!hasAdbConnection || activeOperationRef.current) return;
    const sourcePath = await open({ multiple: false, directory: false });
    if (typeof sourcePath !== 'string') return;
    const completed = await runFileOperation(
      'upload',
      async () => {
        await invoke('files_upload', { sourcePath, remoteDirectory: currentRemotePath });
      },
      '上传设备文件失败',
    );
    if (completed) {
      await refreshRemote();
    }
  };

  const installApk = async () => {
    if (!hasAdbConnection || activeOperationRef.current) return;
    const apkPath = await open({
      multiple: false,
      directory: false,
      filters: [{ name: 'Android APK', extensions: ['apk'] }],
    });
    if (typeof apkPath !== 'string') return;
    await runFileOperation(
      'install',
      async () => {
        await invoke('files_install_apk', { apkPath });
      },
      '安装 APK 失败',
    );
  };

  const confirmDelete = async () => {
    if (!hasAdbConnection || activeOperationRef.current || !pendingDelete) return;
    const target = pendingDelete;
    const completed = await runFileOperation(
      'delete',
      async () => {
        await invoke('files_delete', { remotePath: target.full_path });
      },
      '删除设备文件失败',
    );
    if (completed) {
      setPendingDelete(null);
      setSelectedRemote((selected) => selected?.full_path === target.full_path ? null : selected);
      await refreshRemote();
    }
  };

  return (
    <section className="nw-file-manager-page" aria-label="文件管理">
      <header className="nw-file-manager-heading">
        <div>
          <h1>文件管理</h1>
        </div>
        <p className="nw-file-manager-connection"><span aria-hidden="true" />{connectionLabel}</p>
      </header>

      <section className="nw-file-manager-workbench" aria-label="设备文件">
        <div className="nw-file-manager-toolbar">
          <button
            type="button"
            className="nw-test-file-refresh nw-file-manager-primary-action"
            disabled={!hasAdbConnection || isRefreshingRemote || fileOperationBusy}
            onClick={() => void refreshRemote()}
          >
            加载目录
          </button>
          <button
            type="button"
            className="nw-test-file-upload"
            disabled={!hasAdbConnection || isRefreshingRemote || fileOperationBusy}
            onClick={() => void uploadFile()}
          >
            传入文件到手机
          </button>
          <button
            type="button"
            className="nw-test-file-toolbar-download"
            disabled={!hasAdbConnection || fileOperationBusy || !selectedRemote || selectedRemote.is_directory || isRefreshingRemote}
            onClick={() => selectedRemote && void downloadEntry(selectedRemote)}
          >
            传出文件到电脑
          </button>
          <button
            type="button"
            disabled={!hasAdbConnection || isRefreshingRemote || fileOperationBusy}
            onClick={() => void refreshRemote()}
          >
            刷新目录
          </button>
          <button
            type="button"
            className="nw-test-file-remote-up"
            disabled={!hasAdbConnection || currentRemotePath === '/' || isRefreshingRemote || fileOperationBusy}
            onClick={() => void refreshRemote(parentDirectory(currentRemotePath))}
          >
            返回上级
          </button>
          <button
            type="button"
            className="nw-test-file-install-apk"
            disabled={!hasAdbConnection || isRefreshingRemote || fileOperationBusy}
            onClick={() => void installApk()}
          >
            安装 APK
          </button>
          <button
            type="button"
            className="nw-test-file-toolbar-delete"
            disabled={!hasAdbConnection || fileOperationBusy || !selectedRemote || isRefreshingRemote}
            onClick={() => selectedRemote && setPendingDelete(selectedRemote)}
          >
            删除
          </button>
          <button
            type="button"
            className="nw-test-file-cancel"
            aria-label="停止操作"
            aria-busy={isCanceling}
            disabled={!hasAdbConnection || !fileOperationBusy || isCanceling}
            onClick={() => void cancelFileOperation()}
          >
            {isCanceling ? '正在停止…' : '停止操作'}
          </button>
        </div>
        <div className="nw-file-manager-directory-summary">
          <span>设备目录</span>
          <strong>{currentRemotePath}</strong>
          <span>{remoteEntries.length} 个条目</span>
        </div>
        {errorText ? <p className="nw-error-text">{errorText}</p> : null}
        <ul className="nw-file-manager-entry-grid">
          {remoteEntries.map((entry) => (
            <li className="nw-file-manager-entry" key={entry.full_path}>
              <button
                type="button"
                className="nw-test-file-entry"
                aria-pressed={!entry.is_directory && selectedRemote?.full_path === entry.full_path}
                disabled={!hasAdbConnection || isRefreshingRemote || fileOperationBusy}
                onClick={() => {
                  if (entry.is_directory) {
                    void refreshRemote(entry.full_path);
                    return;
                  }
                  setSelectedRemote(entry);
                }}
              >
                <span className={`nw-file-manager-entry-icon${entry.is_directory ? '' : ' is-file'}`} aria-hidden="true">
                  {entry.is_directory ? '□' : '▤'}
                </span>
                <strong>{entry.name}</strong>
                <span>{entry.is_directory ? '' : formatSize(entry.size_bytes)}</span>
              </button>
              <div className="nw-file-manager-entry-actions">
                <button
                  type="button"
                  className="nw-test-file-download"
                  disabled={!hasAdbConnection || entry.is_directory || isRefreshingRemote || fileOperationBusy}
                  onClick={() => void downloadEntry(entry)}
                >
                  下载
                </button>
                <button
                  type="button"
                  className="nw-test-file-delete"
                  disabled={!hasAdbConnection || isRefreshingRemote || fileOperationBusy}
                  onClick={() => setPendingDelete(entry)}
                >
                  删除
                </button>
              </div>
            </li>
          ))}
        </ul>

        <footer>
          <span>{connectionLabel}</span>
          <span>ADB 文件传输</span>
        </footer>
      </section>

      <ModalLayer
        isVisible={pendingDelete !== null}
        title="确认删除"
        onClose={() => setPendingDelete(null)}
      >
        <p>确定删除“{pendingDelete?.name}”吗？此操作无法恢复。</p>
        <div className="nw-driver-dialog-actions">
          <button type="button" onClick={() => setPendingDelete(null)}>
            取消
          </button>
          <button
            type="button"
            className="nw-test-file-delete-confirm nw-dialog-danger"
            disabled={!hasAdbConnection}
            onClick={() => void confirmDelete()}
          >
            删除
          </button>
        </div>
      </ModalLayer>
    </section>
  );
};

import { errorMessage } from '../app/error';
import { FC, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
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

export const FileManagerPage: FC = () => {
  const [currentRemotePath, setCurrentRemotePath] = useState('/sdcard');
  const [remoteEntries, setRemoteEntries] = useState<readonly DeviceFileEntry[]>([]);
  const [selectedRemote, setSelectedRemote] = useState<DeviceFileEntry | null>(null);
  const [isRefreshingRemote, setIsRefreshingRemote] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<DeviceFileEntry | null>(null);
  const [operationStatus, setOperationStatus] = useState('');
  const [errorText, setErrorText] = useState('');

  const refreshRemote = async (path = currentRemotePath) => {
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

  const downloadEntry = async (entry: DeviceFileEntry) => {
    const destinationPath = await save({ defaultPath: entry.name });
    if (typeof destinationPath !== 'string') return;
    setErrorText('');
    setOperationStatus('');
    try {
      await invoke('files_download', {
        remotePath: entry.full_path,
        destinationPath,
      });
      setOperationStatus('下载已完成');
    } catch (error) {
      setErrorText(errorMessage(error, '下载设备文件失败'));
    }
  };

  const uploadFile = async () => {
    const sourcePath = await open({ multiple: false, directory: false });
    if (typeof sourcePath !== 'string') return;
    setErrorText('');
    setOperationStatus('');
    try {
      await invoke('files_upload', { sourcePath, remoteDirectory: currentRemotePath });
      setOperationStatus('上传已完成');
      await refreshRemote();
    } catch (error) {
      setErrorText(errorMessage(error, '上传设备文件失败'));
    }
  };

  const installApk = async () => {
    const apkPath = await open({
      multiple: false,
      directory: false,
      filters: [{ name: 'Android APK', extensions: ['apk'] }],
    });
    if (typeof apkPath !== 'string') return;
    setErrorText('');
    setOperationStatus('');
    try {
      await invoke('files_install_apk', { apkPath });
      setOperationStatus('APK 安装已完成');
    } catch (error) {
      setErrorText(errorMessage(error, '安装 APK 失败'));
    }
  };

  const confirmDelete = async () => {
    if (!pendingDelete) return;
    const target = pendingDelete;
    setErrorText('');
    setOperationStatus('');
    try {
      await invoke('files_delete', { remotePath: target.full_path });
      setPendingDelete(null);
      setSelectedRemote((selected) => selected?.full_path === target.full_path ? null : selected);
      setOperationStatus('删除已完成');
      await refreshRemote();
    } catch (error) {
      setErrorText(errorMessage(error, '删除设备文件失败'));
    }
  };

  return (
    <section className="nw-file-manager-page" aria-label="文件管理">
      <header className="nw-file-manager-heading">
        <div>
          <p className="nw-page-eyebrow">ADB / DEVICE FILES</p>
          <h1>文件管理</h1>
          <p>以设备目录为中心的可视化传输工作台</p>
        </div>
        <p className="nw-file-manager-connection"><span aria-hidden="true" />等待连接</p>
      </header>

      <section className="nw-file-manager-workbench" aria-label="设备文件">
        <div className="nw-file-manager-toolbar">
          <button
            type="button"
            className="nw-test-file-refresh nw-file-manager-primary-action"
            disabled={isRefreshingRemote}
            onClick={() => void refreshRemote()}
          >
            加载目录
          </button>
          <button type="button" className="nw-test-file-upload" onClick={() => void uploadFile()}>
            传入文件到手机
          </button>
          <button
            type="button"
            className="nw-test-file-toolbar-download"
            disabled={!selectedRemote || selectedRemote.is_directory || isRefreshingRemote}
            onClick={() => selectedRemote && void downloadEntry(selectedRemote)}
          >
            传出文件到电脑
          </button>
          <button
            type="button"
            disabled={isRefreshingRemote}
            onClick={() => void refreshRemote()}
          >
            刷新目录
          </button>
          <button
            type="button"
            className="nw-test-file-remote-up"
            disabled={currentRemotePath === '/' || isRefreshingRemote}
            onClick={() => void refreshRemote(parentDirectory(currentRemotePath))}
          >
            返回上级
          </button>
          <button type="button" className="nw-test-file-install-apk" onClick={() => void installApk()}>
            安装 APK
          </button>
          <button
            type="button"
            className="nw-test-file-toolbar-delete"
            disabled={!selectedRemote || isRefreshingRemote}
            onClick={() => selectedRemote && setPendingDelete(selectedRemote)}
          >
            删除
          </button>
        </div>
        <div className="nw-file-manager-directory-summary">
          <span>设备目录</span>
          <strong>{currentRemotePath}</strong>
          <span>{remoteEntries.length} 个条目</span>
        </div>
        <ul className="nw-file-manager-entry-grid">
          {remoteEntries.map((entry) => (
            <li className="nw-file-manager-entry" key={entry.full_path}>
              <button
                type="button"
                className="nw-test-file-entry"
                aria-pressed={!entry.is_directory && selectedRemote?.full_path === entry.full_path}
                disabled={isRefreshingRemote}
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
                  disabled={entry.is_directory || isRefreshingRemote}
                  onClick={() => void downloadEntry(entry)}
                >
                  下载
                </button>
                <button
                  type="button"
                  className="nw-test-file-delete"
                  disabled={isRefreshingRemote}
                  onClick={() => setPendingDelete(entry)}
                >
                  删除
                </button>
              </div>
            </li>
          ))}
        </ul>

        <section className="nw-file-manager-log">
          <header><strong>文件日志</strong><span>{operationStatus ? '1 条记录' : '0 条记录'}</span></header>
          {operationStatus ? <p>{operationStatus}</p> : null}
          {errorText ? <p className="nw-error-text">{errorText}</p> : null}
        </section>
        <footer>
          <span>等待连接</span>
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
          <button type="button" className="nw-test-file-delete-confirm" onClick={() => void confirmDelete()}>
            删除
          </button>
        </div>
      </ModalLayer>
    </section>
  );
};

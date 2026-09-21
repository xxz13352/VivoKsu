import { errorMessage } from '../app/error';
import { FC, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { DeviceSnapshotPayload } from '../app/ipc-events';

type MirrorStatus = {
  is_mirroring: boolean;
  auto_mirror_enabled: boolean;
};

const normalizeMirrorStatus = (value: unknown): MirrorStatus => {
  if (value && typeof value === 'object') {
    const raw = value as Record<string, unknown>;
    return {
      is_mirroring: Boolean(raw.is_mirroring ?? raw.isMirroring),
      auto_mirror_enabled: Boolean(raw.auto_mirror_enabled ?? raw.autoMirrorEnabled),
    };
  }

  return { is_mirroring: false, auto_mirror_enabled: false };
};

export const MirrorPage: FC<{ deviceSnapshot?: DeviceSnapshotPayload | null }> = ({ deviceSnapshot }) => {
  const [mirrorStatus, setMirrorStatus] = useState<MirrorStatus | null>(null);
  const [errorText, setErrorText] = useState('');
  const deviceConnected = deviceSnapshot?.connection_state === 'AdbConnected';
  const connectionLabel = deviceConnected
    ? (deviceSnapshot?.connection_label || 'ADB 已连接')
    : '等待 ADB 连接';

  const loadMirrorData = async () => {
    setErrorText('');

    try {
      const mirrorResponse = await invoke<unknown>('mirror_status');
      setMirrorStatus(normalizeMirrorStatus(mirrorResponse));
    } catch (error) {
      setErrorText(errorMessage(error, 'ADB 投屏状态读取失败'));
    }
  };

  useEffect(() => {
    void loadMirrorData();
  }, []);

  const runMirrorCommand = async (command: 'mirror_start' | 'mirror_stop' | 'mirror_set_auto', enabled?: boolean) => {
    setErrorText('');
    try {
      const response = command === 'mirror_set_auto'
        ? await invoke<MirrorStatus>(command, { enabled })
        : await invoke<MirrorStatus>(command);
      setMirrorStatus(response);
    } catch (error) {
      setErrorText(errorMessage(error, 'ADB 投屏操作失败'));
    }
  };

  return (
    <section className="nw-mirror-page" aria-label="ADB 投屏">
      <header className="nw-mirror-heading">
        <div>
          <h1>ADB 投屏</h1>
        </div>
        <div>
          <button type="button" onClick={() => void loadMirrorData()} className="nw-test-mirror-refresh">刷新状态</button>
          <span className="nw-mirror-connection"><i aria-hidden="true" />{connectionLabel}</span>
        </div>
      </header>
      <section className="nw-mirror-console" aria-label="投屏控制">
        <header><div><h2>屏幕镜像控制台</h2></div></header>
        <div className="nw-mirror-controls">
          <article><h3>手动投屏</h3><div><button type="button" className="nw-test-mirror-start" onClick={() => void runMirrorCommand('mirror_start')}>开始投屏</button><button type="button" className="nw-test-mirror-stop" onClick={() => void runMirrorCommand('mirror_stop')}>结束投屏</button></div></article>
          <article><h3>自动投屏</h3><label className="nw-mirror-switch"><input type="checkbox" checked={mirrorStatus?.auto_mirror_enabled ?? false} onChange={(event) => void runMirrorCommand('mirror_set_auto', event.target.checked)} /><span aria-hidden="true" /></label></article>
        </div>
        <footer><div><span>设备传输</span><strong>{connectionLabel}</strong></div><div><span>镜像进程</span><strong>{mirrorStatus?.is_mirroring ? '投屏运行中' : '投屏未启动'}</strong></div></footer>
      </section>
      {errorText && <p className="nw-error-text">{errorText}</p>}
    </section>
  );
};

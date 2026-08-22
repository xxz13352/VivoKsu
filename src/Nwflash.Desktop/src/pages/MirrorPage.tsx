import { errorMessage } from '../app/error';
import { FC, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type UnknownSessionState = unknown;
type UnknownLogList = unknown;
type SessionState = {
  has_token: boolean;
  healthy: boolean;
  running: boolean;
  session_id: string | null;
};

type OperationLogEntry = {
  timestamp_utc: number;
  level: 'Info' | 'Success' | 'Warning' | 'Error';
  message: string;
  operation_id: string | null;
};

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

const normalizeSessionState = (state: UnknownSessionState): SessionState => {
  if (state && typeof state === 'object') {
    const raw = state as Record<string, unknown>;
    return {
      has_token: Boolean(raw.has_token),
      healthy: Boolean(raw.healthy),
      running: Boolean(raw.running),
      session_id: typeof raw.session_id === 'string' ? raw.session_id : null,
    };
  }

  return {
    has_token: false,
    healthy: false,
    running: false,
    session_id: null,
  };
};

const normalizeLogs = (logs: UnknownLogList): readonly OperationLogEntry[] =>
  Array.isArray(logs) ? logs : [];

const formatFlag = (value: boolean) => (value ? '开启' : '关闭');

export const MirrorPage: FC = () => {
  const [sessionState, setSessionState] = useState<SessionState | null>(null);
  const [logs, setLogs] = useState<readonly OperationLogEntry[]>([]);
  const [mirrorStatus, setMirrorStatus] = useState<MirrorStatus | null>(null);
  const [errorText, setErrorText] = useState('');
  const [loading, setLoading] = useState(true);

  const loadMirrorData = async () => {
    setLoading(true);
    setErrorText('');

    try {
      const [stateResponse, logsResponse, mirrorResponse] = await Promise.all([
        invoke<UnknownSessionState>('session_state'),
        invoke<UnknownLogList>('operation_logs_snapshot'),
        invoke<unknown>('mirror_status'),
      ]);
      setSessionState(normalizeSessionState(stateResponse));
      setLogs(normalizeLogs(logsResponse));
      setMirrorStatus(normalizeMirrorStatus(mirrorResponse));
    } catch (error) {
      setErrorText(errorMessage(error, 'ADB 投屏状态读取失败'));
      setSessionState(null);
      setLogs([]);
    } finally {
      setLoading(false);
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
          <p className="nw-page-eyebrow">ADB / SCREENCAST</p>
          <h1>ADB 投屏</h1>
          <p>通过 scrcpy 管理 Android 屏幕镜像</p>
        </div>
        <div>
          <button type="button" onClick={() => void loadMirrorData()} className="nw-test-mirror-refresh">刷新状态</button>
          <span className="nw-mirror-connection"><i aria-hidden="true" />等待连接</span>
        </div>
      </header>
      <section className="nw-mirror-console" aria-label="投屏控制">
        <header><div><p>SCRCPY SESSION</p><h2>屏幕镜像控制台</h2></div><span>scrcpy 资源</span></header>
        <div className="nw-mirror-controls">
          <article><h3>手动投屏</h3><p>发布包已内置 scrcpy；如组件校验失败，请重新安装应用。</p><div><button type="button" className="nw-test-mirror-start" onClick={() => void runMirrorCommand('mirror_start')}>开始投屏</button><button type="button" className="nw-test-mirror-stop" onClick={() => void runMirrorCommand('mirror_stop')}>结束投屏</button></div></article>
          <article><h3>自动投屏</h3><p>检测到 ADB 设备后自动开启；请先准备 scrcpy，会话意外关闭时自动恢复。</p><label className="nw-mirror-switch"><input type="checkbox" checked={mirrorStatus?.auto_mirror_enabled ?? false} onChange={(event) => void runMirrorCommand('mirror_set_auto', event.target.checked)} /><span aria-hidden="true" /></label></article>
        </div>
        <footer><div><span>设备传输</span><strong>等待连接</strong></div><div><span>镜像进程</span><strong>{mirrorStatus?.is_mirroring ? '投屏运行中' : '投屏未启动'}</strong></div></footer>
      </section>
      {errorText && <p className="nw-error-text">{errorText}</p>}
      <div className="nw-mirror-legacy-state" aria-hidden="true">
        {loading ? <span>正在刷新镜像与会话数据...</span> : null}
        {sessionState ? <span>{formatFlag(sessionState.running)}</span> : null}
        {logs.length === 0 ? <p className="nw-empty-log">暂无历史日志</p> : null}
        {logs.slice(0, 3).map((log, index) => <span key={`${log.timestamp_utc}-${index}`} className="nw-test-mirror-log-item">{log.level}{log.message}</span>)}
      </div>
    </section>
  );
};

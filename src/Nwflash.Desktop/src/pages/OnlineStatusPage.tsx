import { errorMessage } from '../app/error';
import { FC, useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type OnlineSession = {
  name: string;
  client_version: string;
  connected_at: number;
  last_seen_at: number;
  duration_seconds: number;
  is_self: boolean;
};

export const formatDuration = (seconds: number) => {
  const safeSeconds = Math.max(0, Math.floor(Number.isFinite(seconds) ? seconds : 0));
  const hours = Math.floor(safeSeconds / 3_600);
  const minutes = Math.floor((safeSeconds % 3_600) / 60);
  const remainder = safeSeconds % 60;
  return [hours, minutes, remainder].map((value) => String(value).padStart(2, '0')).join(':');
};

const formatClock = (value: Date) =>
  [value.getHours(), value.getMinutes(), value.getSeconds()]
    .map((part) => String(part).padStart(2, '0'))
    .join(':');

const formatUpdatedText = (value: Date) => `更新于 ${formatClock(value)}`;

const deriveDuration = (session: OnlineSession, nowEpochSeconds: number) => {
  const connectedAt = Number(session.connected_at);
  if (Number.isFinite(connectedAt) && connectedAt >= 0) {
    return Math.max(0, nowEpochSeconds - Math.floor(connectedAt));
  }

  return Math.max(0, Number(session.duration_seconds) || 0);
};

const deriveHeartbeatText = (value: unknown) => {
  if (value && typeof value === 'object') {
    const raw = value as Record<string, unknown>;
    if (raw.running === true && raw.healthy === true) return '心跳:正常';
    if (raw.running === true && raw.healthy === false) return '心跳:异常';
  }

  return '心跳:未连接';
};

export const OnlineStatusPage: FC = () => {
  const [sessions, setSessions] = useState<OnlineSession[]>([]);
  const [loading, setLoading] = useState(true);
  const [errorText, setErrorText] = useState('');
  const [heartbeatText, setHeartbeatText] = useState('心跳:未连接');
  const [lastUpdatedText, setLastUpdatedText] = useState('尚未刷新');
  const [statusText, setStatusText] = useState('登录后自动同步在线状态。');
  const [nowEpochSeconds, setNowEpochSeconds] = useState(() => Math.floor(Date.now() / 1_000));
  const requestInFlight = useRef(false);
  const mounted = useRef(true);

  const loadSessions = useCallback(async () => {
    if (requestInFlight.current) {
      return;
    }

    requestInFlight.current = true;
    setLoading(true);
    setErrorText('');
    try {
      const [response, sessionState] = await Promise.all([
        invoke<OnlineSession[]>('online_sessions'),
        invoke<unknown>('session_state').catch(() => null),
      ]);
      if (!mounted.current) return;

      const nextSessions = Array.isArray(response) ? response : [];
      setSessions(nextSessions);
      setNowEpochSeconds(Math.floor(Date.now() / 1_000));
      setHeartbeatText(deriveHeartbeatText(sessionState));
      setLastUpdatedText(formatUpdatedText(new Date()));
      setStatusText(nextSessions.length === 0 ? '当前没有在线用户。' : `${nextSessions.length} 个在线会话`);
    } catch (error) {
      if (!mounted.current) return;
      setSessions([]);
      setErrorText(errorMessage(error, '获取在线会话失败'));
    } finally {
      requestInFlight.current = false;
      if (mounted.current) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void loadSessions();
    const timer = window.setInterval(() => {
      void loadSessions();
    }, 5000);

    return () => clearInterval(timer);
  }, [loadSessions]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      setNowEpochSeconds(Math.floor(Date.now() / 1_000));
    }, 1_000);

    return () => clearInterval(timer);
  }, []);

  useEffect(() => () => {
    mounted.current = false;
  }, []);

  return (
    <section className="nw-online-page" aria-label="在线状态">
      <header className="nw-online-heading">
        <div><p className="nw-page-eyebrow">ONLINE / SESSION</p><h1>在线状态</h1><p>实时查看当前在线的 奶蛙Flash 用户与在线时长（每 5 秒同步）</p></div>
        <span className="nw-online-heartbeat">{heartbeatText}</span>
      </header>
      <section className="nw-online-workbench">
        <header className="nw-online-session-bar">
          <div><strong>在线会话</strong><span>{sessions.length} 人在线</span></div>
          <div><span className="nw-online-last-updated">{lastUpdatedText}</span><button type="button" className="nw-test-online-refresh" onClick={() => void loadSessions()} disabled={loading}>刷新</button></div>
        </header>
        {loading && !errorText ? <p className="nw-online-loading">正在加载...</p> : null}
        {errorText ? <p className="nw-error-text">{errorText}</p> : null}
        {!loading && !errorText && sessions.length === 0 ? <div className="nw-online-empty nw-empty-online"><span /><em>NO ACTIVE SESSION</em><strong>暂无在线用户</strong><p>其他客户端暂无最新在线</p></div> : null}
        {sessions.length > 0 ? <ul className="nw-online-list">{sessions.map((session) => (
          <li key={`${session.name}-${session.connected_at}`} className="nw-test-online-item">
            <div><strong>{session.name}</strong>{session.is_self ? <span className="nw-online-self-label">（当前设备）</span> : null}</div>
            <span>客户端版本：{session.client_version}</span><span>在线时长：{formatDuration(deriveDuration(session, nowEpochSeconds))}</span>
          </li>
        ))}</ul> : null}
        <footer>{statusText}</footer>
      </section>
    </section>
  );
};

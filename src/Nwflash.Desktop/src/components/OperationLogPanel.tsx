import { errorMessage } from '../app/error';
import { FC, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { OperationSnapshotPayload } from '../app/ipc-events';

type OperationLogEntry = {
  timestamp_utc: number;
  level: 'Info' | 'Success' | 'Warning' | 'Error';
  message: string;
  operation_id: string | null;
};

const formatTimestamp = (timestampUtc: number) =>
  new Date(timestampUtc * 1000).toLocaleTimeString('zh-CN', { hour12: false });

const normalizeLogMessage = (message: string) => {
  switch (message) {
    case '正在解析服务器 OTA':
    case '正在获取在线 OTA 信息':
    case '正在请求 OTA 服务器':
    case '正在请求 OTA 服务端':
      return '正在请求服务器';
    case '检测服务器 OTA':
      return '请求服务器';
    case '正在下载在线 OTA':
      return '正在下载在线固件';
    case '提取服务器 OTA 分区':
      return '提取服务器固件分区';
    case '正在探测 OTA 格式':
      return '正在探测固件格式';
    default:
      return message.replaceAll('OTA', '固件');
  }
};

const isHiddenLogMessage = (message: string) => {
  const normalized = message.trim();
  return normalized.length === 0
    || normalized.startsWith('准备 VIVO 线刷');
};

const normalizeEntries = (entries: OperationLogEntry[]): OperationLogEntry[] =>
  entries
    .map((entry) => ({ ...entry, message: normalizeLogMessage(entry.message.trim()) }))
    .filter((entry) => !isHiddenLogMessage(entry.message))
    .sort((left, right) => left.timestamp_utc - right.timestamp_utc);

const normalizeResponse = (entries: unknown): readonly OperationLogEntry[] =>
  Array.isArray(entries)
    ? normalizeEntries(entries as OperationLogEntry[])
    : [];

const operationMessageKey = (entry: OperationLogEntry) =>
  entry.operation_id
    ? `${entry.operation_id}|${entry.level}|${entry.message}`
    : `${entry.timestamp_utc}|${entry.level}|${entry.message}`;

const operationLevel = (kind: OperationSnapshotPayload['kind']): OperationLogEntry['level'] => {
  if (kind === 'Failed') return 'Error';
  if (kind === 'Canceled') return 'Warning';
  if (kind === 'Completed') return 'Success';
  return 'Info';
};

type OperationLogPanelProps = {
  operationSnapshot?: OperationSnapshotPayload | null;
};

export const OperationLogPanel: FC<OperationLogPanelProps> = ({ operationSnapshot = null }) => {
  const [entries, setEntries] = useState<readonly OperationLogEntry[]>([]);
  const [eventEntries, setEventEntries] = useState<readonly OperationLogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [errorText, setErrorText] = useState('');
  const logRef = useRef<HTMLElement>(null);

  const visibleEntries = useMemo(() => {
    const normalizedEntries = normalizeEntries([...entries]);
    const normalizedEvents = normalizeEntries([...eventEntries]);
    const persistedKeys = new Set(normalizedEntries.map(operationMessageKey));
    const pendingEvents = normalizedEvents.filter((entry) => !persistedKeys.has(operationMessageKey(entry)));
    return normalizeEntries([...normalizedEntries, ...pendingEvents]);
  }, [entries, eventEntries]);

  const refresh = () => {
    setLoading(true);
    setErrorText('');
    invoke<OperationLogEntry[]>('operation_logs_snapshot')
      .then((response) => {
        setEntries(normalizeResponse(response));
      })
      .catch((error) => {
        setErrorText(errorMessage(error, '操作日志读取失败'));
      })
      .finally(() => {
        setLoading(false);
      });
  };

  const clear = async () => {
    setErrorText('');
    try {
      await invoke<void>('operation_logs_clear');
      setEntries([]);
      setEventEntries([]);
    } catch (error) {
      setErrorText(errorMessage(error, '操作日志清空失败'));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  useEffect(() => {
    if (!operationSnapshot) return;

    const rawMessage = (operationSnapshot.stage || operationSnapshot.title).trim();
    if (operationSnapshot.kind === 'Idle' || isHiddenLogMessage(rawMessage)) return;

    const eventEntry: OperationLogEntry = {
      timestamp_utc: operationSnapshot.startedAt ?? Math.floor(Date.now() / 1000),
      level: operationLevel(operationSnapshot.kind),
      message: normalizeLogMessage(rawMessage),
      operation_id: operationSnapshot.operationId,
    };
    if (isHiddenLogMessage(eventEntry.message)) return;
    setEventEntries((current) => {
      const next = [...current, eventEntry];
      const seen = new Set<string>();
      return next.filter((entry) => {
        const key = operationMessageKey(entry);
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      }).slice(0, 500);
    });
  }, [operationSnapshot]);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [visibleEntries]);

  return (
    <section
      ref={logRef}
      className="nw-operation-log-panel"
      data-role="operation-log-panel"
      role="log"
      aria-label="操作日志"
      aria-live="polite"
    >
      <header className="nw-operation-log-header">
        <div>
          <div className="nw-operation-log-eyebrow">ACTIVITY LOG</div>
          <h2 className="nw-operation-log-heading">操作日志</h2>
        </div>
        <div className="nw-operation-log-actions">
          <span className="nw-operation-log-count">{visibleEntries.length} 条记录</span>
          <button type="button" aria-label="清空操作日志" onClick={() => void clear()}>
            清空
          </button>
        </div>
      </header>
      <div className="nw-operation-log-body">
        {loading && !errorText ? <p>正在加载...</p> : null}
        {errorText && visibleEntries.length === 0 ? <p className="nw-error-text">{errorText}</p> : null}
        {!loading && !errorText && visibleEntries.length === 0 ? (
          <div className="nw-operation-log-empty">
            <span aria-hidden="true" />
            <div>SESSION LOG</div>
            <strong>等待操作记录</strong>
            <p className="nw-empty-log">会话活动将显示在这里</p>
          </div>
        ) : null}
        {visibleEntries.length > 0 ? (
          <ul className="nw-operation-log-preview">
            {visibleEntries.map((log, index) => (
              <li key={`${log.timestamp_utc}-${log.operation_id}-${index}`}>
                [{formatTimestamp(log.timestamp_utc)}] {log.message}
              </li>
            ))}
          </ul>
        ) : null}
      </div>
      <footer className="nw-operation-log-footer">实时记录当前会话的设备操作</footer>
    </section>
  );
};

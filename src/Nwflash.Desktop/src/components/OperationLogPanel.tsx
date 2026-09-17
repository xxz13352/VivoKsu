import { errorMessage } from '../app/error';
import { FC, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { OperationSnapshotPayload } from '../app/ipc-events';

export type OperationLogEntry = {
  timestamp_utc: number;
  level: 'Info' | 'Success' | 'Warning' | 'Error';
  message: string;
  operation_id: string | null;
};

const formatTimestamp = (timestampUtc: number) =>
  new Date(timestampUtc * 1000).toLocaleTimeString('zh-CN', { hour12: false });

const normalizeLogMessage = (message: string) => {
  // 兜底:源头文案已全部去 OTA 字样,此处仅防御新代码漏网(残留 OTA 统一显示为固件)。
  return message.replaceAll('OTA', '固件');
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

const TERMINAL_LOG_KINDS = new Set<OperationSnapshotPayload['kind']>([
  'Idle',
  'Completed',
  'Canceled',
  'Failed',
]);

const clampProgress = (value: number) => Math.min(Math.max(value, 0), 1);

type OperationLogPanelProps = {
  operationSnapshot?: OperationSnapshotPayload | null;
};

export const OperationLogPanel: FC<OperationLogPanelProps> = ({ operationSnapshot = null }) => {
  const [entries, setEntries] = useState<readonly OperationLogEntry[]>([]);
  const [eventEntries, setEventEntries] = useState<readonly OperationLogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [errorText, setErrorText] = useState('');
  const logRef = useRef<HTMLElement>(null);
  const lastSnapshotRef = useRef<
    { id: string | null; kind: OperationSnapshotPayload['kind'] } | undefined
  >(undefined);

  // 运行中操作的实时进度行：直接从快照 progress 派生、原地刷新，
  // 不写入日志存储，也不随每次进度事件产生新日志条目（与 C# 的
  // 单行就地更新一致）。
  const runningSnapshot =
    operationSnapshot && !TERMINAL_LOG_KINDS.has(operationSnapshot.kind)
      ? operationSnapshot
      : null;
  const rawActiveLabel = runningSnapshot
    ? (runningSnapshot.stage || runningSnapshot.title).trim()
    : '';
  const activeLabel =
    runningSnapshot && !isHiddenLogMessage(rawActiveLabel)
      ? normalizeLogMessage(rawActiveLabel)
      : '';
  const activeProgress =
    runningSnapshot && typeof runningSnapshot.progress === 'number' && activeLabel
      ? clampProgress(runningSnapshot.progress)
      : null;

  const visibleEntries = useMemo(() => {
    const normalizedEntries = normalizeEntries([...entries]);
    const normalizedEvents = normalizeEntries([...eventEntries]);
    const persistedKeys = new Set(normalizedEntries.map(operationMessageKey));
    const pendingEvents = normalizedEvents.filter((entry) => !persistedKeys.has(operationMessageKey(entry)));
    return normalizeEntries([...normalizedEntries, ...pendingEvents]);
  }, [entries, eventEntries]);

  const refresh = (options?: { silent?: boolean }) => {
    const silent = options?.silent === true;
    if (!silent) {
      setLoading(true);
      setErrorText('');
    }
    invoke<OperationLogEntry[]>('operation_logs_snapshot')
      .then((response) => {
        setEntries(normalizeResponse(response));
      })
      .catch((error) => {
        if (!silent) {
          setErrorText(errorMessage(error, '操作日志读取失败'));
        }
      })
      .finally(() => {
        if (!silent) {
          setLoading(false);
        }
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

  // 操作结束时补齐日志：终态快照（Failed/Completed/Canceled）与紧随其后的
  // Idle 常常落在同一批更新里被合并渲染，失败快照的 effect 根本不会执行，
  // 于是失败原因在界面上凭空消失。后端在广播终态之前已经把日志落盘，因此
  // 这里以持久化日志为准静默重取一次，保证终态文案一定可见。
  useEffect(() => {
    if (!operationSnapshot) return;
    const currentId = operationSnapshot.operationId ?? null;
    const kind = operationSnapshot.kind;
    const previous = lastSnapshotRef.current;
    lastSnapshotRef.current = { id: currentId, kind };

    const isTerminal = TERMINAL_LOG_KINDS.has(kind) && kind !== 'Idle';
    const previousWasTerminal =
      previous !== undefined && TERMINAL_LOG_KINDS.has(previous.kind) && previous.kind !== 'Idle';
    // 连续空闲心跳没有新日志，跳过。
    if (previous && previous.kind === 'Idle' && kind === 'Idle') return;
    // 终态已经取过一次，紧随的 Idle 不必重复取。
    if (kind === 'Idle' && previousWasTerminal) return;

    // 其余情况（终态、切换了操作、或只观测到 Idle）都以持久化日志为准重取。
    // 最后一种正是"整批更新被合并"的情形：此时 effect 只执行一次且拿到的
    // 只有 Idle，若不强取一次，失败原因就会彻底不显示。
    if (isTerminal || kind === 'Idle' || (previous && previous.id !== currentId)) {
      refresh({ silent: true });
    }
  }, [operationSnapshot]);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
    // 进度行出现/消失时也滚动到底；其后的百分比刷新不滚动，避免打断
    // 用户在下载期间向上翻阅历史日志。
  }, [visibleEntries, activeProgress !== null]);

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
        {activeProgress !== null ? (
          <p className="nw-operation-log-progress">
            [{formatTimestamp(runningSnapshot?.startedAt ?? Math.floor(Date.now() / 1000))}]{' '}
            {activeLabel} {Math.round(activeProgress * 100)}%
          </p>
        ) : null}
      </div>
    </section>
  );
};

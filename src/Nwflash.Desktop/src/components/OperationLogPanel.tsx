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

/// 把一条快照派生的临时日志行并入既有集合。
///
/// 同一条 stage 重复广播（进度推进、分区任务刷新都会触发）时覆盖旧值：
/// 既去重不刷屏，又把时间推进到最新一次观测。Map 保留首次插入位置，
/// 因此整体顺序仍是真实的阶段先后。
const mergeEventEntry = (
  current: ReadonlyMap<string, OperationLogEntry>,
  key: string,
  entry: OperationLogEntry,
): ReadonlyMap<string, OperationLogEntry> => {
  const next = new Map(current);
  next.set(key, entry);
  while (next.size > 500) {
    const oldest = next.keys().next().value as string | undefined;
    if (oldest === undefined) break;
    next.delete(oldest);
  }
  return next;
};

/// 把快照派生的临时日志行按「同一条 stage」合并成一条，只保留**最后一次
/// 观测到的时间**。
///
/// 后端每个阶段都会广播一次快照，同一段 stage（例如「刷写分区[28/38]」）
/// 往往连发多次（进度推进、分区任务刷新）。若把每次快照都当成一条新记录，
/// 日志区会被同一条文案刷屏；而按 `startedAt` 渲染又会让整轮刷写的每一行
/// 都显示操作起点（= 日志时间全是第一个分区的时间）。这里取「最后一次观测」，
/// 时间才与这一刻真正吻合。
const collapseEventEntries = (
  entries: Iterable<OperationLogEntry>,
): readonly OperationLogEntry[] => {
  const collapsed = new Map<string, OperationLogEntry>();
  for (const entry of entries) {
    collapsed.set(operationMessageKey(entry), entry);
  }
  return [...collapsed.values()];
};

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

/// 距离底部在这个像素数以内就认为用户仍贴底（自动跟随）。取一个略大于
/// 行高（11px 字号 + 6px 行距）的值，容忍字体/图片加载造成的高度抖动。
const STICK_TO_BOTTOM_THRESHOLD = 24;

type OperationLogPanelProps = {
  operationSnapshot?: OperationSnapshotPayload | null;
};

export const OperationLogPanel: FC<OperationLogPanelProps> = ({ operationSnapshot = null }) => {
  const [entries, setEntries] = useState<readonly OperationLogEntry[]>([]);
  /// 快照派生的临时行：按「同一条 stage」折叠，只留最后一次观测（见
  /// [`collapseEventEntries`]）。Map 的插入序天然等于每条 stage 的首见顺序，
  /// 因此不需要再排序。
  const [eventEntries, setEventEntries] = useState<ReadonlyMap<string, OperationLogEntry>>(
    () => new Map(),
  );
  /// 与 `eventEntries` 同步的即时镜像：合并要在 effect 同步体内做（见下），
  /// 而同步体里读不到刚 setState 的值，只能自己持有最新一份。
  const prevEventEntriesRef = useRef<ReadonlyMap<string, OperationLogEntry>>(new Map());

  const [loading, setLoading] = useState(true);
  const [errorText, setErrorText] = useState('');
  const logRef = useRef<HTMLElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  /// 是否应该在新日志到达时把视图贴到最新一行。
  ///
  /// 只有用户主动向上翻阅历史时才停下：此刻任何自动滚动都是抢视线。判定
  /// 依据是滚动容器离底部超过一个行高的距离（图片/字体加载引发的抖动会
  /// 带来几像素误差，不能直接比 `==`）。
  const stickToBottomRef = useRef(true);
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
    const normalizedEvents = normalizeEntries([...collapseEventEntries(eventEntries.values())]);
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
      prevEventEntriesRef.current = new Map();
      setEventEntries(prevEventEntriesRef.current);
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
      // 本地时间 = 这条快照真正到达的时刻。绝不能借用 `startedAt`：那是
      // 整个操作的起点，用它会把这轮刷写的每一行都标成同一个时间，
      // 看起来就像「所有分区都在第一个分区那一刻刷完」。服务端的时间
      // 正确，是因为它的明细按各自落库时刻打点，两边本就不该分叉。
      timestamp_utc: Math.floor(Date.now() / 1000),
      level: operationLevel(operationSnapshot.kind),
      message: normalizeLogMessage(rawMessage),
      operation_id: operationSnapshot.operationId,
    };
    if (isHiddenLogMessage(eventEntry.message)) return;
    // 合并逻辑必须在 **effect 同步体内** 完成：React 会把函数式更新推迟到
    // 下一次渲染才求值，若把合并写进 updater，同一批内的多次广播会被压平到
    // 「最后一次渲染时刻」，早先那条 stage 的时间被后来的时间覆盖。
    const key = operationMessageKey(eventEntry);
    const merged = mergeEventEntry(prevEventEntriesRef.current, key, eventEntry);
    prevEventEntriesRef.current = merged;
    setEventEntries(merged);
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

  // 记录用户的滚动意图：贴底时保持跟随，离开底部就停止自动滚动。
  useEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    const handleScroll = () => {
      stickToBottomRef.current =
        body.scrollHeight - body.scrollTop - body.clientHeight <= STICK_TO_BOTTOM_THRESHOLD;
    };
    body.addEventListener('scroll', handleScroll, { passive: true });
    return () => body.removeEventListener('scroll', handleScroll);
  }, []);

  useEffect(() => {
    const body = bodyRef.current;
    // 容器本身不是滚动宿主：日志行在 .nw-operation-log-body 里滚动，
    // 而面板自身（logRef）的 scrollHeight 恒等于 clientHeight。此前滚动
    // 面板等于什么都没做——这正是「日志不自动滚动」的根因。
    if (!body || !stickToBottomRef.current) return;
    body.scrollTop = body.scrollHeight;
    // 新日志、进度行出现/消失时都贴到底部；用户已向上翻阅时不打扰。
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
      <div className="nw-operation-log-body" ref={bodyRef}>
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
            {/* 进度行说「此刻在写第几个分区」，时间就必须是此刻：借 startedAt
                会把整轮刷写都钉在操作起点，与服务端（各自按真实时刻打点）
                显示的分布完全对不上。 */}
            [{formatTimestamp(Math.floor(Date.now() / 1000))}]{' '}
            {activeLabel} {Math.round(activeProgress * 100)}%
          </p>
        ) : null}
      </div>
    </section>
  );
};

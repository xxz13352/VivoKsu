import { errorMessage } from '../app/error';
import { FC, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import type { OperationSnapshotPayload } from '../app/ipc-events';
import { ModalLayer } from '../components/ModalLayer';

type PartitionEntry = {
  name: string;
  size_bytes: number | null;
  is_high_risk: boolean;
  is_mounted: boolean;
};

type PartitionSnapshot = {
  transport: 'Fastboot' | 'AdbRoot';
  active_slot: string;
  partitions: readonly PartitionEntry[];
};

type PartitionConfirmation = {
  task_count: number;
  high_risk_count: number;
  mounted_count: number;
};

type PendingOperation = {
  kind: 'erase' | 'write';
  confirmation: PartitionConfirmation;
};

type RequestedTransport = 'Automatic' | 'AdbRoot' | 'Fastboot';

const formatSize = (sizeBytes: number | null) => {
  if (sizeBytes === null) return '--';
  if (sizeBytes < 1024) return `${sizeBytes} B`;
  if (sizeBytes < 1024 * 1024) return `${(sizeBytes / 1024).toFixed(1)} KB`;
  if (sizeBytes < 1024 * 1024 * 1024) return `${(sizeBytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(sizeBytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
};

const transportLabel = (transport: RequestedTransport | null) =>
  transport === 'AdbRoot' ? 'ADB Root' : transport === 'Fastboot' ? 'Fastboot' : '自动';

export const LineFlashPage: FC<{ operationSnapshot?: OperationSnapshotPayload | null }> = ({ operationSnapshot }) => {
  const [snapshot, setSnapshot] = useState<PartitionSnapshot | null>(null);
  const [requestedTransport, setRequestedTransport] = useState<RequestedTransport>('Automatic');
  const requestedTransportRef = useRef<RequestedTransport>('Automatic');
  const explicitRefreshRef = useRef(false);
  const [filterText, setFilterText] = useState('');
  const [selectedNames, setSelectedNames] = useState<readonly string[]>([]);
  const [mappedCount, setMappedCount] = useState(0);
  const [pendingOperation, setPendingOperation] = useState<PendingOperation | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [statusText, setStatusText] = useState('等待读取分区表');
  const [errorText, setErrorText] = useState('');
  const [partitionTaskStates, setPartitionTaskStates] = useState<Record<string, string>>({});

  useEffect(() => {
    const tasks = operationSnapshot?.partitionTasks;
    if (tasks && tasks.length > 0) {
      setPartitionTaskStates(Object.fromEntries(tasks.map((task) => [task.partition_name, task.state])));
      return;
    }
    const task = operationSnapshot?.partitionTask;
    if (task) setPartitionTaskStates((current) => ({ ...current, [task.partition_name]: task.state }));
  }, [operationSnapshot]);

  useEffect(() => {
    let disposed = false;
    const restoreCachedSnapshot = async () => {
      try {
        const cachedSnapshot = await invoke<PartitionSnapshot | null>('partitions_cached_snapshot');
        if (disposed || explicitRefreshRef.current || !cachedSnapshot) return;
        setSnapshot(cachedSnapshot);
        setSelectedNames((current) => current.filter((name) =>
          cachedSnapshot.partitions.some((partition) => partition.name === name),
        ));
        setStatusText(`已恢复 ${cachedSnapshot.partitions.length} 个分区`);
      } catch (error) {
        if (!disposed && !explicitRefreshRef.current) {
          setErrorText(errorMessage(error, '恢复分区表缓存失败'));
        }
      }
    };

    void restoreCachedSnapshot();
    return () => {
      disposed = true;
    };
  }, []);

  const visiblePartitions = useMemo(() => snapshot?.partitions.filter((entry) =>
    entry.name.toLowerCase().includes(filterText.trim().toLowerCase()),
  ) ?? [], [filterText, snapshot]);

  const refreshPartitions = async () => {
    explicitRefreshRef.current = true;
    setErrorText('');
    setIsRefreshing(true);
    try {
      const nextSnapshot = await invoke<PartitionSnapshot>('partitions_refresh', {
        requestedTransport: requestedTransportRef.current,
      });
      setSnapshot(nextSnapshot);
      setSelectedNames((current) => current.filter((name) =>
        nextSnapshot.partitions.some((partition) => partition.name === name),
      ));
      setMappedCount(0);
      setPartitionTaskStates({});
      setStatusText(`已读取 ${nextSnapshot.partitions.length} 个分区`);
    } catch (error) {
      setErrorText(errorMessage(error, '读取分区表失败'));
    } finally {
      setIsRefreshing(false);
    }
  };

  const togglePartition = (name: string) => {
    setSelectedNames((current) => current.includes(name)
      ? current.filter((item) => item !== name)
      : [...current, name]);
  };

  const mapImages = async () => {
    const selectedPaths = await open({
      multiple: true,
      directory: false,
      filters: [{ name: 'Android 镜像', extensions: ['img', 'bin'] }],
    });
    const imagePaths = Array.isArray(selectedPaths)
      ? selectedPaths.filter((path): path is string => typeof path === 'string')
      : typeof selectedPaths === 'string' ? [selectedPaths] : [];
    if (imagePaths.length === 0) return;

    setErrorText('');
    try {
      const mapped = await invoke<{ mapped_count: number }>('partitions_map_images', { imagePaths });
      setMappedCount(mapped.mapped_count);
      setStatusText(`已映射 ${mapped.mapped_count} 个镜像`);
    } catch (error) {
      setErrorText(errorMessage(error, '分区镜像检查失败'));
    }
  };

  const executeBackup = async (outputDirectory: string) => {
    setErrorText('');
    try {
      await invoke('partitions_execute_backup', {
        selectedNames,
        outputDirectory,
      });
      setStatusText('分区备份已完成');
    } catch (error) {
      setErrorText(errorMessage(error, '分区备份失败'));
    }
  };

  const prepareOperation = async (kind: PendingOperation['kind'] | 'backup') => {
    if (selectedNames.length === 0) return;
    if (kind === 'backup') {
      const selectedDirectory = await open({ multiple: false, directory: true });
      if (typeof selectedDirectory !== 'string') return;
      await executeBackup(selectedDirectory);
      return;
    }

    setErrorText('');
    try {
      const command = kind === 'erase'
        ? 'partitions_prepare_erase'
        : kind === 'write' ? 'partitions_prepare_write' : 'partitions_prepare_backup';
      const confirmation = await invoke<PartitionConfirmation>(command, {
        selectedNames,
      });
      setPendingOperation({ kind, confirmation });
    } catch (error) {
      setErrorText(errorMessage(error, '分区操作预检失败'));
    }
  };

  const executePendingOperation = async () => {
    if (!pendingOperation || selectedNames.length === 0) return;
    const { kind } = pendingOperation;
    const command = kind === 'erase'
      ? 'partitions_execute_erase'
      : 'partitions_execute_write';
    setErrorText('');
    try {
      await invoke(command, {
        selectedNames,
      });
      setPendingOperation(null);
      setStatusText(kind === 'erase' ? '分区擦除已完成' : kind === 'write' ? '分区写入已完成' : '分区备份已完成');
    } catch (error) {
      setErrorText(errorMessage(error, '分区操作失败'));
    }
  };

  const cancelOperation = async () => {
    setErrorText('');
    try {
      await invoke('operation_cancel');
    } catch (error) {
      setErrorText(errorMessage(error, '停止分区操作失败'));
    }
  };

  const operationTitle = pendingOperation?.kind === 'erase'
    ? '确认擦除分区'
    : '确认写入分区';

  return (
    <section className="nw-line-flash-page nw-line-flash-workspace" aria-label="可视刷写">
      <header className="nw-line-flash-heading">
        <div>
          <p className="nw-page-eyebrow">PARTITION / WORKSPACE</p>
          <h1>可视刷写</h1>
          <p>读取全量分区，按当前通道执行备份、写入或擦除</p>
        </div>
        <span>当前通道 {transportLabel(snapshot?.transport ?? requestedTransport)}</span>
      </header>
      <section className="nw-line-flash-console" aria-label="分区工作区">
        <header>
          <div className="nw-line-flash-transport">
            <strong>传输通道</strong>
            {([
              { value: 'Automatic', label: '自动' },
              { value: 'AdbRoot', label: 'ADB Root' },
              { value: 'Fastboot', label: 'Fastboot' },
            ] as const).map(({ value, label }) => (
              <button
                key={value}
                type="button"
                className={`nw-test-line-transport-${value}`}
                aria-pressed={requestedTransport === value}
                onClick={() => {
                  requestedTransportRef.current = value;
                  setRequestedTransport(value);
                }}
              >
                {label}
              </button>
            ))}
          </div>
          <div>
            <button type="button" className="nw-test-line-partitions-select-images" disabled={!snapshot} onClick={() => void mapImages()}>载入镜像</button>
            <button type="button" className="nw-test-line-partitions-refresh" disabled={isRefreshing} onClick={() => void refreshPartitions()}>{isRefreshing ? '正在读取…' : '读取分区表'}</button>
          </div>
        </header>
        <div className="nw-line-flash-filter">
          <label htmlFor="line-flash-filter">分区筛选</label>
          <input id="line-flash-filter" className="nw-test-line-partition-filter" aria-label="分区筛选" value={filterText} onChange={(event) => setFilterText(event.target.value)} />
          <span>{snapshot?.partitions.length ?? 0} 个分区 · 已选 {selectedNames.length} · 已映射 {mappedCount}</span>
        </div>
        <div className="nw-line-flash-list-heading"><span>分区表</span><span>{visiblePartitions.length} 个分区</span></div>
        {snapshot === null ? (
          <div className="nw-line-flash-partition-empty">
            <span className="nw-line-flash-empty-icon" aria-hidden="true">□</span><strong>读取分区表后开始</strong><p>连接 ADB Root 或 Fastboot 设备，读取全量分区后即可勾选执行</p>
          </div>
        ) : visiblePartitions.length === 0 ? (
          <div className="nw-line-flash-partition-empty"><strong>没有匹配的分区</strong></div>
        ) : (
          <ul className="nw-line-flash-partition-list">
            {visiblePartitions.map((entry) => (
              <li
                key={entry.name}
                className={selectedNames.includes(entry.name) ? 'selected' : ''}
                tabIndex={0}
                onDoubleClick={() => togglePartition(entry.name)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault();
                    togglePartition(entry.name);
                  }
                }}
              >
                <label><input type="checkbox" className={`nw-test-line-partition-select-${entry.name}`} checked={selectedNames.includes(entry.name)} readOnly disabled tabIndex={-1} />选择</label>
                <strong>{entry.name}</strong><span>{formatSize(entry.size_bytes)}</span>
                {entry.is_high_risk || entry.is_mounted ? <em>{entry.is_mounted ? '已挂载' : '高风险'}</em> : null}
                {partitionTaskStates[entry.name] ? <span>{partitionTaskStates[entry.name]}</span> : null}
              </li>
            ))}
          </ul>
        )}
      </section>
      <footer className="nw-line-flash-taskbar">
        <div><strong>{statusText}</strong><span>{operationSnapshot?.progress !== null && operationSnapshot?.progress !== undefined ? `进度 ${Math.round(operationSnapshot.progress * 100)}%` : '速度 --  耗时 00:00'}</span></div>
        <div>
          <button type="button" className="nw-test-line-partitions-backup" disabled={selectedNames.length === 0} onClick={() => void prepareOperation('backup')}>备份所选</button>
          <button type="button" className="nw-test-line-partitions-prepare-write" disabled={selectedNames.length === 0} onClick={() => void prepareOperation('write')}>写入所选</button>
          <button type="button" className="nw-test-line-partitions-prepare-erase" disabled={selectedNames.length === 0} onClick={() => void prepareOperation('erase')}>擦除所选</button>
          <button type="button" className="nw-test-line-partitions-cancel" onClick={() => void cancelOperation()}>停止</button>
        </div>
      </footer>
      {errorText ? <p className="nw-error-text">{errorText}</p> : null}
      <ModalLayer isVisible={pendingOperation !== null} title={operationTitle} onClose={() => setPendingOperation(null)}>
        <p>将{pendingOperation?.kind === 'erase' ? '擦除' : '写入'} {pendingOperation?.confirmation.task_count ?? 0} 个分区，其中 {pendingOperation?.confirmation.high_risk_count ?? 0} 个高风险分区。{(pendingOperation?.confirmation.mounted_count ?? 0) > 0 ? ` ${pendingOperation?.confirmation.mounted_count} 个已挂载分区。` : null}</p>
        <div className="nw-driver-dialog-actions">
          <button type="button" onClick={() => setPendingOperation(null)}>取消</button>
          <button type="button" className={`nw-test-line-partitions-confirm-${pendingOperation?.kind ?? 'operation'}`} onClick={() => void executePendingOperation()}>确认{pendingOperation?.kind === 'erase' ? '擦除' : '写入'}</button>
        </div>
      </ModalLayer>
    </section>
  );
};

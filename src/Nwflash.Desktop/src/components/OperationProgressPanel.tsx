import { FC } from 'react';
import type { OperationSnapshotPayload } from '../app/ipc-events';
import {
  BusyOperationItem,
  isShellBusy,
  resolveProgressText,
} from '../app/window-state';

const clampProgress = (value: number) => Math.min(Math.max(value, 0), 1);

const formatPercent = (value: number) => `${Math.round(clampProgress(value) * 100)}%`;

/// 总进度：与 C# PartitionWorkspace 一致，取已上报分区行的平均进度；
/// 没有分区行时退回快照顶层 progress（VIVO 线刷 / 固件提取等单进度操作）。
const resolveOverallProgress = (
  snapshot: OperationSnapshotPayload | null | undefined,
): number | null => {
  if (!snapshot) return null;
  if (snapshot.partitionTasks && snapshot.partitionTasks.length > 0) {
    const total = snapshot.partitionTasks.reduce(
      (sum, task) => sum + clampProgress(task.overall_progress),
      0,
    );
    return clampProgress(total / snapshot.partitionTasks.length);
  }
  if (typeof snapshot.progress === 'number') return clampProgress(snapshot.progress);
  return null;
};

type OperationProgressPanelProps = {
  operations: ReadonlyArray<BusyOperationItem>;
  operationSnapshot?: OperationSnapshotPayload | null;
};

export const OperationProgressPanel: FC<OperationProgressPanelProps> = ({
  operations,
  operationSnapshot = null,
}) => {
  const isIdle = !isShellBusy(operations);
  const progressText = resolveProgressText(operations);
  const partitionTask = operationSnapshot?.partitionTask ?? null;
  const overallProgress = resolveOverallProgress(operationSnapshot);
  const currentProgress = partitionTask ? clampProgress(partitionTask.overall_progress) : null;
  const currentIndeterminate =
    partitionTask !== null && partitionTask.state === 'Running' && partitionTask.overall_progress <= 0;

  return (
    <section className="nw-progress-panel" data-role="operation-progress">
      <div className="nw-progress-title">操作进度</div>
      <div className="nw-progress-text">{progressText}</div>
      {!isIdle && partitionTask ? (
        <>
          <div className="nw-progress-row">
            <span className="nw-progress-label">当前分区</span>
            <span className="nw-progress-partition">{partitionTask.partition_name}</span>
            <span className="nw-progress-percent">
              {currentProgress !== null && currentProgress > 0 ? formatPercent(currentProgress) : '--'}
            </span>
          </div>
          <div
            className={`nw-progress-bar${currentIndeterminate ? ' is-indeterminate' : ''}`}
            role="progressbar"
            aria-label="当前分区进度"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={currentIndeterminate ? undefined : Math.round((currentProgress ?? 0) * 100)}
          >
            <span
              style={currentIndeterminate ? undefined : { width: `${Math.round((currentProgress ?? 0) * 100)}%` }}
            />
          </div>
          <div className="nw-progress-row">
            <span className="nw-progress-label">总进度</span>
            <span className="nw-progress-percent">
              {overallProgress !== null ? formatPercent(overallProgress) : '--'}
            </span>
          </div>
          <div
            className="nw-progress-bar"
            role="progressbar"
            aria-label="总进度"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={overallProgress !== null ? Math.round(overallProgress * 100) : undefined}
          >
            <span style={{ width: `${overallProgress !== null ? Math.round(overallProgress * 100) : 0}%` }} />
          </div>
        </>
      ) : null}
      {!isIdle && !partitionTask && overallProgress !== null ? (
        <>
          <div className="nw-progress-row">
            <span className="nw-progress-label">总进度</span>
            <span className="nw-progress-percent">{formatPercent(overallProgress)}</span>
          </div>
          <div
            className="nw-progress-bar"
            role="progressbar"
            aria-label="总进度"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(overallProgress * 100)}
          >
            <span style={{ width: `${Math.round(overallProgress * 100)}%` }} />
          </div>
        </>
      ) : null}
      {!isIdle && !partitionTask && overallProgress === null ? (
        <div className="nw-progress-bar is-indeterminate" role="progressbar" aria-label="操作进行中">
          <span />
        </div>
      ) : null}
    </section>
  );
};

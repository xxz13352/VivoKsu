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

/// 判「有没有刻度」只认 null：后端报 progress 为 null = 这一步算不出百分比，
/// 才该左右波动。0 是一个真实百分比（真进度起步就是 0%），别拿 > 0 当判据。
const hasScale = (value: number | null): value is number => value !== null;

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
  /// 「当前分区」条有没有刻度，由后端 state 决定：Running 表示后端能算出百分比
  ///（真刷写和假刷写都是，且真进度起步本来就可能是 0%），Waiting 才是「拿不到
  /// 刻度，该左右波动」。不要用 overall_progress>0 当判据——那会把 0% 起步的
  /// 真实进度误判成没刻度，整个刷写过程都在波动。判据与 SafeFlashPage 对齐。
  const currentDeterminate = partitionTask !== null && partitionTask.state === 'Running';

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
              {currentDeterminate ? formatPercent(currentProgress ?? 0) : '--'}
            </span>
          </div>
          <div
            className={`nw-progress-bar${currentDeterminate ? '' : ' is-indeterminate'}`}
            role="progressbar"
            aria-label="当前分区进度"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={currentDeterminate ? Math.round((currentProgress ?? 0) * 100) : undefined}
          >
            <span
              style={
                currentDeterminate ? { width: `${Math.round((currentProgress ?? 0) * 100)}%` } : undefined
              }
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
      {!isIdle && !partitionTask && hasScale(overallProgress) ? (
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
      {!isIdle && !partitionTask && !hasScale(overallProgress) ? (
        <div className="nw-progress-bar is-indeterminate" role="progressbar" aria-label="操作进行中">
          <span />
        </div>
      ) : null}
    </section>
  );
};

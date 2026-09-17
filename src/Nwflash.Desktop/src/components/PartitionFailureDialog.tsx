import { FC, useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { SafeFlashPartitionFailurePayload } from '../app/ipc-events';

type PartitionFailureDialogProps = {
  failure: SafeFlashPartitionFailurePayload | null;
  onResolved: (failure: SafeFlashPartitionFailurePayload) => void;
};

/**
 * 线刷执行中的分区刷写失败弹窗：后端已挂起等待决策，这里展示失败的
 * 分区名与具体报错日志，并让用户选择重试当前分区、继续刷写剩余分区
 * 或中止本次线刷。
 * 决策经 safe_flash_resolve_partition_failure 送回，送达失败（提示已被
 * 后端收尾）时按已收尾处理，直接收起弹窗。
 */
export const PartitionFailureDialog: FC<PartitionFailureDialogProps> = ({ failure, onResolved }) => {
  const [isResolving, setIsResolving] = useState(false);

  const resolve = useCallback(async (decision: 'retry' | 'continue' | 'abort') => {
    if (!failure) {
      return;
    }
    setIsResolving(true);
    try {
      await invoke('safe_flash_resolve_partition_failure', {
        resolution: { session_id: failure.sessionId, decision },
      });
    } catch (error) {
      // 决策未送达（提示已过期/会话已结束）：后端已按超时或取消收尾，
      // 无需再报错打断用户，静默收起即可。
      console.debug('分区失败决策未送达，按已收尾处理:', error);
    } finally {
      setIsResolving(false);
      onResolved(failure);
    }
  }, [failure, onResolved]);

  if (!failure) {
    return null;
  }

  return (
    <section className="nw-partition-failure-dialog" role="alertdialog" aria-label="分区刷写失败">
      <p className="nw-partition-failure-partition">
        分区 <strong>{failure.partitionName || '未知'}</strong> 刷写失败，线刷已暂停，请选择如何继续。
      </p>
      <p className="nw-partition-failure-log" aria-label="失败日志">{failure.errorMessage}</p>
      <div className="nw-partition-failure-actions">
        <button
          type="button"
          className="nw-partition-failure-retry"
          disabled={isResolving}
          onClick={() => void resolve('retry')}
        >
          重试当前分区
        </button>
        <button
          type="button"
          className="nw-partition-failure-continue"
          disabled={isResolving}
          onClick={() => void resolve('continue')}
        >
          继续刷写剩余分区
        </button>
        <button
          type="button"
          className="nw-partition-failure-abort"
          disabled={isResolving}
          onClick={() => void resolve('abort')}
        >
          中止本次线刷
        </button>
      </div>
      <p className="nw-partition-failure-hint">
        重试不会推进总进度；中止后设备会停留在 fastbootd，可重新执行线刷或手动处理；“停止操作”同样会中止等待。
      </p>
    </section>
  );
};

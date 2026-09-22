import { errorMessage } from '../app/error';
import { FC, FormEvent, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ModalLayer } from '../components/ModalLayer';
import type { DeviceSnapshotPayload, OperationSnapshotPayload } from '../app/ipc-events';
import { isConnectedDeviceSnapshot } from '../app/ipc-events';

type SafeFlashSlotMode = 'CurrentSlot' | 'OtherSlot' | 'BothSlots';
type SafeFlashSourceMode = 'Online' | 'Local';
type SafeFlashPreflight = { session_id: string; source_label: string; partition_count: number; safe_partition_count: number; has_block_based_content?: boolean; requires_confirmation: boolean };
type SafeFlashCompletion = { flashed_partition_count: number; skipped_partition_count: number; status: string };

export const SafeFlashPage: FC<{
  deviceSnapshot?: DeviceSnapshotPayload | null;
  operationSnapshot?: OperationSnapshotPayload | null;
}> = ({ deviceSnapshot, operationSnapshot }) => {
  const [sourceMode, setSourceMode] = useState<SafeFlashSourceMode>('Online');
  const [isSafeFlash, setIsSafeFlash] = useState(true);
  const [isKeepRoot, setIsKeepRoot] = useState(false);
  const [wipeData, setWipeData] = useState(false);
  const [slotMode, setSlotMode] = useState<SafeFlashSlotMode>('CurrentSlot');
  const [preflight, setPreflight] = useState<SafeFlashPreflight | null>(null);
  const [status, setStatus] = useState('');
  const [error, setError] = useState('');
  const [isPreparing, setIsPreparing] = useState(false);
  const [isExecuting, setIsExecuting] = useState(false);
  const closingPreflightRef = useRef(false);
  const options = () => ({ is_safe_flash: isSafeFlash, is_keep_root: isKeepRoot, wipe_data: wipeData, slot_mode: slotMode });

  const prepareOnline = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setError(''); setStatus(''); setPreflight(null); setIsPreparing(true);
    try {
      setPreflight(await invoke<SafeFlashPreflight>('safe_flash_prepare_online', { options: options() }));
      setStatus('线刷预检完成，等待确认');
    }
    catch (reason) { setError(errorMessage(reason, '安全刷写预检失败')); }
    finally { setIsPreparing(false); }
  };
  const prepareLocal = async () => {
    setError(''); setStatus(''); setPreflight(null);
    setIsPreparing(true);
    try {
      setPreflight(await invoke<SafeFlashPreflight>('safe_flash_prepare_local_source', { options: options() }));
      setStatus('线刷预检完成，等待确认');
    }
    catch (reason) { setError(errorMessage(reason, '本地固件预检失败')); }
    finally { setIsPreparing(false); }
  };
  const prepareLocalDirectory = async () => {
    setError(''); setStatus(''); setPreflight(null);
    setIsPreparing(true);
    try {
      setPreflight(await invoke<SafeFlashPreflight>('safe_flash_prepare_local_directory', { options: options() }));
      setStatus('线刷预检完成，等待确认');
    }
    catch (reason) { setError(errorMessage(reason, '本地固件目录预检失败')); }
    finally { setIsPreparing(false); }
  };
  const execute = async () => {
    const confirmed = preflight;
    if (!confirmed || isExecuting || closingPreflightRef.current) return;
    closingPreflightRef.current = true;
    setError('');
    // 先关弹窗再发起刷写（对齐快速刷写页模式）：执行状态由进度面板与底部
    // 「停止操作」接管。确认弹窗若停留整个执行期，全屏遮罩会挡住两者，且
    // 分区失败时分区决策弹窗会叠在它上面——执行中按钮全禁、关闭按钮不渲染，
    // 这层就永远点不掉。
    setPreflight(null);
    setIsExecuting(true);
    try { const completion = await invoke<SafeFlashCompletion>('safe_flash_execute_prepared', { sessionId: confirmed.session_id }); setStatus(completion.status); }
    catch (reason) {
      setError(errorMessage(reason, '安全刷写执行失败'));
      // 执行失败时后端仍保留已预检会话（staging 在盘）：恢复确认弹窗，
      // 用户可直接重试或取消丢弃，不必重新下载/解包整个固件。取消走乐观
      // 收起，即便会话已被作废弹窗也能关掉，不会再卡死。
      setPreflight(confirmed);
    }
    finally { closingPreflightRef.current = false; setIsExecuting(false); }
  };
  const cancelPreflight = async () => {
    const target = preflight;
    if (!target || isExecuting || closingPreflightRef.current) return;
    closingPreflightRef.current = true;
    setError('');
    // 乐观收起：取消命令失败（如预检已被会话失效作废）只作错误提示，弹窗
    // 不再卡死。后端 SafeFlashRuntime 按会话校验，拒绝时预检条目也随会话
    // 失效被 clear_owned 清空，前端无需再点一次才能关窗。
    setPreflight(null);
    try { await invoke('safe_flash_cancel_prepared', { sessionId: target.session_id }); }
    catch (reason) { setError(errorMessage(reason, '取消线刷预检失败')); }
    finally { closingPreflightRef.current = false; }
  };
  const stopPreparation = async () => {
    setError('');
    try { await invoke('operation_cancel'); }
    catch (reason) { setError(errorMessage(reason, '停止线刷预检失败')); }
  };
  const controlsLocked = Boolean(preflight) || isPreparing || isExecuting;
  const deviceConnected = isConnectedDeviceSnapshot(deviceSnapshot);
  const deviceKnownAndDisconnected = deviceSnapshot !== undefined && deviceSnapshot !== null && !deviceConnected;
  const deviceLabel = deviceConnected
    ? `${deviceSnapshot?.connection_label || '设备已连接'}${deviceSnapshot?.model ? ` · ${deviceSnapshot.model}` : ''}`
    : '未连接 ADB/Fastboot 设备';
  const operationStage = operationSnapshot?.isBusy ? operationSnapshot.stage : '';
  // 进度行只记「刷写分区[i/n]」（后端不再追加  ... /  ... OK），
  // 因此这里只匹配方括号里的 i/n；不再要求尾随 ] ...。
  const partitionMatch = operationStage.match(/刷写分区\[(\d+\/\d+)\]/);
  const currentPartition = partitionMatch ? partitionMatch[1] : '--';
  // 分区阶段的 stage 文案本身就是 `刷写分区[i/n]`，而该计数已经显示在进度行里；
  // 再原样印一遍就是重复。这类阶段改用固定文案，其余阶段照常显示 stage。
  const partitionPhase = /刷写分区\[/.test(operationStage);
  const visibleStatus =
    (partitionPhase ? '正在写入分区镜像' : operationStage) || status || '等待操作';

  // ── 进度条之一：总进度 ───────────────────────────────────────────────
  // 后端在**每个阶段**都单调推进它——下载、解包（zip/payload）、逐分区刷写
  // 各自折算到区间里。早先只认「刷写分区[i/n]」，结果最耗时的下载/解包整段
  // 看不到进度条，这正是「解包和刷写都没有实时进度」的一半。因此这里只要
  // 快照带数值进度就显示，不再按阶段名设卡。
  const progressValue =
    typeof operationSnapshot?.progress === 'number'
      ? Math.min(Math.max(operationSnapshot.progress, 0), 1)
      : null;
  const overallPercent = progressValue === null ? null : Math.round(progressValue * 100);

  // ── 进度条之二：当前分区 ─────────────────────────────────────────────
  // 后端在写入期间用 `partitionTask` 单独上报本分区自己的刻度，并用 `state`
  // 区分「有刻度」与「没刻度」：
  //   - Running = 后端确实知道本分区多大 → 显示真实百分比（含 0%，因为
  //     一个刚起步的分区就是 0%，那是**真实进度**而不是未知）。
  //   - Waiting = 后端也不知道（假刷写取不到镜像大小、fastboot 输出被重定向）
  //     → 左右波动的不确定态，**不猜**百分比。
  //
  // 不能用 `fraction > 0` 当"有没有刻度"的判据：假刷写的进度是从 0 平滑涨到
  // 100 的真实进度，用 `> 0` 会把它的起步阶段误判成"没刻度"而全程波动。
  const partitionTask = operationSnapshot?.partitionTask ?? null;
  const partitionHasScale = partitionTask?.state === 'Running';
  const partitionFraction =
    partitionTask && typeof partitionTask.overall_progress === 'number'
      ? Math.min(Math.max(partitionTask.overall_progress, 0), 1)
      : null;
  const partitionDeterminate = partitionHasScale && partitionFraction !== null;
  const partitionPercent = partitionDeterminate ? Math.round(partitionFraction * 100) : null;

  // 刷写阶段才有「当前分区」的语义；下载/解包阶段只显示总进度那一条。
  const isPartitionPhase = partitionPhase;
  // 进度条要等到真正开始推进才出现，避免刚点下按钮就闪一个 0% 的空条。
  const showOverall = isPreparing || isExecuting || (overallPercent !== null && overallPercent > 0);

  return (
    <section className="nw-safe-flash-workspace" aria-label="VIVO 线刷">
      <header className="nw-safe-flash-heading">
        <div>
          <h1>VIVO 线刷</h1>
        </div>
        <span><i />{isPreparing || isExecuting ? '正在操作' : '等待操作'}</span>
      </header>
      <form onSubmit={prepareOnline} className="nw-safe-flash-console nw-test-safe-flash-form">
        <select className="nw-test-safe-source-mode" hidden value={sourceMode} disabled={controlsLocked} onChange={(event) => setSourceMode(event.currentTarget.value as SafeFlashSourceMode)}>
          <option value="Online">在线固件</option><option value="Local">本地固件</option>
        </select>
        <select hidden value={slotMode} disabled={controlsLocked} onChange={(event) => setSlotMode(event.currentTarget.value as SafeFlashSlotMode)}>
          <option value="CurrentSlot">当前槽位</option><option value="OtherSlot">对侧槽位</option><option value="BothSlots">双槽位</option>
        </select>
        <section className="nw-safe-flash-device-summary">
          <span>目标设备</span><strong>{deviceLabel}</strong>
          <div>
            <button type="submit" disabled={controlsLocked || deviceKnownAndDisconnected}>下载+刷入</button>
            <button type="button" onClick={() => void prepareLocal()} disabled={controlsLocked || deviceKnownAndDisconnected}>选择固件</button>
            <button type="button" onClick={() => void prepareLocalDirectory()} disabled={controlsLocked || deviceKnownAndDisconnected} aria-label="选择解包文件夹">选择解包文件夹</button>
          </div>
        </section>
        <section className="nw-safe-flash-options" aria-label="刷写选项">
          <strong>刷写选项</strong>
          <label><input type="checkbox" checked={wipeData} disabled={controlsLocked} onChange={(event) => setWipeData(event.currentTarget.checked)} />清除数据</label>
          <label><input type="checkbox" checked={isSafeFlash} disabled={controlsLocked} onChange={(event) => setIsSafeFlash(event.currentTarget.checked)} />安全刷写</label>
          <label><input type="checkbox" checked={isKeepRoot} disabled={controlsLocked} onChange={(event) => setIsKeepRoot(event.currentTarget.checked)} />保留ROOT</label>
          <span className="nw-safe-flash-slot-label">槽位</span>
          {([
            { value: 'CurrentSlot', label: '当前槽' },
            { value: 'OtherSlot', label: '对槽' },
            { value: 'BothSlots', label: '双槽' },
          ] as const).map(({ value, label }) => (
            <label key={value}><input type="radio" name="safe-flash-slot" checked={slotMode === value} disabled={controlsLocked} onChange={() => setSlotMode(value)} />{label}</label>
          ))}
          <button type="button" disabled>回锁BL</button>
        </section>
        <section className="nw-safe-flash-current">
          {isPartitionPhase ? (
            <div className="nw-safe-flash-progress-row">
              <span className="nw-safe-flash-progress-label">当前分区</span>
              <span className="nw-safe-flash-progress-count">
                {currentPartition}
              </span>
              <span className="nw-safe-flash-progress-percent">
                {partitionPercent !== null ? `${partitionPercent}%` : '--'}
              </span>
            </div>
          ) : (
            <span>当前分区: <strong>{currentPartition}</strong></span>
          )}
          {isPartitionPhase ? (
            <div
              className={`nw-safe-flash-progress${partitionDeterminate ? '' : ' is-indeterminate'}`}
              role="progressbar"
              aria-label="当前分区进度"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={partitionPercent ?? undefined}
            >
              {partitionDeterminate ? <span style={{ width: `${partitionPercent}%` }} /> : <span />}
            </div>
          ) : null}
          {showOverall && overallPercent !== null ? (
            <div className="nw-safe-flash-progress-row">
              <span className="nw-safe-flash-progress-label">总进度</span>
              <span className="nw-safe-flash-progress-percent">{overallPercent}%</span>
            </div>
          ) : null}
          {showOverall && overallPercent !== null ? (
            <div
              className="nw-safe-flash-progress"
              role="progressbar"
              aria-label="线刷总进度"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={overallPercent}
            >
              <span style={{ width: `${overallPercent}%` }} />
            </div>
          ) : null}
          <p>{visibleStatus}</p>
        </section>
      </form>
      {error ? <p className="nw-error-text">{error}</p> : null}
      <ModalLayer isVisible={preflight !== null} title="确认刷写" onClose={isExecuting ? undefined : () => void cancelPreflight()}>
        {preflight ? (
          <>
            <p>{preflight.source_label}</p>
            <p>可刷写分区：{preflight.safe_partition_count}/{preflight.partition_count}</p>
            {preflight.has_block_based_content ? <p>固件含暂不支持刷写的分区内容，这些分区将保持原样。</p> : null}
            {wipeData ? <p>清除数据：刷写完成后会重启到 REC。进 REC 后电脑就检测不到设备了，请手动执行：清除数据-清除全部数据-确定-重启。</p> : null}
            <p>确认后请保持设备连接，中途请勿断开。</p>
            <div className="nw-driver-dialog-actions">
              <button type="button" onClick={() => void cancelPreflight()} disabled={isExecuting}>取消</button>
              <button type="button" className="nw-dialog-confirm" onClick={() => void execute()} disabled={!preflight.requires_confirmation || isExecuting}>确认刷写</button>
            </div>
          </>
        ) : null}
      </ModalLayer>
      <footer className="nw-safe-flash-statusbar"><span>--　00:00</span><button type="button" onClick={() => void stopPreparation()} aria-label="停止操作">停止操作</button></footer>
    </section>
  );
};

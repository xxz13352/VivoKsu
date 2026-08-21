import { errorMessage } from '../app/error';
import { FC, FormEvent, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type SafeFlashSlotMode = 'CurrentSlot' | 'OtherSlot' | 'BothSlots';
type SafeFlashSourceMode = 'Online' | 'Local';
type SafeFlashPreflight = { session_id: string; source_label: string; partition_count: number; safe_partition_count: number; has_block_based_content?: boolean; requires_confirmation: boolean };
type SafeFlashCompletion = { flashed_partition_count: number; skipped_partition_count: number; status: string };

export const SafeFlashPage: FC = () => {
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
  const options = () => ({ is_safe_flash: isSafeFlash, is_keep_root: isKeepRoot, wipe_data: wipeData, slot_mode: slotMode });

  const prepareOnline = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setError(''); setStatus(''); setPreflight(null); setIsPreparing(true);
    try { setPreflight(await invoke<SafeFlashPreflight>('safe_flash_prepare_online', { options: options() })); }
    catch (reason) { setError(errorMessage(reason, '安全刷写预检失败')); }
    finally { setIsPreparing(false); }
  };
  const prepareLocal = async () => {
    setError(''); setStatus(''); setPreflight(null);
    setIsPreparing(true);
    try { setPreflight(await invoke<SafeFlashPreflight>('safe_flash_prepare_local_source', { options: options() })); }
    catch (reason) { setError(errorMessage(reason, '本地固件预检失败')); }
    finally { setIsPreparing(false); }
  };
  const prepareLocalDirectory = async () => {
    setError(''); setStatus(''); setPreflight(null);
    setIsPreparing(true);
    try { setPreflight(await invoke<SafeFlashPreflight>('safe_flash_prepare_local_directory', { options: options() })); }
    catch (reason) { setError(errorMessage(reason, '本地固件目录预检失败')); }
    finally { setIsPreparing(false); }
  };
  const execute = async () => {
    if (!preflight) return;
    setError(''); setIsExecuting(true);
    try { const completion = await invoke<SafeFlashCompletion>('safe_flash_execute_prepared', { sessionId: preflight.session_id }); setStatus(completion.status); setPreflight(null); }
    catch (reason) { setError(errorMessage(reason, '安全刷写执行失败')); }
    finally { setIsExecuting(false); }
  };
  const cancelPreflight = async () => {
    if (!preflight) return;
    setError('');
    try { await invoke('safe_flash_cancel_prepared', { sessionId: preflight.session_id }); setPreflight(null); }
    catch (reason) { setError(errorMessage(reason, '取消线刷预检失败')); }
  };
  const stopPreparation = async () => {
    setError('');
    try { await invoke('operation_cancel'); }
    catch (reason) { setError(errorMessage(reason, '停止线刷预检失败')); }
  };
  const controlsLocked = Boolean(preflight) || isPreparing || isExecuting;

  return (
    <section className="nw-safe-flash-workspace" aria-label="VIVO 线刷">
      <header className="nw-safe-flash-heading">
        <div>
          <p className="nw-page-eyebrow">VIVO LINE FLASH</p>
          <h1>VIVO 线刷</h1>
          <p>查询设备 → 下载 OTA → 解压解包 → 自动重启 fastbootd → 逐个刷入分区</p>
        </div>
        <span><i />{isPreparing || isExecuting ? '正在操作' : '等待操作'}</span>
      </header>
      <form onSubmit={prepareOnline} className="nw-safe-flash-console nw-test-safe-flash-form">
        <select className="nw-test-safe-source-mode" hidden value={sourceMode} disabled={controlsLocked} onChange={(event) => setSourceMode(event.currentTarget.value as SafeFlashSourceMode)}>
          <option value="Online">在线 OTA</option><option value="Local">本地固件</option>
        </select>
        <select hidden value={slotMode} disabled={controlsLocked} onChange={(event) => setSlotMode(event.currentTarget.value as SafeFlashSlotMode)}>
          <option value="CurrentSlot">当前槽位</option><option value="OtherSlot">对侧槽位</option><option value="BothSlots">双槽位</option>
        </select>
        <section className="nw-safe-flash-device-summary">
          <span>目标设备</span><strong>未连接 ADB 设备</strong>
          <div>
            <button type="submit" disabled={controlsLocked}>下载+刷入</button>
            <button type="button" onClick={() => void prepareLocal()} disabled={controlsLocked}>选择固件</button>
            <button type="button" onClick={() => void prepareLocalDirectory()} disabled={controlsLocked} aria-label="选择解包文件夹">选择解包文件夹</button>
          </div>
        </section>
        <p className="nw-safe-flash-description">固件中可直接刷写的分区镜像会被逐个刷入;含块式内容(如 system/vendor/product 的 .new.dat)的固件会在确认时给出警告。</p>
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
        <section className="nw-safe-flash-current"><span>当前分区: <strong>--</strong></span><p>{status || '等待操作'}</p></section>
      </form>
      {error ? <p className="nw-error-text">{error}</p> : null}
      {preflight ? <div className="nw-confirm-modal" role="dialog" aria-modal="true" aria-label="安全线刷确认"><div><strong>确认刷写</strong><p>{preflight.source_label}</p><p>可刷写分区：{preflight.safe_partition_count}/{preflight.partition_count}</p>{preflight.has_block_based_content ? <p>固件含块式分区内容，本次仅刷写可直接镜像分区，其余保持原样。</p> : null}<p>确认后设备将重启到 fastbootd 并逐个刷入,中途请勿断开连接。</p></div><div><button type="button" onClick={() => void cancelPreflight()} disabled={isExecuting}>取消</button><button type="button" onClick={() => void execute()} disabled={!preflight.requires_confirmation || isExecuting}>确认刷写</button></div></div> : null}
      <footer className="nw-safe-flash-statusbar"><span>--　00:00</span><button type="button" onClick={() => void stopPreparation()} aria-label="停止操作">停止操作</button></footer>
    </section>
  );
};

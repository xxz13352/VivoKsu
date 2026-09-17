import { errorMessage } from '../app/error';
import { FC, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { ModalLayer } from '../components/ModalLayer';
import type { OperationSnapshotPayload } from '../app/ipc-events';

type FlashImageInfo = {
  path: string;
  size_bytes: number;
};

type PreparedDualSlotConfirmation = {
  task_count: number;
  switch_slot_after_flash: boolean;
};

type QuickFlashPreset = 'Boot' | 'InitBoot' | 'VendorBoot' | 'Lk';

type PresetImages = Record<QuickFlashPreset, FlashImageInfo | null>;

type PendingFlashPlan = {
  requests: readonly { imagePath: string; partition: QuickFlashPreset }[];
};

const presets: readonly { value: QuickFlashPreset; label: string }[] = [
  { value: 'Boot', label: 'boot' },
  { value: 'InitBoot', label: 'init_boot' },
  { value: 'VendorBoot', label: 'vendor_boot' },
  { value: 'Lk', label: 'lk' },
];

// 与 Rust `QuickFlashPartition::partition_name()` 保持一致：这里只用于确认弹窗
// 里给用户看的分区名预览，不参与任何命令构造。
const presetPartitionNames: Record<QuickFlashPreset, string> = {
  Boot: 'boot',
  InitBoot: 'init_boot',
  VendorBoot: 'vendor_boot',
  Lk: 'lk',
};

const splitPath = (path: string) => {
  const normalized = path.replace(/\\/g, '/');
  const separator = normalized.lastIndexOf('/');
  if (separator < 0) return { directory: '', fileName: path };
  return {
    directory: path.slice(0, separator + 1),
    fileName: normalized.slice(separator + 1),
  };
};

const formatSize = (sizeBytes: number) => {
  if (sizeBytes < 1024) return `${sizeBytes} B`;
  if (sizeBytes < 1024 * 1024) return `${(sizeBytes / 1024).toFixed(1)} KB`;
  return `${(sizeBytes / (1024 * 1024)).toFixed(1)} MB`;
};

export const QuickFlashPage: FC<{ operationSnapshot?: OperationSnapshotPayload | null }> = ({ operationSnapshot }) => {
  const [preset, setPreset] = useState<QuickFlashPreset>('Boot');
  const [presetImages, setPresetImages] = useState<PresetImages>({
    Boot: null,
    InitBoot: null,
    VendorBoot: null,
    Lk: null,
  });
  const [flashBothSlots, setFlashBothSlots] = useState(false);
  const [switchSlotAfterFlash, setSwitchSlotAfterFlash] = useState(false);
  const [autoReboot, setAutoReboot] = useState(true);
  const [waitForDevice, setWaitForDevice] = useState(true);
  const [pendingPlan, setPendingPlan] = useState<PendingFlashPlan | null>(null);
  const [bootConfirmOpen, setBootConfirmOpen] = useState(false);
  const [isExecuting, setIsExecuting] = useState(false);
  const [errorText, setErrorText] = useState('');
  const image = presetImages[preset];
  const selectedImagePath = image?.path ?? null;
  const targetLabel = (partition: QuickFlashPreset) =>
    flashBothSlots ? `${presetPartitionNames[partition]}_a/b` : presetPartitionNames[partition];
  const selectedRequests = presets.flatMap((item) => {
    const selected = presetImages[item.value];
    return selected ? [{ imagePath: selected.path, partition: item.value }] : [];
  });

  const selectImage = async () => {
    const selectedPreset = preset;
    const imagePath = await open({ multiple: false, directory: false, filters: [{ name: 'Android 镜像', extensions: ['img', 'bin'] }] });
    if (typeof imagePath !== 'string') return;
    setErrorText('');
    try {
      const response = await invoke<FlashImageInfo>('quick_flash_inspect_image', { imagePath });
      setPresetImages((current) => ({ ...current, [selectedPreset]: response }));
    } catch (error) {
      setErrorText(errorMessage(error, '镜像检查失败'));
    }
  };

  const requestFlash = (requests: readonly { imagePath: string; partition: QuickFlashPreset }[]) => {
    if (requests.length === 0 || isExecuting) return;
    setErrorText('');
    setPendingPlan({ requests: [...requests] });
    setBootConfirmOpen(true);
  };

  const executePendingPlan = async () => {
    const plan = pendingPlan;
    if (!plan) return;
    setErrorText('');
    // 先关弹窗再发起刷写：刷写过程由下方的「取消当前操作」与进度面板接管，
    // 确认弹窗停留会挡住操作进度，失败时错误也照样通过 errorText 呈现。
    setBootConfirmOpen(false);
    setPendingPlan(null);
    setIsExecuting(true);
    try {
      await invoke('quick_flash_execute_preset_images', {
        requests: plan.requests,
        autoReboot,
        waitForDevice,
        flashBothSlots,
        switchSlotAfterFlash,
      });
    } catch (error) {
      setErrorText(errorMessage(error, 'boot 刷写失败'));
    } finally {
      setIsExecuting(false);
    }
  };

  const cancelExecution = async () => {
    setErrorText('');
    try {
      await invoke('operation_cancel');
    } catch (error) {
      setErrorText(errorMessage(error, '取消快速刷写失败'));
    }
  };

  return (
    <section className="nw-quick-flash-page" aria-label="快速刷写">
      <header className="nw-quick-flash-heading"><div><h1>快速刷写</h1></div><span><i aria-hidden="true" />等待连接</span></header>
      <section className="nw-quick-flash-preset-panel" aria-label="刷写预设">
        <header><button type="button" className="nw-test-quick-flash-execute-boot" disabled={selectedRequests.length === 0 || isExecuting} onClick={() => requestFlash(selectedRequests)}>开始刷入</button><label><input type="checkbox" checked={autoReboot} disabled={isExecuting} onChange={(event) => setAutoReboot(event.target.checked)} />自动重启</label><label><input type="checkbox" checked={waitForDevice} disabled={isExecuting} onChange={(event) => setWaitForDevice(event.target.checked)} />等待 FB 设备</label><label><input type="checkbox" className="nw-test-quick-flash-dual-slot" checked={flashBothSlots} disabled={isExecuting} onChange={(event) => { const enabled = event.target.checked; setFlashBothSlots(enabled); if (!enabled) setSwitchSlotAfterFlash(false); }} />双刷入双槽</label><label><input type="checkbox" className="nw-test-quick-flash-switch-slot" checked={switchSlotAfterFlash} disabled={!flashBothSlots || isExecuting} onChange={(event) => setSwitchSlotAfterFlash(event.target.checked)} />刷完切槽</label></header>
        <div className="nw-quick-flash-preset-grid">
          {presets.map((item) => { const picked = presetImages[item.value]; const { directory, fileName } = picked ? splitPath(picked.path) : { directory: '', fileName: '' }; return <div className="nw-quick-flash-preset-row" key={item.value}><strong>{item.label}</strong>{picked ? <span className="nw-quick-flash-preset-path" title={picked.path}><i>{directory}</i><b>{fileName}</b></span> : <span className="nw-quick-flash-preset-path nw-quick-flash-preset-path-empty">未选择镜像</span>}<button type="button" className={`nw-test-quick-flash-preset-${item.value}${preset === item.value ? ' nw-test-quick-flash-select-image' : ''}`} aria-pressed={preset === item.value} disabled={isExecuting} onClick={() => { if (preset !== item.value) { setPreset(item.value); return; } void selectImage(); }}>文件</button><button type="button" className={preset === item.value ? 'nw-test-quick-flash-prepare-boot' : ''} disabled={!presetImages[item.value] || isExecuting} onClick={() => requestFlash([{ imagePath: presetImages[item.value]!.path, partition: item.value }])}>刷入</button></div>; })}
        </div>
      </section>
      {image ? <dl className="nw-quick-flash-image" aria-label="镜像元数据"><dt>状态</dt><dd>镜像已就绪</dd><dt>文件</dt><dd title={image.path}>{splitPath(image.path).fileName}</dd><dt>大小</dt><dd>{formatSize(image.size_bytes)}</dd></dl> : <p>尚未选择镜像</p>}
      {errorText ? <p className="nw-error-text">{errorText}</p> : null}
      <footer className="nw-quick-flash-statusbar" aria-label="快速刷写状态">
        {/* 状态文本取全局操作快照：切页往返或操作由其他入口发起时依然准确；
            空闲时点击停止由后端 cancel_current 安全空操作兜底（同线刷页）。 */}
        <span>{operationSnapshot?.isBusy ? operationSnapshot.stage : '等待操作'}</span>
        <button type="button" className="nw-test-quick-flash-cancel" onClick={() => void cancelExecution()}>停止操作</button>
      </footer>
      <ModalLayer isVisible={bootConfirmOpen} title="确认刷写" onClose={() => { if (!isExecuting) { setBootConfirmOpen(false); setPendingPlan(null); } }}>
        <p>将刷写 {pendingPlan?.requests.length ?? 0} 个分区。{flashBothSlots ? ' 双槽刷入。' : null}{flashBothSlots && switchSlotAfterFlash ? ' 刷完切换槽位。' : null}{!autoReboot ? ' 不自动重启。' : null} 确定继续吗？</p>
        <ul className="nw-quick-flash-confirm-list">
          {pendingPlan?.requests.map((request) => (
            <li key={`${request.partition}:${request.imagePath}`} title={request.imagePath}>
              <b className="nw-quick-flash-confirm-file">{splitPath(request.imagePath).fileName}</b>
              <span className="nw-quick-flash-confirm-arrow" aria-hidden="true">---&gt;</span>
              <span className="nw-quick-flash-confirm-target">{targetLabel(request.partition)}</span>
            </li>
          ))}
        </ul>
        <div className="nw-driver-dialog-actions"><button type="button" disabled={isExecuting} onClick={() => { setBootConfirmOpen(false); setPendingPlan(null); }}>取消</button><button type="button" className="nw-test-quick-flash-confirm-boot nw-dialog-confirm" disabled={isExecuting} onClick={() => void executePendingPlan()}>确认刷写</button></div>
      </ModalLayer>
    </section>
  );
};

import { FC, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';

type FirmwareEntry = {
  id: string;
  name: string;
  sizeBytes: number;
};

type FirmwareInspection = {
  format: string;
  entries: FirmwareEntry[];
};

type FirmwareExtraction = {
  images: Array<{
    name: string;
    sizeBytes: number;
    resultId?: string;
  }>;
};

type FirmwareOutputDirectorySelection = {
  selectionId: string;
};

type FirmwareArtifactConfirmation = {
  partition: string;
  taskCount: number;
};

type FirmwareProgress = {
  currentPartition: string | null;
  currentPartitionIndex: number | null;
  totalPartitions: number;
  completedPartitions: number;
  successfulPartitions: number;
  failedPartitions: number;
  skippedPartitions: number;
  bytesCompleted: number;
  bytesTotal: number;
  percentage: number;
  bytesPerSecond: number;
  elapsedMilliseconds: number;
};

const formatLabel = (format: string) => {
  switch (format) {
    case 'vivoGzipTar':
      return 'VIVO 压缩固件';
    case 'zip':
      return 'ZIP 固件包';
    case 'imageDirectory':
      return '镜像目录';
    case 'payload':
      return 'payload 固件';
    default:
      return '固件源';
  }
};

const asSinglePath = (value: string | string[] | null): string | null =>
  typeof value === 'string' ? value : null;

export const FirmwareExtractPage: FC = () => {
  const [sourcePath, setSourcePath] = useState<string | null>(null);
  const [remoteUrl, setRemoteUrl] = useState('');
  const [outputDirectory, setOutputDirectory] = useState<FirmwareOutputDirectorySelection | null>(null);
  const [format, setFormat] = useState('');
  const [entries, setEntries] = useState<FirmwareEntry[]>([]);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [statusText, setStatusText] = useState('未加载 payload');
  const [errorText, setErrorText] = useState('');
  const [isWorking, setIsWorking] = useState(false);
  const [extractedImages, setExtractedImages] = useState<FirmwareExtraction['images']>([]);
  const [pendingArtifact, setPendingArtifact] = useState<
    { artifactId: string; confirmation: FirmwareArtifactConfirmation } | null
  >(null);
  const [progress, setProgress] = useState<FirmwareProgress | null>(null);
  const hasSource = Boolean(sourcePath) || Boolean(remoteUrl.trim());

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void Promise.resolve(
      listen<FirmwareProgress>('firmware:progress', (event) => setProgress(event.payload)),
    ).then((stop) => {
      if (typeof stop === 'function') {
        unlisten = stop;
      }
    });
    return () => unlisten?.();
  }, []);

  const chooseSource = async (directory = false) => {
    setErrorText('');
    const selected = asSinglePath(
      await open(
        directory
          ? { multiple: false, directory: true }
          : {
              multiple: false,
              directory: false,
              filters: [{ name: '固件文件', extensions: ['zip', 'gz', 'bin', 'img', 'zst', 'zstd'] }],
            },
      ),
    );
    if (!selected) {
      return;
    }

    setSourcePath(selected);
    setRemoteUrl('');
    clearInspection();
    setStatusText('已选择本地固件，点击读取信息查看分区。');
  };

  const clearInspection = () => {
    setFormat('');
    setEntries([]);
    setSelectedIds(new Set());
    setExtractedImages([]);
    setPendingArtifact(null);
    setProgress(null);
  };

  const inspectSource = async () => {
    const url = remoteUrl.trim();
    if (!sourcePath && !/^https?:\/\/[^\s/]+/i.test(url)) {
      setErrorText('请输入有效的 HTTP 或 HTTPS 固件地址，或选择本地固件。');
      return;
    }
    setErrorText('');
    setIsWorking(true);
    try {
      const inspection = sourcePath
        ? await invoke<FirmwareInspection>('firmware_inspect_local', { sourcePath })
        : await invoke<FirmwareInspection>('firmware_inspect_remote', { url });
      setFormat(inspection.format);
      setEntries(inspection.entries);
      setSelectedIds(new Set());
      setExtractedImages([]);
      setPendingArtifact(null);
      setProgress(null);
      setStatusText(`${formatLabel(inspection.format)}，已发现 ${inspection.entries.length} 个分区。`);
    } catch {
      clearInspection();
      setErrorText(sourcePath ? '固件检查失败，请确认文件格式。' : '远程固件检查失败，请确认 HTTP 或 HTTPS 地址和文件格式。');
    } finally {
      setIsWorking(false);
    }
  };

  const chooseOutputDirectory = async () => {
    setErrorText('');
    const selected = await invoke<FirmwareOutputDirectorySelection | null>(
      'firmware_select_output_directory',
    );
    if (!selected) {
      return;
    }

    setOutputDirectory(selected);
    setStatusText('已选择提取输出目录。');
  };

  const toggleEntry = (entryId: string) => {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (next.has(entryId)) {
        next.delete(entryId);
      } else {
        next.add(entryId);
      }
      return next;
    });
  };

  const extractSelected = async () => {
    if (selectedIds.size === 0) {
      setErrorText('请先选择需要提取的分区。');
      return;
    }
    if (
      format !== 'vivoGzipTar'
      && format !== 'payload'
      && format !== 'zip'
    ) {
      setErrorText('当前格式暂不支持提取。');
      return;
    }

    setErrorText('');
    const selectedOutputDirectory = outputDirectory ?? await invoke<FirmwareOutputDirectorySelection | null>(
      'firmware_select_output_directory',
    );
    if (!selectedOutputDirectory) {
      return;
    }
    if (!outputDirectory) {
      setOutputDirectory(selectedOutputDirectory);
    }

    setIsWorking(true);
    try {
      const extraction = !sourcePath
        ? await invoke<FirmwareExtraction>('firmware_extract_remote', {
            url: remoteUrl.trim(),
            selectedIds: [...selectedIds],
            outputDirectoryId: selectedOutputDirectory.selectionId,
          })
        : format === 'payload'
          ? await invoke<FirmwareExtraction>('firmware_extract_payload_local', {
              selectedIds: [...selectedIds],
              outputDirectoryId: selectedOutputDirectory.selectionId,
            })
          : await invoke<FirmwareExtraction>('firmware_extract_vivo_local', {
              sourcePath,
              selectedIds: [...selectedIds],
              outputDirectoryId: selectedOutputDirectory.selectionId,
            });
      setStatusText(`已提取 ${extraction.images.length} 个镜像。`);
      setExtractedImages(extraction.images);
      setPendingArtifact(null);
      setProgress(null);
    } catch {
      setErrorText('固件提取失败，请重试。');
    } finally {
      setIsWorking(false);
    }
  };

  const cancel = async () => {
    await invoke('operation_cancel');
  };

  const prepareExtractedImageForFlash = async (resultId: string) => {
    setErrorText('');
    setIsWorking(true);
    try {
      const artifact = await invoke<{ artifactId: string }>('firmware_prepare_extracted_artifact', {
        resultId,
      });
      const confirmation = await invoke<FirmwareArtifactConfirmation>(
        'quick_flash_prepare_firmware_artifact',
        { artifactId: artifact.artifactId },
      );
      setPendingArtifact({ artifactId: artifact.artifactId, confirmation });
    } catch {
      setErrorText('镜像刷写准备失败，请重新提取。');
    } finally {
      setIsWorking(false);
    }
  };

  const executePreparedArtifact = async () => {
    if (!pendingArtifact) {
      return;
    }
    setErrorText('');
    setIsWorking(true);
    try {
      await invoke('quick_flash_execute_firmware_artifact', {
        artifactId: pendingArtifact.artifactId,
      });
      setPendingArtifact(null);
      setStatusText('镜像刷写已完成。');
    } catch {
      setErrorText('镜像刷写失败，请检查设备状态后重试。');
    } finally {
      setIsWorking(false);
    }
  };

  return (
    <section className="nw-firmware-extract-page" aria-label="固件提取">
      <header className="nw-firmware-extract-heading">
        <div>
          <p className="nw-page-eyebrow">FIRMWARE / PAYLOAD</p>
          <h1>固件提取</h1>
          <p>解包本地 payload.bin / OTA 包并提取分区镜像</p>
        </div>
        <span className="nw-firmware-tool-chip"><i />payload_dumper 就绪</span>
      </header>

      <section className="nw-firmware-extract-workbench">
        <header className="nw-firmware-source-fields">
          <label>
            <span>固件来源</span>
            <input
              aria-label="固件来源"
              className="nw-test-firmware-source"
              type="text"
              inputMode="url"
              value={sourcePath ? '已选择本地固件' : remoteUrl}
              readOnly={Boolean(sourcePath)}
              onChange={(event) => {
                setSourcePath(null);
                setRemoteUrl(event.target.value);
                clearInspection();
                setErrorText('');
                setStatusText('请输入 HTTP 或 HTTPS 固件地址后读取信息。');
              }}
              placeholder="输入 http:// 或 https:// 地址，或选择本地固件"
              disabled={isWorking}
            />
          </label>
          <button
            type="button"
            className="nw-test-firmware-select"
            onClick={() => void chooseSource()}
            disabled={isWorking}
          >
            选择本地文件
          </button>
          <label>
            <span>输出路径</span>
            <input
              aria-label="提取输出目录"
              value={outputDirectory ? '已选择目录' : '提取时选择目录'}
              readOnly
            />
          </label>
          <button
            type="button"
            className="nw-test-firmware-output-directory"
            onClick={() => void chooseOutputDirectory()}
            disabled={isWorking}
          >
            选择输出目录
          </button>
        </header>
        <div className="nw-firmware-partition-heading">
          <span>分区表</span>
          <span>{entries.length} 个分区</span>
        </div>
        {entries.length > 0 ? (
          <div className="nw-firmware-entry-list" role="list">
            {entries.map((entry) => (
              <label key={entry.id} className="nw-firmware-entry" role="listitem">
                <input
                  type="checkbox"
                  className="nw-test-firmware-entry"
                  checked={selectedIds.has(entry.id)}
                  onChange={() => toggleEntry(entry.id)}
                />
                <span>{entry.name}</span>
                <span>{entry.sizeBytes} B</span>
              </label>
            ))}
          </div>
        ) : (
          <div className="nw-firmware-partition-empty">
            <span className="nw-firmware-empty-icon">+</span>
            <strong>尚未读取分区</strong>
            <p>选择固件来源后查看分区信息</p>
          </div>
        )}
      </section>

      <footer className="nw-firmware-statusbar">
        <div>
          <strong className="nw-firmware-status">{statusText}</strong>
          {isWorking && progress ? (
            <p className="nw-firmware-progress" aria-live="polite">
              {progress.currentPartition
                ? `${progress.currentPartition} (${progress.currentPartitionIndex ?? '--'}/${progress.totalPartitions || '--'}) `
                : `${progress.completedPartitions}/${progress.totalPartitions || '--'} 个分区 `}
              成功 {progress.successfulPartitions} · 失败 {progress.failedPartitions} · 跳过 {progress.skippedPartitions} ·
              {progress.percentage.toFixed(1)}% {Math.round(progress.bytesPerSecond)} B/s {Math.round(progress.elapsedMilliseconds / 1000)} s
            </p>
          ) : (
            <p>速度 --　　耗时 --</p>
          )}
        </div>
        <div className="nw-firmware-actions">
          <button
            type="button"
            className="nw-test-firmware-inspect"
            onClick={() => void inspectSource()}
            disabled={isWorking || !hasSource}
          >
            读取信息
          </button>
          <button
            type="button"
            className="nw-test-firmware-extract"
            onClick={() => void extractSelected()}
            disabled={isWorking || !format || selectedIds.size === 0}
          >
            提取文件
          </button>
          <button
            type="button"
            className="nw-test-firmware-cancel"
            onClick={() => void cancel()}
            disabled={!isWorking}
          >
            停止操作
          </button>
        </div>
      </footer>
      {errorText ? <p className="nw-error-text">{errorText}</p> : null}
      {extractedImages.length > 0 ? (
        <div className="nw-firmware-entry-list" role="list">
          {extractedImages.map((image, index) => (
            <div key={`${image.name}-${index}`} className="nw-firmware-entry" role="listitem">
              <span>{image.name}</span>
              <span>{image.sizeBytes} B</span>
              {image.resultId ? (
                <button
                  type="button"
                  className="nw-test-firmware-flash"
                  disabled={isWorking}
                  onClick={() => void prepareExtractedImageForFlash(image.resultId!)}
                >
                  刷入此镜像
                </button>
              ) : null}
            </div>
          ))}
        </div>
      ) : null}
      {pendingArtifact ? (
        <div className="nw-confirm-modal" role="dialog" aria-modal="true">
          <p>确认刷入 {pendingArtifact.confirmation.partition} 分区（{pendingArtifact.confirmation.taskCount} 个任务）？</p>
          <div className="nw-page-actions">
            <button type="button" onClick={() => setPendingArtifact(null)} disabled={isWorking}>
              取消
            </button>
            <button
              type="button"
              className="nw-test-firmware-confirm-flash"
              onClick={() => void executePreparedArtifact()}
              disabled={isWorking}
            >
              确认刷入
            </button>
          </div>
        </div>
      ) : null}
    </section>
  );
};

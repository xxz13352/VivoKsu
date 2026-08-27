import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { FirmwareExtractPage } from './FirmwareExtractPage';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => undefined),
}));

import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

type Unmount = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: Unmount;
const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const setInputValue = (input: HTMLInputElement, value: string) => {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, value);
  input.dispatchEvent(new Event('input', { bubbles: true }));
};
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const start = Date.now();
  while (!predicate() && Date.now() - start < timeoutMs) {
    await flushPromises();
  }
  if (!predicate()) {
    throw new Error('timeout waiting for async assertion');
  }
};

const renderFirmwareExtract = () => {
  flushSync(() => {
    root.render(<FirmwareExtractPage />);
  });
};

const inspectSelectedLocalSource = async () => {
  (host.querySelector('.nw-test-firmware-select') as HTMLButtonElement).click();
  await waitUntil(() => (
    (host.querySelector('.nw-test-firmware-source') as HTMLInputElement | null)?.value === '已选择本地固件'
  ));
  (host.querySelector('.nw-test-firmware-inspect') as HTMLButtonElement).click();
  await waitUntil(() => host.querySelector('.nw-test-firmware-entry') !== null);
};

const inspectRemoteSource = async (url: string) => {
  const input = host.querySelector('.nw-test-firmware-source') as HTMLInputElement;
  setInputValue(input, url);
  await waitUntil(() => input.value === url);
  (host.querySelector('.nw-test-firmware-inspect') as HTMLButtonElement).click();
  await waitUntil(() => host.querySelector('.nw-test-firmware-entry') !== null);
};

describe('FirmwareExtractPage', () => {
  beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(() => {
    flushSync(() => {
      root.unmount();
    });
    host.remove();
    vi.resetAllMocks();
  });

  test('renders the WPF payload workbench idle state without exposing a local path', () => {
    renderFirmwareExtract();

    expect(host.querySelector('.nw-firmware-extract-heading .nw-page-eyebrow')?.textContent).toBe('FIRMWARE / PAYLOAD');
    expect(host.querySelector('.nw-firmware-extract-workbench')).not.toBeNull();
    expect(host.querySelector('[aria-label="固件来源"]')).not.toBeNull();
    expect(host.querySelector('.nw-firmware-partition-empty strong')?.textContent).toBe('尚未读取分区');
    expect(host.querySelector('.nw-firmware-statusbar strong')?.textContent).toBe('未加载 payload');
    expect(host.textContent ?? '').not.toContain('C:\\private');
  });

  test('选择本地来源后由读取信息按钮再发起检查', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValue('C:\\private\\firmware\\vivo_ota.gz');
    command.mockResolvedValue({ format: 'vivoGzipTar', entries: [{ id: '0', name: 'boot.img', sizeBytes: 4 }] });

    renderFirmwareExtract();
    (host.querySelector('.nw-test-firmware-select') as HTMLButtonElement).click();
    await waitUntil(() => (
      (host.querySelector('.nw-test-firmware-source') as HTMLInputElement | null)?.value === '已选择本地固件'
    ));

    expect(command).not.toHaveBeenCalled();
    (host.querySelector('.nw-test-firmware-inspect') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-firmware-entry') !== null);
    expect(command).toHaveBeenCalledWith('firmware_inspect_local', {
      sourcePath: 'C:\\private\\firmware\\vivo_ota.gz',
    });
  });

  test('操作栏始终提供读取信息、提取文件和停止操作', () => {
    renderFirmwareExtract();

    expect(host.querySelector('.nw-test-firmware-inspect')?.textContent).toBe('读取信息');
    expect(host.querySelector('.nw-test-firmware-extract')?.textContent).toBe('提取文件');
    expect(host.querySelector('.nw-test-firmware-cancel')?.textContent).toBe('停止操作');
    expect((host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).disabled).toBe(true);
  });

  test('选择本地固件后只展示路径安全的分区元数据', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValue('C:\\private\\firmware\\vivo_ota.gz');
    command.mockResolvedValue({
      format: 'vivoGzipTar',
      entries: [{ id: '0', name: 'boot.img', sizeBytes: 4 }],
    });

    renderFirmwareExtract();
    await inspectSelectedLocalSource();

    expect(command).toHaveBeenCalledWith('firmware_inspect_local', {
      sourcePath: 'C:\\private\\firmware\\vivo_ota.gz',
    });
    expect(host.textContent ?? '').not.toContain('C:\\private\\firmware');
    expect(host.textContent ?? '').not.toContain('release/images');
  });

  test('本地固件选择器允许 zst 与 zstd 压缩包', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValue('C:\\private\\firmware\\vivo_ota.zst');
    command.mockResolvedValue({ format: 'unknown', entries: [] });

    renderFirmwareExtract();
    (host.querySelector('.nw-test-firmware-select') as HTMLButtonElement).click();

    await waitUntil(() => dialog.mock.calls.length === 1);
    expect(dialog).toHaveBeenCalledWith({
      multiple: false,
      directory: false,
      filters: [{ name: '固件文件', extensions: ['zip', 'gz', 'bin', 'img', 'zst', 'zstd'] }],
    });
  });

  test('提取选择的 VIVO 分区时不向页面返回输出路径', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValueOnce('C:\\private\\firmware\\vivo_ota.gz');
    command
      .mockResolvedValueOnce({
        format: 'vivoGzipTar',
        entries: [{ id: '0', name: 'boot.img', sizeBytes: 4 }],
      })
      .mockResolvedValueOnce({
        selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
      })
      .mockResolvedValueOnce({ images: [{ name: 'boot.img', sizeBytes: 4 }] });

    renderFirmwareExtract();
    await inspectSelectedLocalSource();
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('已提取 1 个镜像') ?? false);

    expect(command).toHaveBeenNthCalledWith(3, 'firmware_extract_vivo_local', {
      sourcePath: 'C:\\private\\firmware\\vivo_ota.gz',
      selectedIds: ['0'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
    expect(host.textContent ?? '').not.toContain('C:\\private\\output');
  });

  test('目录选择只保存提取输出目录，不能作为固件来源进行检查', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValue({
      selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });

    renderFirmwareExtract();
    (host.querySelector('.nw-test-firmware-output-directory') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('已选择提取输出目录') ?? false);

    expect(dialog).not.toHaveBeenCalled();
    expect(command).toHaveBeenCalledWith('firmware_select_output_directory');
    expect(host.textContent ?? '').not.toContain('C:\\private\\output');
  });

  test('输出目录由 Rust 原生选择命令签发 capability 且页面不显示路径', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValue({
      selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });

    renderFirmwareExtract();
    (host.querySelector('.nw-test-firmware-output-directory') as HTMLButtonElement).click();
    await waitUntil(() => (
      host.querySelector('[aria-label="提取输出目录"]') as HTMLInputElement
    ).value === '已选择目录');

    expect(command).toHaveBeenCalledWith('firmware_select_output_directory');
    expect(dialog).not.toHaveBeenCalled();
    expect(host.textContent ?? '').not.toContain('C:\\private\\output');
    expect((host.querySelector('[aria-label="提取输出目录"]') as HTMLInputElement).value).toBe('已选择目录');
  });

  test('取消重新选择输出目录不会清除页面当前 capability', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({
        selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
      })
      .mockResolvedValueOnce(null);

    renderFirmwareExtract();
    const selectButton = host.querySelector('.nw-test-firmware-output-directory') as HTMLButtonElement;
    selectButton.click();
    await waitUntil(() => (
      host.querySelector('[aria-label="提取输出目录"]') as HTMLInputElement
    ).value === '已选择目录');
    selectButton.click();
    await waitUntil(() => command.mock.calls.length === 2);

    expect((host.querySelector('[aria-label="提取输出目录"]') as HTMLInputElement).value).toBe('已选择目录');
  });

  test('远程提取只提交 capability ID 而不提交 Rust 返回的原始目录路径', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockImplementation((name: string) => {
      if (name === 'firmware_inspect_remote') {
        return Promise.resolve({
          format: 'zip',
          entries: [{ id: '4', name: 'boot', sizeBytes: 4 }],
        });
      }
      if (name === 'firmware_select_output_directory') {
        return Promise.resolve({
          selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
        });
      }
      if (name === 'firmware_extract_remote') {
        return Promise.resolve({ images: [{ name: 'boot.img', sizeBytes: 4 }] });
      }
      return Promise.reject(new Error(`unexpected command: ${name}`));
    });

    renderFirmwareExtract();
    await inspectRemoteSource('https://firmware.example.test/ota.zip');
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已提取 1 个镜像') ?? false);

    expect(command).toHaveBeenCalledWith('firmware_select_output_directory');
    expect(command).toHaveBeenCalledWith('firmware_extract_remote', {
      url: 'https://firmware.example.test/ota.zip',
      selectedIds: ['4'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
    const remoteArgs = command.mock.calls.find(([name]) => name === 'firmware_extract_remote')?.[1];
    expect(remoteArgs).not.toHaveProperty('outputDirectory');
    expect(JSON.stringify(remoteArgs)).not.toContain('C:\\\\private\\\\output');
  });

  test('Rust 选择 command 保持本地提取 UX 且 IPC 只提交 capability ID', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValue('C:\\private\\firmware\\vivo_ota.gz');
    command.mockImplementation((name: string) => {
      if (name === 'firmware_inspect_local') {
        return Promise.resolve({
          format: 'vivoGzipTar',
          entries: [{ id: '0', name: 'boot.img', sizeBytes: 4 }],
        });
      }
      if (name === 'firmware_select_output_directory') {
        return Promise.resolve({
          selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
        });
      }
      if (name === 'firmware_extract_vivo_local') {
        return Promise.resolve({ images: [{ name: 'boot.img', sizeBytes: 4 }] });
      }
      return Promise.reject(new Error(`unexpected command: ${name}`));
    });

    renderFirmwareExtract();
    await inspectSelectedLocalSource();
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已提取 1 个镜像') ?? false);

    expect(command).toHaveBeenCalledWith('firmware_extract_vivo_local', {
      sourcePath: 'C:\\private\\firmware\\vivo_ota.gz',
      selectedIds: ['0'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
    const localArgs = command.mock.calls.find(([name]) => name === 'firmware_extract_vivo_local')?.[1];
    expect(localArgs).not.toHaveProperty('outputDirectory');
    expect(JSON.stringify(localArgs)).not.toContain('C:\\\\private\\\\output');
  });

  test('远程重复提取复用当前有效目录 capability', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockImplementation((name: string) => {
      if (name === 'firmware_inspect_remote') {
        return Promise.resolve({ format: 'zip', entries: [{ id: '4', name: 'boot', sizeBytes: 4 }] });
      }
      if (name === 'firmware_select_output_directory') {
        return Promise.resolve({
          selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
        });
      }
      if (name === 'firmware_extract_remote') {
        return Promise.resolve({ images: [{ name: 'boot.img', sizeBytes: 4 }] });
      }
      return Promise.reject(new Error(`unexpected command: ${name}`));
    });

    renderFirmwareExtract();
    await inspectRemoteSource('https://firmware.example.test/ota.zip');
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();
    await waitUntil(() => command.mock.calls.filter(([name]) => name === 'firmware_extract_remote').length === 1);
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();
    await waitUntil(() => command.mock.calls.filter(([name]) => name === 'firmware_extract_remote').length === 2);

    const remoteCalls = command.mock.calls.filter(([name]) => name === 'firmware_extract_remote');
    expect(remoteCalls[0][1].outputDirectoryId).toBe('firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8');
    expect(remoteCalls[1][1].outputDirectoryId).toBe(remoteCalls[0][1].outputDirectoryId);
    expect(command.mock.calls.filter(([name]) => name === 'firmware_select_output_directory')).toHaveLength(1);
  });

  test('HTTP(S) 模式检查签名 URL 时只提交 URL，不在页面状态文本中回显 URL', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValue({
      format: 'zip',
      entries: [{ id: '4', name: 'boot', sizeBytes: 4 }],
    });

    renderFirmwareExtract();
    await inspectRemoteSource('https://firmware.example.test/ota.zip?token=secret');

    expect(command).toHaveBeenCalledWith('firmware_inspect_remote', {
      url: 'https://firmware.example.test/ota.zip?token=secret',
    });
    expect(host.textContent ?? '').not.toContain('token=secret');
  });

  test('HTTP(S) 直接镜像 ZIP 提取使用远程命令和已检查 ID', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({
        format: 'zip',
        entries: [{ id: '4', name: 'boot', sizeBytes: 4 }],
      })
      .mockResolvedValueOnce({
        selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
      })
      .mockResolvedValueOnce({ images: [{ name: 'boot.img', sizeBytes: 4 }] });

    renderFirmwareExtract();
    await inspectRemoteSource('https://firmware.example.test/ota.zip');
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('已提取 1 个镜像') ?? false);

    expect(command).toHaveBeenNthCalledWith(3, 'firmware_extract_remote', {
      url: 'https://firmware.example.test/ota.zip',
      selectedIds: ['4'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
  });

  test('HTTP(S) payload 固件提取仍使用远程 payload 命令', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({
        format: 'payload',
        entries: [{ id: '2', name: 'vendor', sizeBytes: 8 }],
      })
      .mockResolvedValueOnce({
        selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
      })
      .mockResolvedValueOnce({ images: [{ name: 'vendor.img', sizeBytes: 8 }] });

    renderFirmwareExtract();
    await inspectRemoteSource('https://firmware.example.test/payload.bin?sig=abc');
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('已提取 1 个镜像') ?? false);
    expect(command).toHaveBeenNthCalledWith(3, 'firmware_extract_remote', {
      url: 'https://firmware.example.test/payload.bin?sig=abc',
      selectedIds: ['2'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
  });

  test('HTTP 输入调用远程检查命令', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command.mockResolvedValue({ format: 'payload', entries: [{ id: '0', name: 'boot', sizeBytes: 4 }] });

    renderFirmwareExtract();
    await inspectRemoteSource('http://firmware.example.test/ota.zip');
    expect(command).toHaveBeenCalledWith('firmware_inspect_remote', {
      url: 'http://firmware.example.test/ota.zip',
    });
  });

  test('统一来源输入会保留单一来源控件和空分区状态', () => {
    renderFirmwareExtract();

    const source = host.querySelector('.nw-test-firmware-source') as HTMLInputElement;
    setInputValue(source, 'https://firmware.example.test/ota.zip');

    expect(source.value).toBe('https://firmware.example.test/ota.zip');
    expect(host.querySelector('.nw-firmware-partition-empty strong')?.textContent).toBe('尚未读取分区');
  });

  test('提取普通 ZIP 镜像时只提交已检查的 opaque ID 与输出目录', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValueOnce('C:\\private\\firmware\\ota.zip');
    command
      .mockResolvedValueOnce({
        format: 'zip',
        entries: [{ id: '0', name: 'boot.img', sizeBytes: 0 }],
      })
      .mockResolvedValueOnce({
        selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
      })
      .mockResolvedValueOnce({ images: [{ name: 'boot.img', sizeBytes: 4 }] });

    renderFirmwareExtract();
    await inspectSelectedLocalSource();
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('已提取 1 个镜像') ?? false);
    expect(command).toHaveBeenNthCalledWith(3, 'firmware_extract_vivo_local', {
      sourcePath: 'C:\\private\\firmware\\ota.zip',
      selectedIds: ['0'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
    expect(host.textContent ?? '').not.toContain('C:\\private\\firmware');
    expect(host.textContent ?? '').not.toContain('C:\\private\\output');
  });

  test('提取 payload 分区时只提交已读取的不透明 ID 和输出目录', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValueOnce('C:\\private\\firmware\\payload.bin');
    command
      .mockResolvedValueOnce({
        format: 'payload',
        entries: [{ id: '0', name: 'boot', sizeBytes: 4 }],
      })
      .mockResolvedValueOnce({
        selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
      })
      .mockResolvedValueOnce({ images: [{ name: 'boot.img', sizeBytes: 9 }] });

    renderFirmwareExtract();
    await inspectSelectedLocalSource();
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();

    await waitUntil(() => host.textContent?.includes('已提取 1 个镜像') ?? false);

    expect(command).toHaveBeenNthCalledWith(3, 'firmware_extract_payload_local', {
      selectedIds: ['0'],
      outputDirectoryId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
    });
    expect(host.textContent ?? '').not.toContain('C:\\private\\firmware');
    expect(host.textContent ?? '').not.toContain('C:\\private\\output');
  });

  test('停止提取调用统一 operation_cancel', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValue('C:\\private\\firmware\\vivo_ota.gz');
    command.mockImplementation(() => new Promise(() => undefined));

    renderFirmwareExtract();
    (host.querySelector('.nw-test-firmware-select') as HTMLButtonElement).click();
    await waitUntil(() => (
      (host.querySelector('.nw-test-firmware-source') as HTMLInputElement | null)?.value === '已选择本地固件'
    ));
    (host.querySelector('.nw-test-firmware-inspect') as HTMLButtonElement).click();
    await waitUntil(
      () => !(host.querySelector('.nw-test-firmware-cancel') as HTMLButtonElement).disabled,
    );
    (host.querySelector('.nw-test-firmware-cancel') as HTMLButtonElement).click();

    await waitUntil(() => command.mock.calls.length === 2);
    expect(command).toHaveBeenNthCalledWith(2, 'operation_cancel');
  });

  test('提取结果必须经不透明工件预检和显式确认后才刷入', async () => {
    const dialog = open as unknown as ReturnType<typeof vi.fn>;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    dialog.mockResolvedValueOnce('C:\\private\\firmware\\vivo_ota.gz');
    command
      .mockResolvedValueOnce({
        format: 'vivoGzipTar',
        entries: [{ id: '0', name: 'boot.img', sizeBytes: 4 }],
      })
      .mockResolvedValueOnce({
        selectionId: 'firmware-output-8d03772f-0062-4cc1-92ec-b10c85e75ca8',
      })
      .mockResolvedValueOnce({
        images: [{ name: 'boot.img', sizeBytes: 4, resultId: 'result-1' }],
      })
      .mockResolvedValueOnce({ artifactId: 'firmware-1', name: 'boot.img', sizeBytes: 4 })
      .mockResolvedValueOnce({ partition: 'boot', taskCount: 1 })
      .mockResolvedValueOnce({});

    renderFirmwareExtract();
    await inspectSelectedLocalSource();
    (host.querySelector('.nw-test-firmware-entry') as HTMLInputElement).click();
    (host.querySelector('.nw-test-firmware-extract') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-firmware-flash') !== null);

    (host.querySelector('.nw-test-firmware-flash') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-firmware-confirm-flash') !== null);
    expect(command).toHaveBeenNthCalledWith(4, 'firmware_prepare_extracted_artifact', {
      resultId: 'result-1',
    });
    expect(command).toHaveBeenNthCalledWith(5, 'quick_flash_prepare_firmware_artifact', {
      artifactId: 'firmware-1',
    });

    (host.querySelector('.nw-test-firmware-confirm-flash') as HTMLButtonElement).click();
    await waitUntil(() => command.mock.calls.length === 6);
    expect(command).toHaveBeenNthCalledWith(6, 'quick_flash_execute_firmware_artifact', {
      artifactId: 'firmware-1',
    });
    expect(host.textContent ?? '').not.toContain('C:\\private');
  });
});

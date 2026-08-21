import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { RootPage } from './RootPage';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';

const OTA_CHECK_OFF = { available: false, label: null };
const OTA_CHECK_ON = { available: true, label: 'PD2417 OTA' };

type RootHandle = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: RootHandle;
const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const start = Date.now();
  while (!predicate() && Date.now() - start < timeoutMs) {
    await flushPromises();
  }
  if (!predicate()) {
    throw new Error('timeout waiting for async assertion');
  }
};

const renderRoot = () => {
  flushSync(() => {
    root.render(<RootPage />);
  });
};

describe('RootPage', () => {
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
    vi.clearAllMocks();
  });

  test('自动 KMI 默认开启，与 C# ROOT 工作流保持一致', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: null })
      .mockResolvedValueOnce(OTA_CHECK_OFF);

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);

    expect((host.querySelector('.nw-test-root-auto-kmi') as HTMLInputElement).checked).toBe(true);
  });

  test('ROOT 执行中提供取消操作并调用统一取消命令', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    let resolveAutomatic: ((value: unknown) => void) | undefined;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: null })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ id: 'root-image-init_boot-cancel', kind: 'initBoot', fileName: 'init_boot.img', partitionName: 'init_boot', sizeBytes: 1024 })
      .mockResolvedValueOnce({ managerLabel: 'Vivo KSU', effectiveKmi: 'android14-6.1', canPatch: true, canRunAutomatic: true, summary: 'ROOT 前置条件已就绪' })
      .mockImplementationOnce(() => new Promise((resolve) => { resolveAutomatic = resolve; }));

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);
    (host.querySelector('.nw-test-root-preflight') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('ROOT 前置条件已就绪') ?? false);
    (host.querySelector('.nw-test-root-automatic') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('.nw-test-root-cancel') !== null);

    (host.querySelector('.nw-test-root-cancel') as HTMLButtonElement).click();
    await waitUntil(() => command.mock.calls.some(([name]) => name === 'operation_cancel'));
    expect(command).toHaveBeenCalledWith('operation_cancel');
    resolveAutomatic?.({ flashedPartitionCount: 0, commandCount: 0, status: '已取消' });
  });

  test('renders the WPF ROOT workbench idle structure', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: null })
      .mockResolvedValueOnce(OTA_CHECK_OFF);

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-workbench') !== null);

    expect(host.querySelector('.nw-root-heading .nw-page-eyebrow')?.textContent).toBe('VIVO / ROOT WORKFLOW');
    expect(host.querySelector('.nw-root-heading h1')?.textContent).toBe('Vivo ROOT');
    expect(host.querySelector('.nw-root-image-preflight h2')?.textContent).toBe('启动镜像预检');
    expect(host.querySelector('.nw-root-status-chip')?.textContent).toContain('等待选择启动镜像');
    expect(host.querySelector('.nw-root-manager-kmi')).not.toBeNull();
    expect(host.querySelector('.nw-root-actions strong')?.textContent).toBe('KSU / 镜像修补');
  });

  test('加载成功时展示会话与版本信息', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      update_required: false,
      force_update: false,
      latest: '2.0.0',
      min_version: '1.0.0',
      download_url: 'https://example.com/fw',
    });
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      has_token: true,
      healthy: true,
      running: true,
      session_id: 'root-session',
    });
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(OTA_CHECK_OFF);

    renderRoot();

    await waitUntil(() => {
      const version = host.querySelector('.nw-root-version') as HTMLDivElement | null;
      return version !== null && version.textContent?.includes('2.0.0');
    });

    expect(invoke).toHaveBeenCalledWith('version_check');
    expect(invoke).toHaveBeenCalledWith('session_state');
    const version = host.querySelector('.nw-root-version') as HTMLDivElement;
    expect(version).not.toBeNull();
    expect(version.textContent ?? '').toContain('最低: 1.0.0');
    expect(host.textContent ?? '').not.toContain('root-session');
  });

  test('刷新按钮可再次触发两个命令', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      update_required: true,
      force_update: false,
      latest: null,
      min_version: null,
      download_url: null,
    });
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue({
      has_token: false,
      healthy: false,
      running: false,
      session_id: null,
    });
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(OTA_CHECK_OFF);

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);

    const button = host.querySelector('.nw-test-root-refresh') as HTMLButtonElement;
    button.click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length >= 6);
    expect((invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBeGreaterThanOrEqual(6);
  });

  test('会话失败时回退为空态并展示错误', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValue(new Error('root rpc failed'));

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-error-text') !== null);

    const err = host.querySelector('.nw-error-text') as HTMLParagraphElement;
    expect(err).not.toBeNull();
    expect(err.textContent).toBe('root rpc failed');
    expect(host.querySelector('.nw-root-version')).toBeNull();
  });

  test('ROOT 预检只提交不透明镜像 ID 并显示路径安全元数据', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({
        update_required: false,
        force_update: false,
        latest: null,
        min_version: null,
        download_url: null,
      })
      .mockResolvedValueOnce({
        has_token: true,
        healthy: true,
        running: false,
        session_id: 'root-session',
      })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({
        id: 'root-image-init_boot-1',
        kind: 'initBoot',
        fileName: 'init_boot.img',
        sizeBytes: 1024,
      })
      .mockResolvedValueOnce({
        managerLabel: 'Vivo KSU',
        effectiveKmi: 'android14-6.1',
        canPatch: true,
        canRunAutomatic: true,
        summary: '已就绪：将修补 init_boot。',
      });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);

    (host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);

    // This regression keeps the pre-existing manual-KMI contract explicit now
    // that the page defaults to automatic KMI detection.
    (host.querySelector('.nw-test-root-auto-kmi') as HTMLInputElement).click();
    (host.querySelector('.nw-test-root-preflight') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已就绪：将修补 init_boot。') ?? false);

    expect(invoke).toHaveBeenCalledWith('root_select_image', { kind: 'initBoot' });
    expect(invoke).toHaveBeenCalledWith('root_preflight', {
      options: expect.objectContaining({
        manager: 'vivoKsu',
        initBootId: 'root-image-init_boot-1',
        vendorBootId: null,
        useAutomaticKmi: false,
        selectedKmi: 'android14-6.1',
      }),
    });
    expect(host.textContent ?? '').not.toContain('C:\\');
  });

  test('自动 KMI 不向 Rust 提交浏览器提供的 Kernel release 或手动 KMI', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ id: 'root-image-init_boot-2', kind: 'initBoot', fileName: 'init_boot.img',
        partitionName: 'init_boot', sizeBytes: 1024 })
      .mockResolvedValueOnce({ managerLabel: 'Vivo KSU', effectiveKmi: 'android14-6.1', canPatch: true, canRunAutomatic: true, summary: '已就绪：将修补 init_boot。' });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);
    (host.querySelector('.nw-test-root-preflight') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('已就绪：将修补 init_boot。') ?? false);

    expect(invoke).toHaveBeenCalledWith('root_preflight', {
      options: expect.objectContaining({
        useAutomaticKmi: true,
        selectedKmi: null,
      }),
    });
  });

  test('只为当前管理器请求受控的已验证 APK 资源', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce(['manager-KSU']);

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('.nw-test-root-install-manager') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('管理器资源已就绪') ?? false);

    expect(invoke).toHaveBeenCalledWith('resource_install', { keys: ['manager-KSU'] });
  });

  test('设备安装只提交受控管理器枚举且不渲染 APK 路径', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ managerLabel: 'Vivo KSU', summary: 'Vivo KSU 管理器已安装并启动。' });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('.nw-test-root-install-device-manager') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('Vivo KSU 管理器已安装并启动。') ?? false);

    expect(invoke).toHaveBeenCalledWith('root_install_manager', { manager: 'vivoKsu' });
    expect(host.textContent ?? '').not.toContain('C:\\');
  });

  test('Vivo KSU 修补结果必须经不透明工件预检和显式确认后才移交快速刷写', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ id: 'root-image-init_boot-9', kind: 'initBoot', fileName: 'init_boot.img',
        partitionName: 'init_boot', sizeBytes: 1024 })
      .mockResolvedValueOnce({ artifactId: 'root-patch-init_boot-9', partition: 'init_boot', fileName: 'patched_init_boot.img', sizeBytes: 1028 })
      .mockResolvedValueOnce({ partition: 'init_boot', taskCount: 1 })
      .mockResolvedValueOnce({ commandCount: 1, executedCount: 1 });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);
    (host.querySelector('.nw-test-root-auto-kmi') as HTMLInputElement).click();
    (host.querySelector('.nw-test-root-patch') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('patched_init_boot.img') ?? false);

    expect(command).toHaveBeenCalledWith('root_patch_vivo_ksu', {
      options: {
        initBootId: 'root-image-init_boot-9',
        useAutomaticKmi: false,
        selectedKmi: 'android14-6.1',
      },
    });
    expect(host.textContent ?? '').not.toContain('C:\\');

    (host.querySelector('.nw-test-root-transfer') as HTMLButtonElement).click();
    await waitUntil(() => host.querySelector('[role="dialog"]') !== null);
    expect(command).toHaveBeenCalledWith('root_prepare_patched_artifact_flash', {
      artifactId: 'root-patch-init_boot-9',
    });
    expect(command).not.toHaveBeenCalledWith('root_execute_patched_artifact_flash', expect.anything());

    (host.querySelector('.nw-test-root-confirm') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('ROOT 修补镜像刷写已完成。') ?? false);
    expect(command).toHaveBeenCalledWith('root_execute_patched_artifact_flash', {
      artifactId: 'root-patch-init_boot-9',
    });
  });

  test('全自动 ROOT 从一次用户动作受控串联安装、修补与 fastbootd 刷写', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ id: 'root-image-init_boot-auto', kind: 'initBoot', fileName: 'init_boot.img',
        partitionName: 'init_boot', sizeBytes: 1024 })
      .mockResolvedValueOnce({ managerLabel: 'Vivo KSU', effectiveKmi: 'android14-6.1', canPatch: true, canRunAutomatic: true, summary: 'ROOT 前置条件已就绪' })
      .mockResolvedValueOnce({ flashedPartitionCount: 1, commandCount: 2, status: 'ROOT 全自动流程已完成。' });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);
    (host.querySelector('.nw-test-root-preflight') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('ROOT 前置条件已就绪') ?? false);

    (host.querySelector('.nw-test-root-automatic') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('ROOT 全自动流程已完成。') ?? false);

    expect(command).toHaveBeenCalledWith('root_run_automatic', {
      options: {
        manager: 'vivoKsu',
        initBootId: 'root-image-init_boot-auto',
        vendorBootId: null,
        useAutomaticKmi: true,
        selectedKmi: null,
      },
    });
    expect(command).not.toHaveBeenCalledWith('root_install_manager', expect.anything());
    expect(command).not.toHaveBeenCalledWith('root_patch_vivo_ksu', expect.anything());
    expect(command).not.toHaveBeenCalledWith('root_execute_automatic_artifacts', expect.anything());
    expect(host.textContent).not.toContain('init_boot.img');
    expect(host.textContent).toContain('等待选择启动镜像');
    expect((host.querySelector('.nw-test-root-automatic') as HTMLButtonElement).disabled).toBe(true);
  });

  test('官方 KernelSU 只将 vendor_boot 不透明 ID 提交给修补命令', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ id: 'root-image-vendor_boot-9', kind: 'vendorBoot', fileName: 'vendor_boot.img',
        partitionName: 'vendor_boot', sizeBytes: 2048 })
      .mockResolvedValueOnce({ artifactId: 'root-patch-vendor_boot-9', partition: 'vendor_boot', fileName: 'patched_vendor_boot.img', sizeBytes: 2052 });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('select') as HTMLSelectElement).value = 'officialKernelSu';
    (host.querySelector('select') as HTMLSelectElement).dispatchEvent(new Event('change', { bubbles: true }));
    (host.querySelector('.nw-test-root-select-vendor') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('vendor_boot.img') ?? false);
    (host.querySelector('.nw-test-root-patch') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('patched_vendor_boot.img') ?? false);

    expect(command).toHaveBeenCalledWith('root_patch_official_vendor_boot', {
      options: { vendorBootId: 'root-image-vendor_boot-9' },
    });
    expect(host.textContent ?? '').not.toContain('C:\\');
  });

  test('官方 KernelSU 全自动 ROOT 将受控管理器枚举传给 init_boot 修补并随后处理 vendor_boot', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ id: 'root-image-init_boot-official', kind: 'initBoot', fileName: 'init_boot.img',
        partitionName: 'init_boot', sizeBytes: 1024 })
      .mockResolvedValueOnce({ id: 'root-image-vendor_boot-official', kind: 'vendorBoot', fileName: 'vendor_boot.img',
        partitionName: 'vendor_boot', sizeBytes: 2048 })
      .mockResolvedValueOnce({ managerLabel: '官方 KernelSU', effectiveKmi: 'android14-6.1', canPatch: true, canRunAutomatic: true, summary: 'ROOT 前置条件已就绪' })
      .mockResolvedValueOnce({ flashedPartitionCount: 2, commandCount: 3, status: 'ROOT 全自动流程已完成。' });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('select') as HTMLSelectElement).value = 'officialKernelSu';
    (host.querySelector('select') as HTMLSelectElement).dispatchEvent(new Event('change', { bubbles: true }));
    (host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);
    (host.querySelector('.nw-test-root-select-vendor') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('vendor_boot.img') ?? false);
    (host.querySelector('.nw-test-root-preflight') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('ROOT 前置条件已就绪') ?? false);
    (host.querySelector('.nw-test-root-automatic') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('ROOT 全自动流程已完成。') ?? false);

    expect(command).toHaveBeenCalledWith('root_run_automatic', {
      options: {
        manager: 'officialKernelSu',
        initBootId: 'root-image-init_boot-official',
        vendorBootId: 'root-image-vendor_boot-official',
        useAutomaticKmi: true,
        selectedKmi: null,
      },
    });
    expect(command).not.toHaveBeenCalledWith('root_patch_official_vendor_boot', expect.anything());
    expect(command).not.toHaveBeenCalledWith('root_execute_automatic_artifacts', expect.anything());
    expect(host.textContent).not.toContain('init_boot.img');
    expect(host.textContent).not.toContain('vendor_boot.img');
    expect(host.textContent).toContain('等待选择启动镜像');
    expect((host.querySelector('.nw-test-root-automatic') as HTMLButtonElement).disabled).toBe(true);
  });

  test('刷新与 ROOT 操作共享 busy 锁定状态', async () => {
    let resolveRootPatch: ((value: unknown) => void) | undefined;
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockResolvedValueOnce({ id: 'root-image-init_boot-busy', kind: 'initBoot', fileName: 'init_boot.img',
        partitionName: 'init_boot', sizeBytes: 1024 })
      .mockImplementationOnce(() => new Promise((resolve) => {
        resolveRootPatch = resolve;
      }));

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-version') !== null);
    (host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).click();
    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);
    (host.querySelector('.nw-test-root-patch') as HTMLButtonElement).click();
    await waitUntil(() => (host.querySelector('.nw-test-root-refresh') as HTMLButtonElement).disabled);

    expect((host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).disabled).toBe(true);
    expect((host.querySelector('.nw-test-root-preflight') as HTMLButtonElement).disabled).toBe(true);

    resolveRootPatch?.({ artifactId: 'root-patch-busy', partition: 'init_boot', fileName: 'patched_init_boot.img', sizeBytes: 1028 });
    await waitUntil(() => !(host.querySelector('.nw-test-root-refresh') as HTMLButtonElement).disabled);
  });

  test('检测服务器 OTA 结果显示来源单选并禁用不可用项', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_ON);

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-ota-source') !== null);
    await waitUntil(() => host.textContent?.includes('PD2417 OTA') ?? false);

    expect(command).toHaveBeenCalledWith('root_ota_check');
    // 服务器来源单选在检测到 OTA 后可勾选。
    const serverRadio = host.querySelector('input[name="root-ota-source"][value="server"]') as HTMLInputElement
        ?? host.querySelectorAll('input[name="root-ota-source"]')[1] as HTMLInputElement;
    expect(serverRadio.disabled).toBe(false);
  });

  test('检测服务器 OTA 不可用时服务器来源单选被禁用', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF);

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-ota-source') !== null);

    const radios = host.querySelectorAll('input[name="root-ota-source"]');
    const serverRadio = radios.length >= 2 ? radios[1] as HTMLInputElement : null;
    expect(serverRadio).not.toBeNull();
    // available=false → 服务器来源单选禁用。
    await waitUntil(() => (serverRadio as HTMLInputElement).disabled === true);
  });

  test('服务器检测未完成时锁定检测按钮，避免并发请求', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    let resolveOtaCheck: ((value: unknown) => void) | undefined;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockImplementationOnce(() => new Promise((resolve) => {
        resolveOtaCheck = resolve;
      }));

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-ota-source') !== null);
    await waitUntil(() => command.mock.calls.some(([name]) => name === 'root_ota_check'));

    const checkButton = host.querySelector('.nw-test-root-ota-check') as HTMLButtonElement;
    expect(checkButton.disabled).toBe(true);
    expect(command.mock.calls.filter(([name]) => name === 'root_ota_check')).toHaveLength(1);

    resolveOtaCheck?.(OTA_CHECK_OFF);
    await waitUntil(() => !checkButton.disabled);
  });

  test('服务器检测按钮在同一事件循环内重复点击也只发出一次请求', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    let resolveOtaCheck: ((value: unknown) => void) | undefined;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_OFF)
      .mockImplementationOnce(() => new Promise((resolve) => {
        resolveOtaCheck = resolve;
      }));

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-test-root-ota-check') !== null);
    await waitUntil(() => !(host.querySelector('.nw-test-root-ota-check') as HTMLButtonElement).disabled);

    const checkButton = host.querySelector('.nw-test-root-ota-check') as HTMLButtonElement;
    checkButton.click();
    checkButton.click();

    await waitUntil(() => command.mock.calls.filter(([name]) => name === 'root_ota_check').length >= 2);
    expect(command.mock.calls.filter(([name]) => name === 'root_ota_check')).toHaveLength(2);

    resolveOtaCheck?.(OTA_CHECK_OFF);
  });

  test('勾选服务器来源后从服务器提取镜像并填充启动镜像槽位', async () => {
    const command = invoke as unknown as ReturnType<typeof vi.fn>;
    command
      .mockResolvedValueOnce({ update_required: false, force_update: false, latest: null, min_version: null, download_url: null })
      .mockResolvedValueOnce({ has_token: true, healthy: true, running: false, session_id: 'root-session' })
      .mockResolvedValueOnce(OTA_CHECK_ON)
      .mockResolvedValueOnce({
        sourceLabel: '已从 PD2417 OTA 提取',
        initBoot: { id: 'root-image-init_boot-50', kind: 'initBoot', fileName: 'init_boot.img',
        partitionName: 'init_boot', sizeBytes: 8388608 },
        vendorBoot: null,
      });

    renderRoot();
    await waitUntil(() => host.querySelector('.nw-root-ota-source') !== null);
    await waitUntil(() => host.textContent?.includes('PD2417 OTA') ?? false);

    const radios = host.querySelectorAll('input[name="root-ota-source"]');
    const serverRadio = radios.length >= 2 ? radios[1] as HTMLInputElement : null;
    (serverRadio as HTMLInputElement).click();

    await waitUntil(() => host.textContent?.includes('init_boot.img') ?? false);
    expect(command).toHaveBeenCalledWith('root_ota_extract_images');
    // 路径安全：不渲染本地路径。
    expect(host.textContent ?? '').not.toContain('C:\\');
    // 服务器模式隐藏本地选择按钮可用性。
    expect((host.querySelector('.nw-test-root-select-init') as HTMLButtonElement).disabled).toBe(true);
  });
});

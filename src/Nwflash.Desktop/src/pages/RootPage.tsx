import { errorMessage } from '../app/error';
import { FC, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ModalLayer } from '../components/ModalLayer';

type UnknownVersionCheck = unknown;
type UnknownSessionState = unknown;

type VersionState = {
  latest: string | null;
  min_version: string | null;
  download_url: string | null;
  update_required: boolean;
  force_update: boolean;
};

type SessionState = {
  has_token: boolean;
  healthy: boolean;
  running: boolean;
};

type RootImageSelection = {
  id: string;
  kind: 'initBoot' | 'vendorBoot';
  fileName: string;
  partitionName: string;
  sizeBytes: number;
};

type RootReadiness = {
  managerLabel: string;
  effectiveKmi: string;
  canPatch: boolean;
  canRunAutomatic: boolean;
  summary: string;
};

type RootManagerInstallation = {
  managerLabel: string;
  summary: string;
};

type RootPatchedArtifact = {
  artifactId: string;
  partition: string;
  fileName: string;
  sizeBytes: number;
};

type RootPatchedFlashConfirmation = {
  partition: string;
  taskCount: number;
};

type RootAutomaticResult = {
  flashedPartitionCount: number;
  commandCount: number;
  status: string;
};

type RootSourceMode = 'local' | 'server';

type RootOtaCheck = {
  available: boolean;
  label: string | null;
};

type RootOtaExtractResult = {
  sourceLabel: string;
  initBoot: RootImageSelection | null;
  vendorBoot: RootImageSelection | null;
};

const normalizeVersion = (value: UnknownVersionCheck): VersionState => {
  if (value && typeof value === 'object') {
    const raw = value as Record<string, unknown>;
    return {
      latest: typeof raw.latest === 'string' ? raw.latest : null,
      min_version: typeof raw.min_version === 'string' ? raw.min_version : null,
      download_url: typeof raw.download_url === 'string' ? raw.download_url : null,
      update_required: Boolean(raw.update_required),
      force_update: Boolean(raw.force_update),
    };
  }

  return {
    latest: null,
    min_version: null,
    download_url: null,
    update_required: false,
    force_update: false,
  };
};

const normalizeSession = (value: UnknownSessionState): SessionState => {
  if (value && typeof value === 'object') {
    const raw = value as Record<string, unknown>;
    return {
      has_token: Boolean(raw.has_token),
      healthy: Boolean(raw.healthy),
      running: Boolean(raw.running),
    };
  }

  return {
    has_token: false,
    healthy: false,
    running: false,
  };
};

export const RootPage: FC = () => {
  const [versionState, setVersionState] = useState<VersionState | null>(null);
  const [sessionState, setSessionState] = useState<SessionState | null>(null);
  const [errorText, setErrorText] = useState('');
  const [loading, setLoading] = useState(true);
  const [manager, setManager] = useState<'vivoKsu' | 'officialKernelSu'>('vivoKsu');
  const [kmi, setKmi] = useState('android14-6.1');
  const [useAutomaticKmi, setUseAutomaticKmi] = useState(true);
  const [initBoot, setInitBoot] = useState<RootImageSelection | null>(null);
  const [vendorBoot, setVendorBoot] = useState<RootImageSelection | null>(null);
  const [readiness, setReadiness] = useState<RootReadiness | null>(null);
  const [rootBusy, setRootBusy] = useState(false);
  const [managerResourceReady, setManagerResourceReady] = useState(false);
  const [managerInstallation, setManagerInstallation] = useState<RootManagerInstallation | null>(null);
  const [patchedArtifact, setPatchedArtifact] = useState<RootPatchedArtifact | null>(null);
  const [patchConfirmation, setPatchConfirmation] = useState<RootPatchedFlashConfirmation | null>(null);
  const [patchStatus, setPatchStatus] = useState('');
  const [sourceMode, setSourceMode] = useState<RootSourceMode>('local');
  const [otaAvailable, setOtaAvailable] = useState(false);
  const [otaLabel, setOtaLabel] = useState<string | null>(null);
  const [otaChecking, setOtaChecking] = useState(false);
  const otaCheckInFlight = useRef(false);
  const [sourceLabel, setSourceLabel] = useState('');

  const runOtaCheck = async () => {
    if (otaCheckInFlight.current) return;
    otaCheckInFlight.current = true;
    setOtaChecking(true);
    try {
      const check = await invoke<RootOtaCheck>('root_ota_check');
      setOtaAvailable(check.available);
      setOtaLabel(check.available ? check.label : null);
    } catch {
      // 静默：检测服务器 OTA 失败不打断页面。
      setOtaAvailable(false);
      setOtaLabel(null);
    } finally {
      otaCheckInFlight.current = false;
      setOtaChecking(false);
    }
  };

  const extractFromServer = async () => {
    setRootBusy(true);
    setErrorText('');
    try {
      const result = await invoke<RootOtaExtractResult>('root_ota_extract_images');
      setInitBoot(result.initBoot);
      setVendorBoot(result.vendorBoot);
      setSourceLabel(result.sourceLabel);
      setReadiness(null);
      setPatchedArtifact(null);
      setPatchConfirmation(null);
      setPatchStatus('');
    } catch (error) {
      setErrorText(errorMessage(error, '服务器 OTA 提取失败'));
      setSourceMode('local');
    } finally {
      setRootBusy(false);
    }
  };

  const refresh = async () => {
    setLoading(true);
    setErrorText('');

    try {
      const [versionResponse, sessionResponse] = await Promise.all([
        invoke<UnknownVersionCheck>('version_check'),
        invoke<UnknownSessionState>('session_state'),
      ]);
      setVersionState(normalizeVersion(versionResponse));
      setSessionState(normalizeSession(sessionResponse));
      void runOtaCheck();
    } catch (error) {
      setErrorText(errorMessage(error, 'Root 页面状态读取失败'));
      setVersionState(null);
      setSessionState(null);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const selectRootImage = async (kind: RootImageSelection['kind']) => {
    setRootBusy(true);
    setErrorText('');
    try {
      const selection = await invoke<RootImageSelection>('root_select_image', { kind });
      if (kind === 'initBoot') {
        setInitBoot(selection);
      } else {
        setVendorBoot(selection);
      }
      setReadiness(null);
      setPatchedArtifact(null);
      setPatchConfirmation(null);
      setPatchStatus('');
    } catch (error) {
      setErrorText(errorMessage(error, 'ROOT 镜像选择失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const preflightRoot = async () => {
    setRootBusy(true);
    setErrorText('');
    try {
      const response = await invoke<RootReadiness>('root_preflight', {
        options: {
          manager,
          initBootId: initBoot?.id ?? null,
          vendorBootId: vendorBoot?.id ?? null,
          useAutomaticKmi,
          selectedKmi: useAutomaticKmi ? null : kmi,
        },
      });
      setReadiness(response);
    } catch (error) {
      setReadiness(null);
      setErrorText(errorMessage(error, 'ROOT 预检失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const installManagerResource = async () => {
    setRootBusy(true);
    setErrorText('');
    try {
      const key = manager === 'vivoKsu' ? 'manager-KSU' : 'manager-OfficialKsu';
      await invoke<string[]>('resource_install', { keys: [key] });
      setManagerResourceReady(true);
    } catch (error) {
      setErrorText(errorMessage(error, 'ROOT 管理器资源安装失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const installManagerOnDevice = async () => {
    setRootBusy(true);
    setErrorText('');
    try {
      const result = await invoke<RootManagerInstallation>('root_install_manager', { manager });
      setManagerInstallation(result);
    } catch (error) {
      setManagerInstallation(null);
      setErrorText(errorMessage(error, 'ROOT 管理器设备安装失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const patchRootImage = async () => {
    const selectedImage = manager === 'vivoKsu' ? initBoot : vendorBoot;
    if (!selectedImage) {
      setErrorText(manager === 'vivoKsu' ? '请先选择 init_boot 镜像。' : '请先选择 vendor_boot 镜像。');
      return;
    }

    setRootBusy(true);
    setErrorText('');
    setPatchConfirmation(null);
    setPatchStatus('');
    try {
      const artifact = manager === 'vivoKsu'
        ? await invoke<RootPatchedArtifact>('root_patch_vivo_ksu', {
          options: {
            initBootId: selectedImage.id,
            useAutomaticKmi,
            selectedKmi: useAutomaticKmi ? null : kmi,
          },
        })
        : await invoke<RootPatchedArtifact>('root_patch_official_vendor_boot', {
          options: { vendorBootId: selectedImage.id },
        });
      setPatchedArtifact(artifact);
    } catch (error) {
      setPatchedArtifact(null);
      setErrorText(errorMessage(error, 'ROOT 镜像修补失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const preparePatchedArtifactFlash = async () => {
    if (!patchedArtifact) return;

    setRootBusy(true);
    setErrorText('');
    try {
      const confirmation = await invoke<RootPatchedFlashConfirmation>(
        'root_prepare_patched_artifact_flash',
        { artifactId: patchedArtifact.artifactId },
      );
      setPatchConfirmation(confirmation);
    } catch (error) {
      setPatchConfirmation(null);
      setErrorText(errorMessage(error, 'ROOT 修补镜像刷写预检失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const executePatchedArtifactFlash = async () => {
    if (!patchedArtifact) return;

    setRootBusy(true);
    setErrorText('');
    try {
      await invoke('root_execute_patched_artifact_flash', {
        artifactId: patchedArtifact.artifactId,
      });
      setPatchConfirmation(null);
      setPatchStatus('ROOT 修补镜像刷写已完成。');
    } catch (error) {
      setErrorText(errorMessage(error, 'ROOT 修补镜像刷写失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const runAutomaticRoot = async () => {
    if (!readiness?.canRunAutomatic || !initBoot) {
      setErrorText('请先完成 ROOT 预检。');
      return;
    }
    if (manager === 'officialKernelSu' && !vendorBoot) {
      setErrorText('官方 KernelSU 全自动 ROOT 需要 vendor_boot 镜像。');
      return;
    }

    setRootBusy(true);
    setErrorText('');
    setPatchConfirmation(null);
    setPatchStatus('');
    try {
      const result = await invoke<RootAutomaticResult>('root_run_automatic', {
        options: {
          manager,
          initBootId: initBoot.id,
          vendorBootId: manager === 'officialKernelSu' ? vendorBoot?.id ?? null : null,
          useAutomaticKmi,
          selectedKmi: useAutomaticKmi ? null : kmi,
        },
      });
      setPatchedArtifact(null);
      setInitBoot(null);
      setVendorBoot(null);
      setReadiness(null);
      setPatchStatus(result.status);
    } catch (error) {
      setErrorText(errorMessage(error, 'ROOT 全自动流程失败'));
    } finally {
      setRootBusy(false);
    }
  };

  const cancelRootOperation = async () => {
    setErrorText('');
    try {
      await invoke('operation_cancel');
    } catch (error) {
      setErrorText(errorMessage(error, '取消 ROOT 操作失败'));
    }
  };

  const isBusy = loading || rootBusy || otaChecking;

  return (
    <section className="nw-root-page" aria-label="Vivo ROOT">
      <header className="nw-root-heading">
        <div>
          <p className="nw-page-eyebrow">VIVO / ROOT WORKFLOW</p>
          <h1>Vivo ROOT</h1>
          <p>KernelSU 启动镜像修补与刷写</p>
        </div>
        <span className="nw-root-status-chip"><i />{readiness?.summary ?? (initBoot ? '已选择启动镜像' : '等待选择启动镜像')}</span>
      </header>

      <section className="nw-root-workbench">
        <article className="nw-root-ota-source">
          <div>
            <label>OTA 来源</label>
            <div className="nw-root-ota-options">
              <label><input type="radio" name="root-ota-source" value="local" checked={sourceMode === 'local'} disabled={isBusy} onChange={() => setSourceMode('local')} />本地镜像</label>
              <label><input type="radio" name="root-ota-source" value="server" checked={sourceMode === 'server'} disabled={isBusy || !otaAvailable} onChange={() => { setSourceMode('server'); void extractFromServer(); }} />从服务器获取</label>
            </div>
            <button type="button" className="nw-test-root-ota-check" disabled={isBusy} onClick={() => void runOtaCheck()}>检测服务器 OTA</button>
            <small>{sourceLabel ? sourceLabel : (otaAvailable ? `已发现服务器 OTA：${otaLabel ?? ''}` : '从服务器获取时按需云提取 init_boot/boot/vendor_boot 分区')}</small>
          </div>
        </article>

        <article className="nw-root-image-preflight">
          <div>
            <h2>启动镜像预检</h2>
            <p>{initBoot ? `${initBoot.partitionName} 镜像` : '未选择启动镜像'}　<strong>{initBoot ? initBoot.fileName : ''}</strong></p>
            <small>官版 KSU 全自动需另选 vendor_boot。</small>
          </div>
          <button type="button" className="nw-test-root-select-init" disabled={isBusy || sourceMode === 'server'} onClick={() => void selectRootImage('initBoot')}>
            选择启动镜像
          </button>
        </article>

        <article className="nw-root-manager-kmi">
          <div>
            <label>ROOT 管理器</label>
            <select value={manager} disabled={isBusy} onChange={(event) => {
              setManager(event.target.value as typeof manager);
              setManagerResourceReady(false);
              setManagerInstallation(null);
              setReadiness(null);
              setPatchedArtifact(null);
              setPatchConfirmation(null);
              setPatchStatus('');
            }}>
              <option value="vivoKsu">Vivo KSU</option>
              <option value="officialKernelSu">官方 KernelSU</option>
            </select>
            <p>{managerResourceReady ? '管理器资源已就绪' : 'Vivo KSU APK 已校验'}</p>
          </div>
          <div>
            <label className="nw-root-kmi-label">KMI</label>
            <label className="nw-root-auto-kmi"><input type="checkbox" className="nw-test-root-auto-kmi" checked={useAutomaticKmi} disabled={isBusy} onChange={(event) => setUseAutomaticKmi(event.target.checked)} />自动识别（默认）</label>
            <select value={kmi} disabled={isBusy || useAutomaticKmi} onChange={(event) => setKmi(event.target.value)}>
              <option value="android13-5.15">android13-5.15</option>
              <option value="android14-6.1">android14-6.1</option>
              <option value="android15-6.6">android15-6.6</option>
            </select>
            <p>生效 KMI: {readiness?.effectiveKmi ?? '未检测'}</p>
          </div>
        </article>

        {manager === 'officialKernelSu' ? (
          <article className="nw-root-vendor-image">
            <div><label>vendor_boot 镜像</label><strong>{vendorBoot?.fileName ?? '未选择 vendor_boot 镜像'}</strong></div>
            <button type="button" className="nw-test-root-select-vendor" disabled={isBusy || sourceMode === 'server'} onClick={() => void selectRootImage('vendorBoot')}>选择 vendor_boot</button>
          </article>
        ) : <button type="button" className="nw-test-root-select-vendor nw-root-hidden-control" disabled={isBusy || sourceMode === 'server'} onClick={() => void selectRootImage('vendorBoot')}>选择 vendor_boot</button>}

        <footer className="nw-root-actions">
          <div><strong>KSU / 镜像修补</strong><p>{patchedArtifact ? `${patchedArtifact.fileName}（${patchedArtifact.sizeBytes} B）` : '尚未生成 init_boot 修补镜像'}</p></div>
          <div>
            <button type="button" className="nw-test-root-install-manager" disabled={isBusy || managerResourceReady} onClick={() => void installManagerResource()}>准备管理器</button>
            <button type="button" className="nw-test-root-install-device-manager" disabled={isBusy} onClick={() => void installManagerOnDevice()}>安装管理器</button>
            <button type="button" className="nw-test-root-preflight" disabled={isBusy} onClick={() => void preflightRoot()}>ROOT 预检</button>
            <button type="button" className="nw-test-root-automatic" disabled={isBusy || !readiness?.canRunAutomatic} onClick={() => void runAutomaticRoot()}>全自动 ROOT</button>
            {rootBusy ? <button type="button" className="nw-test-root-cancel" onClick={() => void cancelRootOperation()}>取消操作</button> : null}
            <button type="button" className="nw-test-root-patch" disabled={isBusy || (manager === 'vivoKsu' ? initBoot === null : vendorBoot === null)} onClick={() => void patchRootImage()}>修补镜像</button>
            {patchedArtifact ? <button type="button" className="nw-test-root-transfer" disabled={isBusy} onClick={() => void preparePatchedArtifactFlash()}>刷写 {patchedArtifact.partition}</button> : null}
          </div>
        </footer>

        {managerInstallation ? <p className="nw-root-installation">{managerInstallation.summary}</p> : null}
        {errorText && <p className="nw-error-text">{errorText}</p>}
        {patchStatus ? <p className="nw-success-text">{patchStatus}</p> : null}
      </section>

      <ModalLayer
        isVisible={patchConfirmation !== null}
        title="确认刷写 ROOT 修补镜像"
        onClose={() => setPatchConfirmation(null)}
      >
        <p>确认刷入 {patchConfirmation?.partition} 分区吗？</p>
        <p>将执行 {patchConfirmation?.taskCount ?? 0} 项刷写任务。</p>
        <div className="nw-page-actions">
          <button type="button" onClick={() => setPatchConfirmation(null)} disabled={isBusy}>
            取消
          </button>
          <button
            type="button"
            className="nw-test-root-confirm"
            onClick={() => void executePatchedArtifactFlash()}
            disabled={isBusy}
          >
            确认刷写
          </button>
        </div>
      </ModalLayer>

      <div className="nw-root-meta" aria-hidden="true">
        <button type="button" className="nw-test-root-refresh" disabled={isBusy} onClick={() => void refresh()}>刷新</button>
        {loading && !errorText ? <p>加载中...</p> : null}
        {sessionState ? <div className="nw-root-session">
          <h3>会话</h3>
          <p>Token: {sessionState.has_token ? '已登录' : '未登录'}</p>
          <p>运行: {sessionState.running ? '进行中' : '空闲'}</p>
          <p>健康: {sessionState.healthy ? '正常' : '异常'}</p>
        </div> : null}
        {versionState ? <div className="nw-root-version">
          <h3>版本</h3>
          <p>最新: {versionState.latest || '未知'}</p>
          <p>最低: {versionState.min_version || '未配置'}</p>
          <p>更新要求: {versionState.update_required ? '是' : '否'}</p>
          <p>强制更新: {versionState.force_update ? '是' : '否'}</p>
        </div> : null}
      </div>
    </section>
  );
};

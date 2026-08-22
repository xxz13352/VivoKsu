import { errorMessage } from '../app/error';
import { FC, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ModalLayer } from '../components/ModalLayer';
import { ResourceDownloadPage } from './ResourceDownloadPage';

type UnknownVersionCheck = unknown;
type UnknownLogs = unknown;
type UnknownSoftwareStatus = unknown;

type VersionState = {
  latest: string | null;
  min_version: string | null;
  download_url: string | null;
  update_required: boolean;
  force_update: boolean;
};

type OperationLogEntry = {
  timestamp_utc: number;
  level: 'Info' | 'Success' | 'Warning' | 'Error';
  message: string;
  operation_id: string | null;
};

type SoftwareStatus = {
  app_version: string;
  adb_ready: boolean;
  fastboot_ready: boolean;
  scrcpy_ready: boolean;
  payload_dumper_ready: boolean;
  adb_driver_installed: boolean;
  fastboot_driver_installed: boolean;
  mediatek_driver_installed: boolean;
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

const normalizeLogs = (logs: UnknownLogs): readonly OperationLogEntry[] =>
  Array.isArray(logs) ? logs : [];

const normalizeSoftwareStatus = (value: UnknownSoftwareStatus): SoftwareStatus => {
  const raw = value && typeof value === 'object' ? (value as Record<string, unknown>) : {};
  return {
    app_version: typeof raw.app_version === 'string' && raw.app_version.trim()
      ? raw.app_version
      : '0.0.0',
    adb_ready: Boolean(raw.adb_ready),
    fastboot_ready: Boolean(raw.fastboot_ready),
    scrcpy_ready: Boolean(raw.scrcpy_ready),
    payload_dumper_ready: Boolean(raw.payload_dumper_ready),
    adb_driver_installed: Boolean(raw.adb_driver_installed),
    fastboot_driver_installed: Boolean(raw.fastboot_driver_installed),
    mediatek_driver_installed: Boolean(raw.mediatek_driver_installed),
  };
};

const formatReadiness = (value: boolean, ready: string, unavailable: string) =>
  value ? ready : unavailable;

export const SoftwarePage: FC = () => {
  const [versionState, setVersionState] = useState<VersionState | null>(null);
  const [logs, setLogs] = useState<readonly OperationLogEntry[]>([]);
  const [softwareStatus, setSoftwareStatus] = useState<SoftwareStatus | null>(null);
  const [errorText, setErrorText] = useState('');
  const [loading, setLoading] = useState(true);
  const [driverDialogOpen, setDriverDialogOpen] = useState(false);
  const [driverInstalling, setDriverInstalling] = useState(false);
  const [driverErrorText, setDriverErrorText] = useState('');
  const [resourceDialogOpen, setResourceDialogOpen] = useState(false);
  const [resourceInstalling, setResourceInstalling] = useState(false);

  const loadSoftware = async () => {
    setLoading(true);
    setErrorText('');

    const [versionResult, logsResult, statusResult] = await Promise.allSettled([
      invoke<UnknownVersionCheck>('version_check'),
      invoke<UnknownLogs>('operation_logs_snapshot'),
      invoke<UnknownSoftwareStatus>('software_status'),
    ]);

    if (versionResult.status === 'fulfilled') {
      setVersionState(normalizeVersion(versionResult.value));
    } else {
      // 版本门禁在 App 启动阶段单独处理;其诊断失败不能隐藏本地组件状态。
      setVersionState(null);
    }

    if (logsResult.status === 'fulfilled') {
      setLogs(normalizeLogs(logsResult.value));
    } else {
      // 操作日志由右侧常驻面板负责;软件页的组件检测不依赖它。
      setLogs([]);
    }

    if (statusResult.status === 'fulfilled') {
      setSoftwareStatus(normalizeSoftwareStatus(statusResult.value));
    } else {
      setErrorText(errorMessage(statusResult.reason, '软件信息读取失败'));
      setSoftwareStatus(null);
    }

    setLoading(false);
  };

  useEffect(() => {
    void loadSoftware();
  }, []);

  const reinstallDrivers = async () => {
    setDriverInstalling(true);
    setDriverErrorText('');
    try {
      await invoke('driver_reinstall');
      await loadSoftware();
      setDriverDialogOpen(false);
    } catch (error) {
      setDriverErrorText(errorMessage(error, '驱动安装失败'));
    } finally {
      setDriverInstalling(false);
    }
  };

  const closeResourceDialog = async () => {
    if (resourceInstalling) {
      try {
        await invoke('operation_cancel');
      } finally {
        setResourceDialogOpen(false);
      }
      return;
    }
    setResourceDialogOpen(false);
  };

  return (
    <section className="nw-software-page">
      <header className="nw-software-page-heading">
        <div>
          <p className="nw-page-eyebrow">SOFTWARE / STATUS</p>
          <h1>软件</h1>
          <p>奶蛙Flash 版本与依赖组件的就绪状态</p>
        </div>
        <div className="nw-software-page-actions">
          <button
            type="button"
            className="nw-test-resource-install-open"
            disabled={loading || driverInstalling || resourceInstalling}
            onClick={() => setResourceDialogOpen(true)}
          >
            安装组件
          </button>
          <button type="button" className="nw-test-software-refresh" onClick={() => void loadSoftware()}>
            重新检测
          </button>
        </div>
      </header>

      {loading && !errorText ? <p>加载软件信息...</p> : null}
      {errorText && <p className="nw-error-text">{errorText}</p>}

      {versionState ? (
        <section className="nw-software-version" hidden>
          <p>最新版本：{versionState.latest || '未知'}</p>
          <p>最低要求：{versionState.min_version || '未配置'}</p>
          <p>更新链接：{versionState.download_url || '无'}</p>
          <p>更新要求：{versionState.update_required ? '是' : '否'}</p>
          <p>强制更新：{versionState.force_update ? '是' : '否'}</p>
        </section>
      ) : null}

      {softwareStatus ? (
        <section className="nw-software-components nw-software-status-table" aria-label="组件状态">
          <header>
            <h3>组件状态</h3>
            <span>SOFTWARE COMPONENTS</span>
          </header>
          <article className="nw-software-status-row">
            <div><strong>奶蛙Flash 客户端</strong><small>版本 v{softwareStatus.app_version}</small></div>
            <em className="nw-software-ready">就绪</em>
          </article>
          <article className="nw-software-status-row nw-software-driver-row">
            <div>
              <strong>手机 USB 驱动</strong>
              <small>ADB / Fastboot / MediaTek 三类分别检测,任缺一类启动即提醒</small>
              <div className="nw-software-driver-list">
                <div className="nw-software-driver-status">
                  <strong>ADB（WinUSB）</strong>
                  <em className={softwareStatus.adb_driver_installed ? 'nw-software-ready' : 'nw-software-missing'}>
                    {formatReadiness(softwareStatus.adb_driver_installed, '已安装', '未安装')}
                  </em>
                </div>
                <div className="nw-software-driver-status">
                  <strong>Fastboot（fastbootd 刷写）</strong>
                  <em className={softwareStatus.fastboot_driver_installed ? 'nw-software-ready' : 'nw-software-missing'}>
                    {formatReadiness(softwareStatus.fastboot_driver_installed, '已安装', '未安装')}
                  </em>
                </div>
                <div className="nw-software-driver-status">
                  <strong>MediaTek（联发科 / BROM 救砖）</strong>
                  <em className={softwareStatus.mediatek_driver_installed ? 'nw-software-ready' : 'nw-software-missing'}>
                    {formatReadiness(softwareStatus.mediatek_driver_installed, '已安装', '未安装')}
                  </em>
                </div>
              </div>
            </div>
            <button
              type="button"
              className="nw-test-driver-reinstall-open"
              disabled={loading || driverInstalling}
              onClick={() => {
                setDriverErrorText('');
                setDriverDialogOpen(true);
              }}
            >
              重新安装
            </button>
          </article>
          <article className="nw-software-status-row nw-software-tool-row">
            <div><strong>scrcpy 投屏工具</strong><small>投屏所需 scrcpy.exe（发布内置）</small></div>
            <em className={softwareStatus.scrcpy_ready ? 'nw-software-ready' : 'nw-software-missing'}>
              {formatReadiness(softwareStatus.scrcpy_ready, 'scrcpy 已就绪', '未检测到 scrcpy.exe')}
            </em>
          </article>
          <article className="nw-software-status-row nw-software-tool-row">
            <div><strong>payload_dumper 解包工具</strong><small>固件提取所需 payload_dumper.exe</small></div>
            <em className={softwareStatus.payload_dumper_ready ? 'nw-software-ready' : 'nw-software-missing'}>
              {formatReadiness(softwareStatus.payload_dumper_ready, '就绪', '未就绪')}
            </em>
          </article>
        </section>
      ) : null}

      <section className="nw-software-help" aria-label="软件帮助">
        <h3>需要帮助?</h3>
        <p>驱动未安装时,启动会自动弹出驱动提醒;也可点右上角「重新检测」刷新各组件状态。</p>
      </section>

      <section className="nw-software-logs" hidden>
        <h3>最近操作日志</h3>
        {logs.length === 0 ? <p className="nw-empty-log">暂无最近日志</p> : null}
        <ul>
          {logs.slice(0, 2).map((log, index) => (
            <li key={`${log.timestamp_utc}-${index}`} className="nw-test-software-log-item">
              {log.level}：{log.message}
            </li>
          ))}
        </ul>
      </section>
      <ModalLayer
        isVisible={driverDialogOpen}
        title="USB 驱动安装"
        onClose={driverInstalling ? undefined : () => setDriverDialogOpen(false)}
      >
        <p>可以重新安装 ADB / Fastboot / MediaTek 三类驱动。安装需要管理员权限。</p>
        {driverErrorText ? <p className="nw-error-text">{driverErrorText}</p> : null}
        <div className="nw-driver-dialog-actions">
          <button
            type="button"
            disabled={driverInstalling}
            onClick={() => setDriverDialogOpen(false)}
          >
            取消
          </button>
          <button
            type="button"
            className="nw-test-driver-reinstall-confirm"
            disabled={driverInstalling}
            onClick={() => void reinstallDrivers()}
          >
            {driverInstalling ? '安装中...' : '安装驱动'}
          </button>
        </div>
      </ModalLayer>
      <ModalLayer
        isVisible={resourceDialogOpen}
        title="内置组件检查"
        onClose={() => void closeResourceDialog()}
      >
        <ResourceDownloadPage
          embedded
          onInstallingChange={setResourceInstalling}
          onCompleted={() => {
            setResourceDialogOpen(false);
            void loadSoftware();
          }}
          onRequestClose={() => setResourceDialogOpen(false)}
        />
      </ModalLayer>
    </section>
  );
};

import { FC, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { LogicalSize } from '@tauri-apps/api/dpi';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { NWFLASH_APP_PAGES, PAGE_TITLES, type AppPageId } from './pageManifest';
import { errorMessage } from './error';
import { APP_OPERATION_LABELS, PROGRESS_CHANNEL_ORDER, type BusyOperationItem } from './window-state';
import {
  IPC_EVENTS,
  type AuthSessionPayload,
  type DeviceSnapshotPayload,
  type OperationSnapshotPayload,
  type SessionForceExitPayload,
  type SessionStateV2Payload,
  type SessionUpdateRequiredPayload,
} from './ipc-events';
import { AppShell } from '../components/AppShell';
import { LoginScreen } from '../components/LoginScreen';
import { PageContainer } from '../components/PageContainer';
import { PageFactory } from '../components/PageFactory';
import { UpdateRequiredDialog, type UpdateRequiredDetails } from '../components/UpdateRequiredDialog';
import { ResourceDownloadPage } from '../pages/ResourceDownloadPage';

export const APP_BRAND = '奶蛙Flash';
const LOGIN_WINDOW_SIZE = new LogicalSize(400, 564);
const MAIN_WINDOW_SIZE = new LogicalSize(1240, 700);
const hasTauriRuntime = (): boolean => {
  if (typeof window === 'undefined') {
    return false;
  }

  const runtime = window as Window & {
    __TAURI__?: unknown;
    __TAURI_INTERNALS__?: { invoke?: unknown };
  };
  return runtime.__TAURI__ !== undefined || typeof runtime.__TAURI_INTERNALS__?.invoke === 'function';
};

type VersionCheckPayload = {
  latest: string | null;
  min_version: string | null;
  minVersion?: string | null;
  download_url: string | null;
  update_required: boolean;
  force_update: boolean;
};

type ReadinessDialog = 'resources' | 'drivers' | null;
const MAX_TERMINAL_GENERATIONS = 32;
type TerminalGenerationState =
  | { readonly kind: 'force-exit'; readonly reason: string }
  | { readonly kind: 'update-required'; readonly payload: SessionUpdateRequiredPayload };

export const rememberBoundedTerminalGeneration = <T,>(
  generations: Map<string, T>,
  generation: string,
  state: T,
  capacity = MAX_TERMINAL_GENERATIONS,
): void => {
  if (generations.has(generation)) {
    return;
  }
  generations.set(generation, state);
  while (generations.size > capacity) {
    const oldest = generations.keys().next().value as string | undefined;
    if (!oldest) {
      break;
    }
    generations.delete(oldest);
  }
};

export const formatCurrentTime = (value = new Date()): string => {
  const month = String(value.getMonth() + 1).padStart(2, '0');
  const day = String(value.getDate()).padStart(2, '0');
  const hour = String(value.getHours()).padStart(2, '0');
  const minute = String(value.getMinutes()).padStart(2, '0');
  const second = String(value.getSeconds()).padStart(2, '0');
  return `${month}-${day} ${hour}:${minute}:${second}`;
};

const fallbackLabel = (kind: BusyOperationItem['kind']): string => `${APP_OPERATION_LABELS[kind]}进行中`;

const DEMO_PROGRESS_LABELS: Record<(typeof PROGRESS_CHANNEL_ORDER)[number], string> = {
  quick: `${APP_OPERATION_LABELS.quick}任务示例`,
  lineFlash: `${APP_OPERATION_LABELS.lineFlash}任务示例`,
  safeFlash: `${APP_OPERATION_LABELS.safeFlash}任务示例`,
  firmwareExtract: `${APP_OPERATION_LABELS.firmwareExtract}任务示例`,
  device: `${APP_OPERATION_LABELS.device}任务示例`,
};

export const resolveBusyKind = (
  snapshot: OperationSnapshotPayload,
): BusyOperationItem['kind'] | null => {
  if (!snapshot.isBusy) {
    return null;
  }

  switch (snapshot.kind) {
    case 'Mirroring':
    case 'Discovering':
    case 'Rebooting':
      return 'device';
    case 'Hashing':
      return 'firmwareExtract';
    case 'Installing':
      return 'safeFlash';
    case 'Flashing':
      return 'quick';
    case 'Transferring':
      return 'lineFlash';
    case 'Completed':
    case 'Canceled':
    case 'Failed':
    case 'Idle':
      return null;
  }

  const title = snapshot.stage || snapshot.title;
  const normalizedTitle = title.toLowerCase();

  if (
    normalizedTitle.includes('可视') ||
    normalizedTitle.includes('line') ||
    normalizedTitle.includes('partition')
  ) {
    return 'lineFlash';
  }

  if (
    normalizedTitle.includes('vivo') ||
    normalizedTitle.includes('root') ||
    normalizedTitle.includes('vivo root')
  ) {
    return 'safeFlash';
  }

  if (
    normalizedTitle.includes('固件') ||
    normalizedTitle.includes('payload') ||
    normalizedTitle.includes('extract')
  ) {
    return 'firmwareExtract';
  }

  if (
    normalizedTitle.includes('刷写') ||
    normalizedTitle.includes('flash') ||
    normalizedTitle.includes('partition')
  ) {
    return 'quick';
  }

  if (
    snapshot.kind === 'Mirroring' ||
    snapshot.kind === 'Discovering' ||
    snapshot.kind === 'Rebooting'
  ) {
    return 'device';
  }

  return 'quick';
};

const formatOperationLabel = (snapshot: OperationSnapshotPayload): string =>
  snapshot.stage || snapshot.title || fallbackLabel('device');

const updateRequiredDetailsFromError = (error: unknown): UpdateRequiredDetails | null => {
  const message = errorMessage(error, '');
  if (!/^(?:需要更新|更新要求|版本不符合|服务端返回 426|426\b)/.test(message)) {
    return null;
  }

  return {
    message,
    latest: null,
    minVersion: null,
    downloadUrl: null,
  };
};

export const App: FC = () => {
  const [currentPage, setCurrentPage] = useState<AppPageId>('Overview');
  const [isLoggedIn, setIsLoggedIn] = useState(false);
  const [accountName, setAccountName] = useState('未登录');
  const [operations, setOperations] = useState<readonly BusyOperationItem[]>([]);
  const [operationSnapshot, setOperationSnapshot] = useState<OperationSnapshotPayload | null>(null);
  const [sessionNotice, setSessionNotice] = useState('');
  const [currentTime, setCurrentTime] = useState(() => formatCurrentTime());
  const [loginUsername, setLoginUsername] = useState('');
  const [loginPassword, setLoginPassword] = useState('');
  const [isBusyAction, setIsBusyAction] = useState(false);
  const [versionBlock, setVersionBlock] = useState<UpdateRequiredDetails | null>(null);
  const [deviceSnapshot, setDeviceSnapshot] = useState<DeviceSnapshotPayload | null>(null);
  const [readinessDialog, setReadinessDialog] = useState<ReadinessDialog>(null);
  const [readinessBusy, setReadinessBusy] = useState(false);
  const [driverReadinessError, setDriverReadinessError] = useState('');
  const closingWindow = useRef(false);
  const startupCancelledRef = useRef(false);
  const startupAttemptRef = useRef(0);
  const currentGenerationRef = useRef<string | null>(null);
  const terminalGenerationsRef = useRef<Map<string, TerminalGenerationState>>(new Map());

  const isVersionBlocked = versionBlock !== null;

  const pages = useMemo(() => PAGE_TITLES[currentPage], [currentPage]);
  const pageOwnsHeader =
    (currentPage === 'Software' || currentPage === 'Overview' || currentPage === 'FileManager' || currentPage === 'Mirror' || currentPage === 'QuickFlash' || currentPage === 'LineFlash' || currentPage === 'FirmwareExtract' || currentPage === 'Root' || currentPage === 'Online' || currentPage === 'SafeFlash' || currentPage === 'OperationLog') &&
    !sessionNotice;

  const upsertOperation = useCallback((snapshot: OperationSnapshotPayload) => {
    setOperationSnapshot(snapshot);
    const kind = resolveBusyKind(snapshot);
    if (!kind) {
      setOperations([]);
      return;
    }

    setOperations([{ kind, message: formatOperationLabel(snapshot) }]);
  }, []);

  const showDriverReminderIfNeeded = useCallback(async () => {
    try {
      const response = await invoke<unknown>('software_status');
      const status = response && typeof response === 'object'
        ? response as Record<string, unknown>
        : {};
      if (!Boolean(status.adb_driver_installed) || !Boolean(status.fastboot_driver_installed)) {
        setDriverReadinessError('');
        setReadinessDialog('drivers');
      } else {
        setReadinessDialog(null);
      }
    } catch (error) {
      console.debug('驱动就绪检查失败，按容错策略继续:', error);
      setReadinessDialog(null);
    }
  }, []);

  const runPostLoginReadiness = useCallback(async () => {
    try {
      const response = await invoke<unknown>('resource_inventory');
      const hasMissingResource = Array.isArray(response)
        && response.some((item) => item && typeof item === 'object' && !Boolean(item.is_ready));
      if (hasMissingResource) {
        setReadinessDialog('resources');
        return;
      }
    } catch (error) {
      console.debug('内置组件就绪检查失败:', error);
    }

    await showDriverReminderIfNeeded();
  }, [showDriverReminderIfNeeded]);

  const rememberTerminalGeneration = useCallback(
    (generation: string, state: TerminalGenerationState) => {
      rememberBoundedTerminalGeneration(terminalGenerationsRef.current, generation, state);
    },
    [],
  );

  const isCurrentLiveGeneration = useCallback(
    (generation: string) =>
      currentGenerationRef.current === generation &&
      !terminalGenerationsRef.current.has(generation),
    [],
  );

  const isStartupAttemptCurrent = useCallback(
    (attempt: number) =>
      !startupCancelledRef.current && startupAttemptRef.current === attempt,
    [],
  );

  const cancelStartupAttempt = useCallback(() => {
    startupCancelledRef.current = true;
    startupAttemptRef.current += 1;
  }, []);

  const clearVersionBlock = useCallback(() => {
    setVersionBlock(null);
  }, []);

  const applyTerminalGeneration = useCallback(
    (state: TerminalGenerationState) => {
      setOperations([]);
      setIsLoggedIn(false);
      setAccountName('未登录');
      if (state.kind === 'force-exit') {
        setSessionNotice(`会话已退出：${state.reason}`);
        clearVersionBlock();
        return;
      }

      const message = state.payload.message || '检测到新版本要求，请立即更新后重新登录。';
      setSessionNotice(message);
      setVersionBlock({
        message,
        latest: state.payload.latest,
        minVersion: state.payload.minVersion,
        downloadUrl: state.payload.downloadUrl,
      });
      setReadinessDialog(null);
    },
    [clearVersionBlock],
  );

  const refreshSessionState = useCallback(async (startupAttempt: number) => {
    let revealedGeneration: string | null = null;
    try {
      const sessionState = await invoke<SessionStateV2Payload>('session_state');
      if (!isStartupAttemptCurrent(startupAttempt)) {
        return;
      }
      if (!sessionState.has_token || !sessionState.running) {
        currentGenerationRef.current = null;
        setIsLoggedIn(false);
        setAccountName('未登录');
        return;
      }
      const generation = sessionState.generation;
      if (!generation) {
        currentGenerationRef.current = null;
        setIsLoggedIn(false);
        setAccountName('未登录');
        return;
      }
      revealedGeneration = generation;
      const terminal = terminalGenerationsRef.current.get(generation);
      if (terminal) {
        currentGenerationRef.current = null;
        applyTerminalGeneration(terminal);
        return;
      }
      currentGenerationRef.current = generation;

      const validated = await invoke<string | null>('auth_validate_token');
      if (
        !isStartupAttemptCurrent(startupAttempt) ||
        !isCurrentLiveGeneration(generation)
      ) {
        return;
      }
      if (!validated) {
        currentGenerationRef.current = null;
        setIsLoggedIn(false);
        setAccountName('未登录');
        return;
      }

      setIsLoggedIn(true);
      setAccountName(validated);

      await runPostLoginReadiness();
      if (
        !isStartupAttemptCurrent(startupAttempt) ||
        !isCurrentLiveGeneration(generation)
      ) {
        return;
      }
    } catch (error) {
      if (
        !isStartupAttemptCurrent(startupAttempt) ||
        (revealedGeneration !== null &&
          currentGenerationRef.current !== revealedGeneration)
      ) {
        return;
      }
      const updateDetails = updateRequiredDetailsFromError(error);
      if (updateDetails) {
        setVersionBlock(updateDetails);
        setSessionNotice(updateDetails.message);
      }
      currentGenerationRef.current = null;
      setIsLoggedIn(false);
      setAccountName('未登录');
      console.debug('会话恢复失败:', error);
    }
  }, [applyTerminalGeneration, isCurrentLiveGeneration, isStartupAttemptCurrent, runPostLoginReadiness]);

  const applyVersionBlock = useCallback(
    (check: VersionCheckPayload, reason = '版本不符合要求，请先更新。') => {
      const minVersion = check.min_version || check.minVersion;
      const targetVersion = minVersion || check.latest;
      const message = targetVersion
        ? `检测到新版本要求（最低 ${targetVersion}），请更新后继续使用。`
        : reason;

      setVersionBlock({
        message,
        latest: check.latest,
        minVersion: minVersion || null,
        downloadUrl: check.download_url,
      });
      setReadinessDialog(null);
      setSessionNotice(message);
      setIsLoggedIn(false);
      setAccountName('未登录');
      setLoginPassword('');
    },
    [],
  );

  const checkVersionGate = useCallback(async (startupAttempt: number) => {
    try {
      const version = await invoke<VersionCheckPayload>('version_check');
      if (!isStartupAttemptCurrent(startupAttempt)) {
        return false;
      }
      if (version.force_update || version.update_required) {
        applyVersionBlock(version);
        return false;
      }

      clearVersionBlock();
      return true;
    } catch (error) {
      if (!isStartupAttemptCurrent(startupAttempt)) {
        return false;
      }
      console.debug('版本检查失败，按容错策略继续启动:', error);
      clearVersionBlock();
      return true;
    }
  }, [applyVersionBlock, clearVersionBlock, isStartupAttemptCurrent]);

  const stopSession = useCallback(async () => {
    if (!hasTauriRuntime()) {
      setIsLoggedIn(false);
      setAccountName('未登录');
      return;
    }

    setIsBusyAction(true);
    setSessionNotice('');
    try {
      await invoke<void>('session_stop');
      await invoke<void>('auth_logout');
      currentGenerationRef.current = null;
      setIsLoggedIn(false);
      setAccountName('未登录');
      setLoginPassword('');
    } catch (error) {
      setSessionNotice(errorMessage(error, '登出失败，请重试。'));
    } finally {
      setIsBusyAction(false);
    }
  }, []);

  const refreshDevice = useCallback(async () => {
    try {
      const snapshot = await invoke<DeviceSnapshotPayload>('device_refresh');
      setDeviceSnapshot(snapshot);
    } catch (error) {
      console.debug('设备刷新失败:', error);
    }
  }, []);

  const closeWindow = useCallback(async () => {
    if (!hasTauriRuntime()) {
      return;
    }

    if (closingWindow.current) {
      return;
    }

    closingWindow.current = true;

    try {
      let sessionState: SessionStateV2Payload | null = null;
      try {
        sessionState = await invoke<SessionStateV2Payload>('session_state');
      } catch (error) {
        console.debug('关闭前读取会话状态失败:', error);
      }

      if (sessionState?.has_token || sessionState?.running) {
        try {
          await invoke<void>('session_stop');
        } catch (error) {
          console.debug('关闭前停止会话失败:', error);
        }
      }

      if (sessionState?.has_token || sessionState?.running) {
        try {
          await invoke<void>('auth_logout');
        } catch (error) {
          console.debug('关闭前清理认证状态失败:', error);
        }
      }
    } catch (error) {
      console.debug('关闭前会话收尾失败:', error);
    } finally {
      try {
        await getCurrentWindow().close();
      } catch (error) {
        console.debug('窗口关闭失败:', error);
      }
    }
  }, []);

  const minimizeWindow = useCallback(async () => {
    if (!hasTauriRuntime()) {
      return;
    }

    try {
      await getCurrentWindow().minimize();
    } catch (error) {
      console.debug('窗口最小化失败:', error);
    }
  }, []);

  const toggleMaximizeWindow = useCallback(async () => {
    if (!hasTauriRuntime()) {
      return;
    }

    try {
      await getCurrentWindow().toggleMaximize();
    } catch (error) {
      console.debug('窗口最大化切换失败:', error);
    }
  }, []);

  const startSession = useCallback(async () => {
    if (!hasTauriRuntime()) {
      setIsLoggedIn((value) => !value);
      return;
    }

    if (!loginUsername.trim() || !loginPassword.trim()) {
      setSessionNotice('账号与密码不能为空');
      return;
    }

    if (isVersionBlocked) {
      setSessionNotice('检测到更新要求，请先更新到新版本后登录。');
      return;
    }

    startupCancelledRef.current = true;
    setIsBusyAction(true);
    setSessionNotice('');
    const loginPayload = {
      username: loginUsername,
      password: loginPassword,
    };
    setLoginPassword('');
    try {
      const response = await invoke<AuthSessionPayload>('auth_login', loginPayload);
      const generation = response.generation;
      if (!generation) {
        currentGenerationRef.current = null;
        setIsLoggedIn(false);
        setAccountName('未登录');
        return;
      }
      const terminal = terminalGenerationsRef.current.get(generation);
      if (terminal) {
        currentGenerationRef.current = null;
        applyTerminalGeneration(terminal);
        return;
      }
      currentGenerationRef.current = generation;
      clearVersionBlock();
      setSessionNotice('');
      setAccountName(response.name || response.username || 'admin');
      setIsLoggedIn(true);
      await runPostLoginReadiness();
      if (!isCurrentLiveGeneration(generation)) {
        return;
      }
    } catch (error) {
      const updateDetails = updateRequiredDetailsFromError(error);
      if (updateDetails) {
        setVersionBlock(updateDetails);
        setSessionNotice(updateDetails.message);
      } else {
        setSessionNotice(errorMessage(error, '登录失败，请检查账号和密码'));
      }
      setIsLoggedIn(false);
      setAccountName('未登录');
    } finally {
      setIsBusyAction(false);
    }
  }, [
    applyTerminalGeneration,
    cancelStartupAttempt,
    clearVersionBlock,
    isCurrentLiveGeneration,
    isVersionBlocked,
    loginPassword,
    loginUsername,
    runPostLoginReadiness,
  ]);

  const closeResourceReadiness = useCallback(() => {
    setReadinessDialog(null);
    void showDriverReminderIfNeeded();
  }, [showDriverReminderIfNeeded]);

  const installDrivers = useCallback(async () => {
    setReadinessBusy(true);
    setDriverReadinessError('');
    try {
      await invoke('driver_reinstall');
      setReadinessDialog(null);
    } catch (error) {
      setDriverReadinessError(errorMessage(error, '驱动安装失败'));
    } finally {
      setReadinessBusy(false);
    }
  }, []);

  const canLogin =
    loginUsername.trim().length > 0 &&
    loginPassword.trim().length > 0 &&
    !isBusyAction &&
    !isVersionBlocked;

  useEffect(() => {
    const timer = setInterval(() => {
      setCurrentTime(formatCurrentTime());
    }, 1000);

    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!hasTauriRuntime()) {
      return;
    }

    let active = true;
    const syncWindowSize = async () => {
      try {
        const appWindow = getCurrentWindow();
        await appWindow.setResizable(isLoggedIn);
        await appWindow.setSize(isLoggedIn ? MAIN_WINDOW_SIZE : LOGIN_WINDOW_SIZE);
      } catch (error) {
        if (active) {
          console.debug('窗口尺寸同步失败:', error);
        }
      }
    };

    void syncWindowSize();
    return () => {
      active = false;
    };
  }, [isLoggedIn]);

  useEffect(() => {
    if (!hasTauriRuntime()) {
      setIsLoggedIn(true);
      setAccountName('admin');
      return;
    }

    let active = true;
    startupCancelledRef.current = false;
    const startupAttempt = startupAttemptRef.current + 1;
    startupAttemptRef.current = startupAttempt;
    const unlisteners: Array<() => void> = [];

    const captureListener = (listener: Promise<() => void>) => {
      return listener
        .then((unlisten) => {
          if (active) {
            unlisteners.push(unlisten);
          } else {
            unlisten();
          }
        })
        .catch((error) => {
          console.debug('Tauri event init ignored:', error);
        });
    };

    const listenersReady = Promise.all([
      captureListener(listen<OperationSnapshotPayload>(
        IPC_EVENTS.operationSnapshot,
        (event) => {
          if (active) {
            upsertOperation(event.payload);
          }
        },
      )),
      captureListener(listen<DeviceSnapshotPayload>(
        IPC_EVENTS.deviceSnapshot,
        (event) => {
          if (active) {
            setDeviceSnapshot(event.payload);
          }
        },
      )),
      captureListener(listen<SessionForceExitPayload>(
        IPC_EVENTS.sessionForceExit,
        (event) => {
          if (!active) {
            return;
          }
          const generation = event.payload.generation;
          const terminal: TerminalGenerationState = {
            kind: 'force-exit',
            reason: event.payload.reason,
          };
          rememberTerminalGeneration(generation, terminal);
          if (currentGenerationRef.current !== generation) {
            return;
          }
          currentGenerationRef.current = null;
          applyTerminalGeneration(terminal);
        },
      )),
      captureListener(listen<SessionUpdateRequiredPayload>(
        IPC_EVENTS.sessionUpdateRequired,
        (event) => {
          if (!active) {
            return;
          }
          const generation = event.payload.generation;
          const terminal: TerminalGenerationState = {
            kind: 'update-required',
            payload: event.payload,
          };
          rememberTerminalGeneration(generation, terminal);
          if (currentGenerationRef.current !== generation) {
            return;
          }
          currentGenerationRef.current = null;
          applyTerminalGeneration(terminal);
        },
      )),
    ]);

    const bootstrap = async () => {
      await listenersReady;
      if (!active || !isStartupAttemptCurrent(startupAttempt)) {
        return;
      }
      const canContinue = await checkVersionGate(startupAttempt);
      if (canContinue && active && isStartupAttemptCurrent(startupAttempt)) {
        await refreshSessionState(startupAttempt);
      }
    };

    bootstrap();

    return () => {
      active = false;
      cancelStartupAttempt();
      for (const unlisten of unlisteners) {
        unlisten();
      }
    };
  }, [
    applyTerminalGeneration,
    cancelStartupAttempt,
    clearVersionBlock,
    checkVersionGate,
    isStartupAttemptCurrent,
    refreshSessionState,
    rememberTerminalGeneration,
    upsertOperation,
  ]);

  if (!isLoggedIn) {
    return (
      <>
        <LoginScreen
          username={loginUsername}
          password={loginPassword}
          notice={sessionNotice}
          busy={isBusyAction}
          canSubmit={canLogin}
          onUsernameChange={setLoginUsername}
          onPasswordChange={setLoginPassword}
          onSubmit={startSession}
          onClose={closeWindow}
        />
        <UpdateRequiredDialog details={versionBlock} onQuit={closeWindow} />
      </>
    );
  }

  return (
      <AppShell
        appTitle={APP_BRAND}
        navGroups={NWFLASH_APP_PAGES}
        currentPage={currentPage}
        onSelectPage={setCurrentPage}
        operations={operations}
        operationSnapshot={operationSnapshot}
        isBusyAction={isBusyAction || readinessBusy}
        username={isLoggedIn ? accountName : '未登录'}
        currentTime={currentTime}
        isLoggedIn={isLoggedIn}
        deviceSnapshot={deviceSnapshot}
        onRefreshDevice={refreshDevice}
        onLogout={stopSession}
        onMinimize={minimizeWindow}
        onMaximize={toggleMaximizeWindow}
        onClose={closeWindow}
        modalOpen={readinessDialog !== null}
        modalTitle={readinessDialog === 'resources' ? '内置组件检查' : 'USB 驱动提醒'}
        modalChildren={readinessDialog === 'resources' ? (
          <ResourceDownloadPage
            embedded
            onInstallingChange={setReadinessBusy}
            onCompleted={closeResourceReadiness}
            onRequestClose={closeResourceReadiness}
          />
        ) : readinessDialog === 'drivers' ? (
          <section className="nw-driver-readiness">
            <h2>缺少手机 USB 驱动</h2>
            <p>当前电脑缺少 ADB 或 Fastboot 驱动，刷机和文件管理等功能可能无法识别手机。</p>
            {driverReadinessError ? <p className="nw-error-text">{driverReadinessError}</p> : null}
            <div className="nw-driver-dialog-actions">
              <button type="button" disabled={readinessBusy} onClick={() => setReadinessDialog(null)}>
                取消
              </button>
              <button type="button" disabled={readinessBusy} onClick={() => void installDrivers()}>
                {readinessBusy ? '安装中...' : '安装驱动'}
              </button>
            </div>
          </section>
        ) : null}
      >
      {!pageOwnsHeader ? (
        <header className="nw-page-header">
          <h1>{pages}</h1>
          <div className="nw-page-actions">
            {sessionNotice ? <p className="nw-session-notice">{sessionNotice}</p> : null}
            <button
              type="button"
              className="nw-test-login-button"
              onClick={stopSession}
              aria-label="退出登录"
              disabled={isBusyAction || operations.length > 0}
            >
              已登录
            </button>
          </div>
        </header>
      ) : null}
      <PageContainer flushTop={pageOwnsHeader}>
        <PageFactory
          page={currentPage}
          deviceSnapshot={deviceSnapshot}
          operationSnapshot={operationSnapshot}
        />
      </PageContainer>
    </AppShell>
  );
};

export default App;

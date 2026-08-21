import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { AppShell } from './AppShell';
import { NWFLASH_APP_PAGES } from '../app/pageManifest';
import type { BusyOperationItem } from '../app/window-state';

type RootHandle = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: RootHandle;

const renderShell = (
  operations: readonly BusyOperationItem[],
  extra: { isBusyAction?: boolean; onRefreshDevice?: () => void } = {},
) => {
  const shellProps = {
    appTitle: '奶蛙Flash',
    navGroups: NWFLASH_APP_PAGES,
    currentPage: 'Overview' as const,
    onSelectPage: vi.fn(),
    operations,
    isBusyAction: extra.isBusyAction,
    username: 'admin',
    currentTime: '12:00:00',
    isLoggedIn: true,
    onLogout: vi.fn(),
    ...(extra.onRefreshDevice ? { onRefreshDevice: extra.onRefreshDevice } : {}),
  };

  flushSync(() => {
    root.render(
      <AppShell {...shellProps}>
        <div data-shell-content>content</div>
      </AppShell>,
    );
  });
};

describe('AppShell', () => {
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
  });

  test('导航顺序与点击行为保持一致', () => {
    const onSelectPage = vi.fn();
    flushSync(() => {
      root.render(
        <AppShell
          appTitle="奶蛙Flash"
          navGroups={NWFLASH_APP_PAGES}
          currentPage="Overview"
          onSelectPage={onSelectPage}
          operations={[]}
          username="admin"
          currentTime="12:00:00"
          isLoggedIn
          onLogout={vi.fn()}
        >
          <div>content</div>
        </AppShell>,
      );
    });

    const labels = [...host.querySelectorAll('[data-page-id]')].map((button) =>
      button.textContent?.trim() || '',
    );
    expect(labels.join('|')).toBe(
      '设备概览|文件管理|ADB 投屏|快速刷写|可视刷写|VIVO 线刷|固件提取|Vivo ROOT|在线状态|软件',
    );

    const target = host.querySelector('[data-page-id="SafeFlash"]') as HTMLButtonElement;
    target.click();
    expect(onSelectPage).toHaveBeenCalledWith('SafeFlash');
  });

  test('无任务时显示空闲提示，任务按优先级显示', () => {
    renderShell([
      { kind: 'device', message: '设备未响应' },
      { kind: 'lineFlash', message: '镜像读取中' },
      { kind: 'quick', message: '正在刷写分区' },
    ]);

    const progress = host.querySelector('[data-role="operation-progress"]') as HTMLDivElement;
    expect(progress.textContent).toContain('快速刷写');
    expect(progress.textContent).toContain('正在刷写分区');
  });

  test('统一进度区保留 WPF 标题', () => {
    renderShell([]);

    expect(host.querySelector('[data-role="operation-progress"]')?.textContent).toContain('操作进度');
  });

  test('标题栏提供 WPF 全局设备刷新入口', () => {
    const onRefreshDevice = vi.fn();
    renderShell([], { onRefreshDevice });

    (host.querySelector('[aria-label="刷新设备"]') as HTMLButtonElement).click();
    expect(onRefreshDevice).toHaveBeenCalledOnce();
  });

  test('无边框窗口标题栏提供拖动区域且窗口按钮不属于拖动区域', () => {
    renderShell([]);

    const titlebar = host.querySelector('.nw-titlebar') as HTMLElement;
    const controls = host.querySelector('.nw-titlebar-controls') as HTMLElement;
    expect(titlebar.hasAttribute('data-tauri-drag-region')).toBe(true);
    expect(controls.hasAttribute('data-tauri-drag-region')).toBe(false);
  });

  test('有任务时登出按钮置灰', () => {
    renderShell([{ kind: 'safeFlash', message: '准备线刷资源' }]);
    const logout = host.querySelector('[data-role="logout-button"]') as HTMLButtonElement;
    expect(logout.disabled).toBe(true);
  });

  test('有登录操作时登出按钮置灰', () => {
    renderShell([], { isBusyAction: true });
    const logout = host.querySelector('[data-role="logout-button"]') as HTMLButtonElement;
    expect(logout.disabled).toBe(true);
  });

  test('无任务且无登录操作时登出按钮可用', () => {
    renderShell([]);
    expect((host.querySelector('[data-role="logout-button"]') as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  test('侧栏登出按钮保留退出登录的可访问名称', () => {
    renderShell([]);
    expect(host.querySelector('[data-role="logout-button"]')?.getAttribute('aria-label')).toBe('退出登录');
  });

  test('右侧状态区始终展示操作日志面板', () => {
    renderShell([]);
    const operationLogPanel = host.querySelector('[data-role="operation-log-panel"]') as HTMLDivElement;
    expect(operationLogPanel).not.toBeNull();
  });

  test('账号与登出归属左侧栏，右侧保留连续的设备状态轨道', () => {
    renderShell([]);

    expect(host.querySelector('.nw-sidebar .nw-sidebar-account')).not.toBeNull();
    expect(host.querySelector('.nw-status-rail .nw-device-status-panel')).not.toBeNull();
    expect(host.querySelector('.nw-status-rail .nw-progress-panel')).not.toBeNull();
    expect(host.querySelector('.nw-status-rail [data-role="operation-log-panel"]')).not.toBeNull();
    expect(host.querySelector('.nw-shell-side-tools > .nw-logout-button')).toBeNull();
  });
});

import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import type { DeviceSnapshotPayload } from '../app/ipc-events';
import { PageFactory } from './PageFactory';

type RootHandle = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: RootHandle;

const renderFileManager = (deviceSnapshot: DeviceSnapshotPayload) => {
  flushSync(() => {
    root.render(<PageFactory page="FileManager" deviceSnapshot={deviceSnapshot} />);
  });
};

const expectConnectionLabels = (label: string) => {
  expect(host.querySelector('.nw-file-manager-connection')?.textContent).toBe(label);
  expect(
    host.querySelector('.nw-file-manager-workbench > footer > span:first-child')?.textContent,
  ).toBe(label);
};

const expectToolbarDisabled = () => {
  const actions = [...host.querySelectorAll<HTMLButtonElement>('.nw-file-manager-toolbar button')];
  expect(actions.length).toBeGreaterThan(0);
  expect(actions.every((action) => action.disabled)).toBe(true);
};

describe('PageFactory file manager device snapshot flow', () => {
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

  test('将真实连接快照传给文件管理页并在头部和页脚显示后端连接标签', () => {
    renderFileManager({
      connection_state: 'AdbConnected',
      serial: 'RF8T123',
      connection_label: 'ADB 已连接',
      model: 'V2318A',
      android_version: '15',
      battery_level: '78%',
    });

    expectConnectionLabels('ADB 已连接');
    expect((host.querySelector('.nw-test-file-refresh') as HTMLButtonElement).disabled).toBe(false);
    expect((host.querySelector('.nw-test-file-upload') as HTMLButtonElement).disabled).toBe(false);
    expect((host.querySelector('.nw-test-file-install-apk') as HTMLButtonElement).disabled).toBe(false);
  });

  test('Fastboot 连接时显示文件管理不可用并禁用 ADB 文件操作', () => {
    renderFileManager({
      connection_state: 'FastbootConnected',
      serial: 'FAST8T123',
      connection_label: 'Fastboot 已连接',
      model: 'V2318A',
      android_version: '--',
      battery_level: '--',
    });

    expectConnectionLabels('Fastboot 模式，文件管理不可用');
    expectToolbarDisabled();
  });

  test('断开连接时文件管理页头部和页脚保持等待连接', () => {
    renderFileManager({
      connection_state: 'Disconnected',
      serial: '--',
      connection_label: '设备已断开',
      model: '未检测到设备',
      android_version: '--',
      battery_level: '--',
    });

    expectConnectionLabels('等待连接');
    expect(host.textContent).not.toContain('设备已断开');
    expectToolbarDisabled();
  });
});

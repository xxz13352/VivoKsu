import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { ResourceDownloadPage } from './ResourceDownloadPage';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { invoke } from '@tauri-apps/api/core';

type Root = ReturnType<typeof createRoot>;
let host: HTMLDivElement;
let root: Root;
const flushPromises = () => new Promise((resolve) => setTimeout(resolve, 0));
const waitUntil = async (predicate: () => boolean, timeoutMs = 1000) => {
  const start = Date.now();
  while (!predicate() && Date.now() - start < timeoutMs) await flushPromises();
  if (!predicate()) throw new Error('timeout waiting for async assertion');
};

const inventory = [
  { key: 'scrcpy', display_name: 'scrcpy 投屏', is_ready: false, default_selected: true },
  { key: 'payload', display_name: 'payload_dumper', is_ready: true, default_selected: false },
  { key: 'manager-KSU', display_name: 'KSU 管理器', is_ready: false, default_selected: true },
  { key: 'manager-OfficialKsu', display_name: 'KernelSU 管理器', is_ready: true, default_selected: false },
];

describe('ResourceDownloadPage', () => {
  beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(() => {
    flushSync(() => root.unmount());
    host.remove();
    vi.clearAllMocks();
  });

  test('缺失内置资源默认选中且已就绪资源不选中', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockResolvedValue(inventory);
    flushSync(() => root.render(<ResourceDownloadPage />));

    await waitUntil(() => (host.textContent ?? '').includes('KSU 管理器'));

    expect(invoke).toHaveBeenCalledWith('resource_inventory');
    expect((host.querySelector('[data-resource-key="scrcpy"]') as HTMLInputElement).checked).toBe(true);
    expect((host.querySelector('[data-resource-key="payload"]') as HTMLInputElement).checked).toBe(false);
    expect(host.textContent).toContain('校验所选 (2)');
  });

  test('校验只提交被选中的固定资源键', async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(inventory)
      .mockResolvedValueOnce(['scrcpy', 'manager-KSU'])
      .mockResolvedValueOnce(inventory);
    flushSync(() => root.render(<ResourceDownloadPage />));

    await waitUntil(() => (host.textContent ?? '').includes('校验所选 (2)'));
    (host.querySelector('.nw-test-resource-install') as HTMLButtonElement).click();

    await waitUntil(() => (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.length >= 2);
    expect(invoke).toHaveBeenCalledWith('resource_install', { keys: ['scrcpy', 'manager-KSU'] });
  });

  test('校验并刷新清单后通知宿主刷新组件状态', async () => {
    const onCompleted = vi.fn();
    (invoke as unknown as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce(inventory)
      .mockResolvedValueOnce(['scrcpy', 'manager-KSU'])
      .mockResolvedValueOnce(inventory);
    flushSync(() => root.render(<ResourceDownloadPage onCompleted={onCompleted} />));

    await waitUntil(() => (host.textContent ?? '').includes('校验所选 (2)'));
    (host.querySelector('.nw-test-resource-install') as HTMLButtonElement).click();

    await waitUntil(() => onCompleted.mock.calls.length === 1);
    expect(onCompleted).toHaveBeenCalledTimes(1);
  });

  test('校验中关闭会请求取消当前受控操作', async () => {
    const onRequestClose = vi.fn();
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation((command: string) => {
      if (command === 'resource_inventory') return Promise.resolve(inventory);
      if (command === 'resource_install') return new Promise(() => undefined);
      if (command === 'operation_cancel') return Promise.resolve();
      return Promise.resolve(null);
    });
    flushSync(() => root.render(<ResourceDownloadPage onRequestClose={onRequestClose} />));

    await waitUntil(() => (host.textContent ?? '').includes('校验所选 (2)'));
    (host.querySelector('.nw-test-resource-install') as HTMLButtonElement).click();
    await waitUntil(() => (host.textContent ?? '').includes('取消校验'));
    (host.querySelector('.nw-test-resource-close') as HTMLButtonElement).click();

    await waitUntil(() =>
      (invoke as unknown as ReturnType<typeof vi.fn>).mock.calls.some(
        ([command]) => command === 'operation_cancel',
      ),
    );
    expect(onRequestClose).toHaveBeenCalledTimes(1);
  });
});

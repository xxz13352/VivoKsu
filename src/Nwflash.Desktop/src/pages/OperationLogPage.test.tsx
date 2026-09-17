import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { OperationLogPage } from './OperationLogPage';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';

type Unmount = ReturnType<typeof createRoot>;

let host: HTMLDivElement;
let root: Unmount;
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

const renderOperationLog = () => {
  flushSync(() => {
    root.render(<OperationLogPage />);
  });
};

describe('OperationLogPage', () => {
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

  test('OperationLog 导航项保留 WPF 的空白中心工作区，日志只在右侧轨道读取', async () => {
    renderOperationLog();

    await waitUntil(() => host.querySelector('.nw-operation-log-workspace') !== null);
    expect(host.textContent?.trim()).toBe('');
    expect(invoke).not.toHaveBeenCalled();
  });
});

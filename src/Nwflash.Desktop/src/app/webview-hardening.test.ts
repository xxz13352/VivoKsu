import { describe, expect, it, beforeEach, vi } from 'vitest';
import {
  guardIpcEntrypoints,
  guardDebugShortcuts,
  installWebviewHardening,
  ipcIntegrityAlerts,
} from './webview-hardening';

describe('webview hardening', () => {
  beforeEach(() => {
    // 每个用例从干净的 window 形状开始，避免用例间互相污染。
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    delete (window as unknown as Record<string, unknown>).__TAURI_IPC__;
  });

  /**
   * Tauri 的真实时序是：WebView 先注入 `__TAURI_INTERNALS__`，再执行应用
   * 代码。基线必须在"注入之后、加固之前"捕获，因此每个用例都先布置 window、
   * 再 resetModules 重新导入，这样模块加载时就看到了正确的基线。
   */
  async function importWithInjectedIpc(invoke: unknown) {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = { invoke };
    vi.resetModules();
    return import('./webview-hardening');
  }

  it('release 构建启用时会钉住 __TAURI_INTERNALS__.invoke', async () => {
    const invoke = () => 'ok';
    const mod = await importWithInjectedIpc(invoke);

    mod.installWebviewHardening(true);

    const pinned = (window as unknown as { __TAURI_INTERNALS__: { invoke: unknown } })
      .__TAURI_INTERNALS__.invoke;
    expect(pinned).toBe(invoke);
  });

  it('重写被钉住的 invoke 不会生效，并记录一条告警', async () => {
    const invoke = () => 'ok';
    const mod = await importWithInjectedIpc(invoke);
    mod.installWebviewHardening(true);
    const before = mod.ipcIntegrityAlerts().length;

    const host = (window as unknown as { __TAURI_INTERNALS__: { invoke: unknown } })
      .__TAURI_INTERNALS__;
    host.invoke = () => 'hijacked';

    expect(host.invoke).toBe(invoke);
    expect(mod.ipcIntegrityAlerts().length).toBeGreaterThan(before);
    expect(mod.ipcIntegrityAlerts().at(-1)?.kind).toBe('ipc-hook-rewritten');
  });

  it('当前值偏离基线时会被留痕，并钉回基线', async () => {
    const realInvoke = () => 'real';
    const mod = await importWithInjectedIpc(realInvoke);

    // 模拟"加固前有人先动手"。
    const hijacked = () => 'hijacked';
    const host = (window as unknown as { __TAURI_INTERNALS__: { invoke: unknown } })
      .__TAURI_INTERNALS__;
    host.invoke = hijacked;

    mod.guardIpcEntrypoints();

    expect(
      mod
        .ipcIntegrityAlerts()
        .some((alert) => alert.detail.includes('在加固前已被替换')),
    ).toBe(true);
    expect(host.invoke).toBe(realInvoke);
  });
  it('开发态（enabled=false）不做任何加固', () => {
    const invoke = () => 'ok';
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = { invoke };
    const before = ipcIntegrityAlerts().length;

    installWebviewHardening(false);

    // 仍可被自由替换，说明没有钉住。
    const host = (window as unknown as { __TAURI_INTERNALS__: { invoke: unknown } })
      .__TAURI_INTERNALS__;
    const replacement = () => 'replaced';
    host.invoke = replacement;
    expect(host.invoke).toBe(replacement);
    expect(ipcIntegrityAlerts().length).toBe(before);
  });

  it('F12 被拦截且记录 debug-shortcut 告警', () => {
    guardDebugShortcuts();
    const before = ipcIntegrityAlerts().length;

    const event = new KeyboardEvent('keydown', {
      key: 'F12',
      bubbles: true,
      cancelable: true,
    });
    window.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    expect(ipcIntegrityAlerts().length).toBeGreaterThan(before);
    expect(ipcIntegrityAlerts().at(-1)?.kind).toBe('debug-shortcut');
  });

  it('Ctrl+Shift+I 被拦截', () => {
    guardDebugShortcuts();
    const event = new KeyboardEvent('keydown', {
      key: 'I',
      ctrlKey: true,
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    });
    window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
  });

  it('普通按键不被拦截（不得影响正常输入）', () => {
    guardDebugShortcuts();
    const event = new KeyboardEvent('keydown', {
      key: 'a',
      bubbles: true,
      cancelable: true,
    });
    window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
  });

  it('右键菜单被阻止', () => {
    guardDebugShortcuts();
    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true });
    window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
  });
});
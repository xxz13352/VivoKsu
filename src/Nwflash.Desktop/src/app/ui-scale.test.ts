import { afterEach, describe, expect, test } from 'vitest';
import {
  UI_SCALE_DESIGN_WIDTH,
  UI_SCALE_MAX,
  applyUiScale,
  resolveUiScale,
} from './ui-scale';

describe('ui-scale', () => {
  test('主窗口设计宽度及以下都不缩放', () => {
    expect(resolveUiScale(400)).toBe(1);
    expect(resolveUiScale(1240)).toBe(1);
    expect(resolveUiScale(UI_SCALE_DESIGN_WIDTH)).toBe(1);
  });

  test('放大到 1920 宽时按比例放大到 1.35', () => {
    // 1920 / 1440 = 1.333… → 量化到 0.05 → 1.35
    expect(resolveUiScale(1920)).toBe(1.35);
  });

  test('缩放后 innerWidth 变窄，但换算结果保持不变（不自激振荡）', () => {
    const applied = resolveUiScale(1920);
    expect(applied).toBe(1.35);

    // 放大 1.35 后 1920 的窗口只剩 1422 个 CSS px；乘回已生效级别再算仍为 1.35，
    // 否则 resize 事件会把缩放级别反复在 1 与 1.35 之间来回切。
    const replayed = resolveUiScale(1920 / applied, applied);
    expect(replayed).toBe(applied);
  });

  test('缩放级别随宽度单调不减', () => {
    const widths = [1240, 1440, 1600, 1728, 1920, 2100, 2560, 3840];
    const factors = widths.map((width) => resolveUiScale(width));
    for (let index = 1; index < factors.length; index += 1) {
      expect(factors[index]).toBeGreaterThanOrEqual(factors[index - 1] as number);
    }
    expect(factors[0]).toBe(1);
  });

  test('超宽屏封顶 1.5，不会无限放大', () => {
    expect(resolveUiScale(2560)).toBe(UI_SCALE_MAX);
    expect(resolveUiScale(3840)).toBe(UI_SCALE_MAX);
  });

  test('非法宽度回退到不缩放', () => {
    expect(resolveUiScale(0)).toBe(1);
    expect(resolveUiScale(-100)).toBe(1);
    expect(resolveUiScale(Number.NaN)).toBe(1);
    expect(resolveUiScale(Number.POSITIVE_INFINITY)).toBe(1);
  });

  test('注入层没有 currentWebview 时只换算、不调用缩放 IPC', async () => {
    // 真实 Tauri 运行时会走到 setZoom；测试/浏览器环境不应产生未捕获异常，
    // 返回值仍是换算后的级别（便于调用方记录）。
    (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {
      invoke: () => Promise.resolve(),
    };

    await expect(applyUiScale(1920, 1)).resolves.toBe(1.35);
    await expect(applyUiScale(400, 1)).resolves.toBe(1);
  });

  afterEach(() => {
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });
});

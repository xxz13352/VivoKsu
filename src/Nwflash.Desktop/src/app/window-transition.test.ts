import { LogicalSize } from '@tauri-apps/api/dpi';
import { describe, expect, test, vi } from 'vitest';
import {
  WINDOW_TRANSITION_INTERVAL_MS,
  animateWindowSize,
  easeOutCubic,
  interpolateSize,
  type TransitionableWindow,
} from './window-transition';

type RecordedFrame = { width: number; height: number; centered: boolean };

const createFakeWindow = (start: { width: number; height: number }) => {
  const frames: RecordedFrame[] = [];
  let current = { ...start };

  const window: TransitionableWindow = {
    innerSize: vi.fn(async () => ({
      toLogical: () => current,
    })) as unknown as TransitionableWindow['innerSize'],
    scaleFactor: vi.fn(async () => 1),
    setSize: vi.fn(async (size: LogicalSize) => {
      current = { width: size.width, height: size.height };
      frames.push({ ...current, centered: false });
    }),
    center: vi.fn(async () => {
      const last = frames[frames.length - 1];
      if (last) {
        last.centered = true;
      }
    }),
  };

  return { window, frames };
};

describe('window-transition', () => {
  test('缓出曲线两端为 0 / 1 且单调不减', () => {
    expect(easeOutCubic(0)).toBe(0);
    expect(easeOutCubic(1)).toBe(1);
    for (let step = 1; step <= 10; step += 1) {
      expect(easeOutCubic(step / 10)).toBeGreaterThan(easeOutCubic((step - 1) / 10));
    }
  });

  test('插值端点与中点', () => {
    const from = { width: 400, height: 564 };
    const to = { width: 1240, height: 700 };
    expect(interpolateSize(from, to, 0)).toEqual({ width: 400, height: 564 });
    expect(interpolateSize(from, to, 1)).toEqual({ width: 1240, height: 700 });
    expect(interpolateSize(from, to, 0.5)).toEqual({ width: 820, height: 632 });
  });

  test('登录到主界面逐帧放大，且每一帧都重新居中，最后一帧尺寸精确', async () => {
    const { window, frames } = createFakeWindow({ width: 400, height: 564 });

    await animateWindowSize(window, new LogicalSize(1240, 700), 4);

    // 4 帧过渡 + finally 补一帧精确尺寸
    expect(frames).toHaveLength(5);
    expect(frames[frames.length - 1]).toEqual({ width: 1240, height: 700, centered: true });
    // 每一帧都必须居中：否则就是从左上角往右下角长出去
    expect(frames.every((frame) => frame.centered)).toBe(true);
    // 尺寸单调不减
    for (let index = 1; index < frames.length; index += 1) {
      expect(frames[index].width).toBeGreaterThanOrEqual(frames[index - 1].width);
      expect(frames[index].height).toBeGreaterThanOrEqual(frames[index - 1].height);
    }
    // 第一帧不能直接跳到目标尺寸（否则退化成「闪现」）
    expect(frames[0].width).toBeLessThan(1240);
  });

  test('登出时逐帧缩小到登录窗尺寸', async () => {
    const { window, frames } = createFakeWindow({ width: 1240, height: 700 });

    await animateWindowSize(window, new LogicalSize(400, 564), 4);

    expect(frames[frames.length - 1]).toEqual({ width: 400, height: 564, centered: true });
    expect(frames.every((frame) => frame.centered)).toBe(true);
    expect(frames[0].width).toBeGreaterThan(400);
  });

  test('尺寸已经一致时不跑动画，只校准一次并居中', async () => {
    const { window, frames } = createFakeWindow({ width: 400, height: 564 });

    await animateWindowSize(window, new LogicalSize(400, 564));

    expect(frames).toEqual([{ width: 400, height: 564, centered: true }]);
  });

  test('读不到当前尺寸时退化为直接跳到位，不抛错', async () => {
    const frames: RecordedFrame[] = [];
    const window = {
      innerSize: vi.fn(async () => {
        throw new Error('no metadata');
      }),
      scaleFactor: vi.fn(async () => 1),
      setSize: vi.fn(async (size: LogicalSize) => {
        frames.push({ width: size.width, height: size.height, centered: false });
      }),
      center: vi.fn(async () => {
        frames[frames.length - 1].centered = true;
      }),
    } as unknown as TransitionableWindow;

    await expect(animateWindowSize(window, new LogicalSize(1240, 700))).resolves.toBeUndefined();
    expect(frames).toEqual([{ width: 1240, height: 700, centered: true }]);
  });

  test('过渡间隔为正数，避免逐帧调用把窗口管理器打满', () => {
    expect(WINDOW_TRANSITION_INTERVAL_MS).toBeGreaterThan(0);
  });
});

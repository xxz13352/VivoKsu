import { LogicalSize, type PhysicalSize } from '@tauri-apps/api/dpi';

// 登录窗 400×564、主界面 1240×700。直接 setSize 的话窗口是从**左上角**往右下角
// 长出去的，长完再 center() 就变成「先向右下展开、再突然闪现到屏幕中间」
// （2026-09-19 报障原话）。这里改成逐帧过渡，并且**每帧都重新居中**，
// 视觉上就是窗口从屏幕中心向四周展开。
export const WINDOW_TRANSITION_STEPS = 10;
export const WINDOW_TRANSITION_INTERVAL_MS = 18;

export type WindowSizeLike = { readonly width: number; readonly height: number };

/** animateWindowSize 实际用到的最小窗口接口，便于单测注入假窗口。 */
export type TransitionableWindow = {
  innerSize: () => Promise<PhysicalSize>;
  scaleFactor: () => Promise<number>;
  setSize: (size: LogicalSize) => Promise<void>;
  center: () => Promise<void>;
};

export const easeOutCubic = (progress: number): number => 1 - (1 - progress) ** 3;

export const interpolateSize = (
  from: WindowSizeLike,
  to: WindowSizeLike,
  progress: number,
): { width: number; height: number } => ({
  width: Math.round(from.width + (to.width - from.width) * progress),
  height: Math.round(from.height + (to.height - from.height) * progress),
});

const wait = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

const measureSize = async (appWindow: TransitionableWindow): Promise<WindowSizeLike | null> => {
  try {
    const scaleFactor = await appWindow.scaleFactor();
    const current = (await appWindow.innerSize()).toLogical(scaleFactor);
    return { width: current.width, height: current.height };
  } catch {
    // 读不到当前尺寸（注入层缺 API）时退化成「直接跳到位」，不阻断登录。
    return null;
  }
};

/**
 * 把窗口过渡到目标尺寸，全程保持居中。
 *
 * - 每帧 `setSize` + `center()`：窗口围绕屏幕中心展开，而不是从左上角长出去。
 * - `finally` 里再补一次精确尺寸：任何一帧失败也不会把窗口停在中间尺寸，
 *   只是可能少一次居中（ACL 缺 allow-center 时的降级行为）。
 */
export const animateWindowSize = async (
  appWindow: TransitionableWindow,
  target: LogicalSize,
  steps = WINDOW_TRANSITION_STEPS,
): Promise<void> => {
  const from = await measureSize(appWindow);
  const unchanged =
    from !== null &&
    Math.abs(from.width - target.width) < 1 &&
    Math.abs(from.height - target.height) < 1;

  if (from === null || unchanged) {
    await appWindow.setSize(target);
    await appWindow.center();
    return;
  }

  try {
    for (let step = 1; step <= steps; step += 1) {
      const next = interpolateSize(from, target, easeOutCubic(step / steps));
      // 尺寸和居中必须在**同一个 tick** 里下发。中间夹一次 await（IPC 往返通常
      // 跨过一个渲染帧）就会把「尺寸已经变了、位置还没动」的那一帧合成出来，
      // 用户看到的就是每一帧先向右下长一下、再抖回中间。
      await Promise.all([
        appWindow.setSize(new LogicalSize(next.width, next.height)),
        appWindow.center(),
      ]);
      if (step < steps) {
        await wait(WINDOW_TRANSITION_INTERVAL_MS);
      }
    }
  } finally {
    // 收尾这一次刻意串行：center() 要读到最终尺寸再算坐标，否则会差半个帧步长。
    await appWindow.setSize(target);
    await appWindow.center();
  }
};

import { getCurrentWebview } from '@tauri-apps/api/webview';

// 主界面的设计密度是按 1240×700 的窗口定的，字号集中在 9–14px。
// 用户点「放大」把窗口拉到 1920×1032 后，绝对像素不变 → 文字和组件显得很小，
// 中间内容还被摊平、右半边留大片空白。
//
// 处理办法：按窗口宽度等比放大 webview 缩放级别（等同浏览器 Ctrl+加号），
// 文字、间距、组件一起变大，布局宽度回落到底层断点之内，密度回到设计值。
// 只放大、不缩小（≤1440 不缩放），上限 1.5 防止 4K 屏上变成「放大镜」。
export const UI_SCALE_DESIGN_WIDTH = 1440;
export const UI_SCALE_MAX = 1.5;

// 量化到 0.05，避免拖拽窗口时缩放级别频繁抖动触发重排。
const UI_SCALE_STEP = 0.05;

/**
 * 换算缩放级别。
 *
 * `viewportWidth` 是 `window.innerWidth`，**会随缩放本身变化**：放大到 1.35 后
 * 1920 的物理宽度只剩 1422 个 CSS px。所以要乘回 `appliedScale` 还原成
 * 「未缩放时的等效宽度」再判断，否则会出现「设缩放 → 触发 resize → 又算出
 * 另一个缩放」的自激振荡。
 */
export const resolveUiScale = (viewportWidth: number, appliedScale = 1): number => {
  const referenceWidth = viewportWidth * (appliedScale > 0 ? appliedScale : 1);
  if (!Number.isFinite(referenceWidth) || referenceWidth <= UI_SCALE_DESIGN_WIDTH) {
    return 1;
  }

  const snapped =
    Math.round(referenceWidth / UI_SCALE_DESIGN_WIDTH / UI_SCALE_STEP) * UI_SCALE_STEP;
  return Math.min(UI_SCALE_MAX, Number(snapped.toFixed(2)));
};

const hasWebviewMetadata = (): boolean => {
  if (typeof window === 'undefined') {
    return false;
  }

  const runtime = window as Window & {
    __TAURI_INTERNALS__?: {
      invoke?: unknown;
      metadata?: { currentWebview?: unknown };
    };
  };
  return (
    typeof runtime.__TAURI_INTERNALS__?.invoke === 'function' &&
    Boolean(runtime.__TAURI_INTERNALS__?.metadata?.currentWebview)
  );
};

/**
 * 换算并下发缩放级别，返回实际生效的级别（供调用方记住，下次当 `appliedScale`）。
 * 非 Tauri 运行时只换算不调用；`setZoom` 失败时抛出，调用方保留旧的已生效级别。
 */
export const applyUiScale = async (
  viewportWidth: number,
  appliedScale = 1,
): Promise<number> => {
  const factor = resolveUiScale(viewportWidth, appliedScale);
  if (!hasWebviewMetadata()) {
    return factor;
  }

  await getCurrentWebview().setZoom(factor);
  return factor;
};

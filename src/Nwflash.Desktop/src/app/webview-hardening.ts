/**
 * WebView 侧加固：防 IPC 重写 + 禁调试快捷键。
 *
 * ## 威胁模型
 *
 * 前端在 Tauri 里是**不受信任**的一方：用户能在 DevTools 里改 `window` 上的
 * 任何东西。真正不能被绕过的是 Rust 侧的 `guard_write_command`。本模块做的是
 * **提高门槛与留痕**，不是把 WebView 变成可信边界——这一点必须写清楚，否则
 * 后来者容易误以为前端校验有安全意义。
 *
 * 具体防两件事：
 * 1. `window.__TAURI_INTERNALS__.invoke` / `window.__TAURI_IPC__` 被替换成
 *    代理（把命令参数记下来、或放行未授权命令）。
 * 2. 打开 DevTools 的常规快捷键（F12 / Ctrl+Shift+I / Ctrl+Shift+J / Ctrl+U）。
 *
 * ## 检测到劫持后的行为
 *
 * **只报警、不中断**。理由与 P1 反调试一致：本项目是刷机工具，用户可能正处于
 * 写入设备的中途。此时若直接禁用 IPC 或退出，会导致**设备变砖**——这是绝对的
 * 底线。劫持只意味着"本地环境可能被观察"，不意味着"当前刷机不安全到必须停止"。
 */

/** 记录一次完整性告警；由调用方决定上报或展示。 */
export type IpcIntegrityAlert = {
  kind: 'ipc-hook-rewritten' | 'debug-shortcut';
  detail: string;
};

const alerts: IpcIntegrityAlert[] = [];
const listeners = new Set<(alert: IpcIntegrityAlert) => void>();

/** 已发生的告警（只读快照），供 UI 或诊断使用。 */
export function ipcIntegrityAlerts(): readonly IpcIntegrityAlert[] {
  return alerts;
}

/** 订阅新的告警。返回取消订阅函数。 */
export function onIpcIntegrityAlert(
  listener: (alert: IpcIntegrityAlert) => void,
): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function record(alert: IpcIntegrityAlert): void {
  alerts.push(alert);
  for (const listener of listeners) {
    try {
      listener(alert);
    } catch {
      // 订阅者自身的异常不得影响加固逻辑，也不得中断刷机流程。
    }
  }
}

type InvokeHost = {
  __TAURI_INTERNALS__?: { invoke?: unknown };
  __TAURI_IPC__?: unknown;
};

/**
 * 用不可配置、不可写的存取器"钉住"关键属性。
 *
 * 若属性已被替换（`get` 返回的不是我们记住的原函数），说明有人在我们之前
 * 动过手——此时**不做任何修复**，只留痕：把属性改回已知原值会掩盖攻击，
 * 而且与原值不同的实现可能正是某个中间件层的正常行为（如 E2E 桥），
 * 静默覆盖会把测试环境弄坏。
 */
/** 加固前捕获的 IPC 入口基线。模块加载即建立，早于任何应用代码。 */
const ipcBaseline = (() => {
  if (typeof window === 'undefined') return { invoke: undefined, ipc: undefined };
  const host = window as unknown as InvokeHost;
  const internals = host.__TAURI_INTERNALS__;
  return {
    invoke:
      internals && typeof internals === 'object' ? internals.invoke : undefined,
    ipc: host.__TAURI_IPC__,
  };
})();

/**
 * 用不可配置、不可写的存取器把关键属性钉在**基线值**上。
 *
 * `baseline` 是模块加载时捕获的原值，不是"进入函数时的当前值"——后者会
 * 让比较恒等、检测彻底失效（实现早期版本的真实缺陷）。若当前值已经与
 * 基线不同，说明有人在加固前替换过：此时**只留痕、不修复**，因为静默覆盖
 * 会掩盖攻击，也可能破坏 E2E 桥这类合法的中间层。
 */
function pinProperty(
  target: object,
  key: string,
  baseline: unknown,
  label: string,
): void {
  const descriptor = Object.getOwnPropertyDescriptor(target, key);
  const current = descriptor ? descriptor.value : undefined;
  if (
    current !== undefined &&
    baseline !== undefined &&
    current !== baseline
  ) {
    record({
      kind: 'ipc-hook-rewritten',
      detail: `${label} 在加固前已被替换；保留现状用于诊断，未做修复。`,
    });
  }
  try {
    Object.defineProperty(target, key, {
      configurable: false,
      enumerable: descriptor?.enumerable ?? false,
      get: () => baseline,
      set: () => {
        record({
          kind: 'ipc-hook-rewritten',
          detail: `${label} 被尝试重写，已忽略写入。`,
        });
      },
    });
  } catch {
    // 某些 WebView 版本不允许重定义；此时的保护退化为"检测"。
    record({
      kind: 'ipc-hook-rewritten',
      detail: `${label} 无法钉住（WebView 拒绝 defineProperty）。`,
    });
  }
}

/**
 * 钉住 Tauri 的 IPC 入口。
 *
 * 注意这里**只钉住函数引用**，不包装调用本身：包装会引入我们自己的错误路径，
 * 在刷机中途抛异常比被观察更危险。
 */
export function guardIpcEntrypoints(): void {
  if (typeof window === 'undefined') return;
  const host = window as unknown as InvokeHost;
  const internals = host.__TAURI_INTERNALS__;
  // 用模块加载时的基线作为"应当是什么"，而不是当前值。
  const baselineInvoke = ipcBaseline.invoke;
  if (internals && typeof internals === 'object' && baselineInvoke !== undefined) {
    pinProperty(
      internals,
      'invoke',
      baselineInvoke,
      '__TAURI_INTERNALS__.invoke',
    );
  }
  if (ipcBaseline.ipc !== undefined) {
    pinProperty(host, '__TAURI_IPC__', ipcBaseline.ipc, '__TAURI_IPC__');
  }
}

/** 阻止 DevTools 快捷键与右键菜单。 */
export function guardDebugShortcuts(): void {
  if (typeof window === 'undefined') return;

  window.addEventListener(
    'keydown',
    (event: KeyboardEvent) => {
      const key = event.key.toLowerCase();
      const isF12 = event.key === 'F12';
      const isDevtoolsChord =
        event.ctrlKey && event.shiftKey && (key === 'i' || key === 'j' || key === 'c');
      const isViewSource = event.ctrlKey && key === 'u';
      if (!isF12 && !isDevtoolsChord && !isViewSource) return;
      event.preventDefault();
      event.stopPropagation();
      record({
        kind: 'debug-shortcut',
        detail: `已拦截调试快捷键：${event.key}。`,
      });
    },
    { capture: true },
  );

  window.addEventListener(
    'contextmenu',
    (event: MouseEvent) => {
      event.preventDefault();
    },
    { capture: true },
  );
}

/**
 * 安装全部 WebView 侧加固。应在 `main.tsx` 渲染前调用一次。
 *
 * @param enabled release 构建才启用。开发态必须放行，否则无法调试。
 */
export function installWebviewHardening(enabled: boolean): void {
  if (!enabled) return;
  guardIpcEntrypoints();
  guardDebugShortcuts();
}
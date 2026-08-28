import { createElement, renderPageState } from "../components.js";

export function createOverviewPage(context) {
  const element = createElement(context.document, "section", { className: "workspace-page overview-page" });
  let active = false;
  let destroyed = false;
  let generation = 0;
  let controller = null;
  let externalSignal = null;
  let externalAbort = null;

  function stopRequest() {
    controller?.abort();
    controller = null;
    externalSignal?.removeEventListener("abort", externalAbort);
    externalSignal = null;
    externalAbort = null;
  }

  function beginActivation(signal) {
    stopRequest();
    active = true;
    const owned = new AbortController();
    const request = { generation: ++generation, signal: owned.signal };
    controller = { abort: () => owned.abort(), signal: owned.signal };
    externalSignal = signal ?? null;
    externalAbort = () => controller?.abort();
    if (externalSignal?.aborted) controller.abort();
    else externalSignal?.addEventListener("abort", externalAbort, { once: true });
    return request;
  }

  function currentRequest() {
    return { generation, signal: controller?.signal ?? null };
  }

  function isCurrent(request) {
    return active && !destroyed && request.generation === generation && controller?.signal === request.signal && !request.signal?.aborted;
  }

  async function load() {
    const request = currentRequest();
    if (!isCurrent(request)) return false;
    renderPageState(element, { state: "loading", title: "正在加载概览" });
    try {
      const overview = await context.api.getTraceOverview({}, { signal: request.signal });
      if (!isCurrent(request)) return false;
      const totals = overview?.totals ?? {};
      const values = [
        ["API 用户", totals.api_users ?? 0],
        ["在线会话", totals.online_sessions ?? 0],
        ["操作总数", totals.operations ?? 0],
        ["失败", totals.failed ?? 0],
      ];
      element.replaceChildren(
        createElement(context.document, "section", { className: "workspace-card overview-workspace", "data-page-state": "ready" }, [
          createElement(context.document, "h2", {}, "权威运行概览"),
          createElement(context.document, "div", { className: "metric-grid" }, values.map(([label, value]) =>
            createElement(context.document, "div", { className: "metric" }, [
              createElement(context.document, "span", { className: "metric-label" }, label),
              createElement(context.document, "strong", { className: "metric-value" }, String(value)),
            ]),
          )),
          createElement(context.document, "h3", {}, "最近失败"),
          createElement(context.document, "ul", { className: "recent-failures" }, (overview?.recent_failures ?? []).length
            ? overview.recent_failures.map((item) => createElement(context.document, "li", {}, `${item.title ?? item.trace_ref ?? "未知操作"} · ${item.outcome ?? "unknown"}`))
            : createElement(context.document, "li", { className: "muted" }, "当前没有失败记录。")),
        ]),
      );
      element.dataset.pageState = "ready";
      return true;
    } catch (error) {
      if (!isCurrent(request)) return false;
      renderPageState(element, {
        state: "retry",
        title: "加载失败",
        message: error?.message ?? "概览数据暂不可用。",
        onRetry: () => { if (isCurrent(request)) void load(); },
      });
      return false;
    }
  }

  function deactivate() {
    active = false;
    generation += 1;
    stopRequest();
  }

  return {
    element,
    async activate(_route, signal) {
      if (destroyed) throw new Error("概览页面已销毁。");
      beginActivation(signal);
      await load();
    },
    deactivate,
    destroy() {
      if (destroyed) return;
      destroyed = true;
      deactivate();
      element.replaceChildren();
    },
  };
}

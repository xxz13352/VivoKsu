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
      const now = Date.now();
      const today = new Date(now);
      today.setHours(0, 0, 0, 0);
      const [dailyOverview, trailingOverview] = await Promise.all([
        context.api.getTraceOverview({ from: today.getTime(), to: now, bucket: "hour" }, { signal: request.signal }),
        context.api.getTraceOverview({ from: now - 86_400_000, to: now, bucket: "hour" }, { signal: request.signal }),
      ]);
      if (!isCurrent(request)) return false;
      if (!dailyOverview?.totals || !Array.isArray(trailingOverview?.trend) || !Array.isArray(trailingOverview?.recent_failures)) {
        renderPageState(element, { state: "partial", title: "概览数据不完整", message: "服务器未返回完整的权威概览结构。" });
        return false;
      }
      const totals = dailyOverview?.totals ?? {};
      const values = [
        ["API 用户", totals.api_users ?? 0],
        ["在线会话", totals.online_sessions ?? 0],
        ["今日操作", totals.operations ?? 0],
        ["今日失败", totals.failed ?? 0],
      ];
      const trend = Array.isArray(trailingOverview?.trend) ? trailingOverview.trend : [];
      const recentFailures = Array.isArray(trailingOverview?.recent_failures) ? trailingOverview.recent_failures : [];
      element.replaceChildren(
        createElement(context.document, "section", { className: "workspace-card overview-workspace", "data-page-state": "ready" }, [
          createElement(context.document, "h2", {}, "权威运行概览"),
          createElement(context.document, "div", { className: "metric-grid" }, values.map(([label, value]) =>
            createElement(context.document, "div", { className: "metric" }, [
              createElement(context.document, "span", { className: "metric-label" }, label),
              createElement(context.document, "strong", { className: "metric-value" }, String(value)),
            ]),
          )),
          createElement(context.document, "h3", {}, "最近 24 小时趋势"),
          trend.length > 0
            ? createElement(context.document, "ol", { className: "overview-trend", "aria-label": "最近 24 小时操作趋势" }, trend.map((bucket) =>
              createElement(context.document, "li", {
                className: "overview-trend-bucket",
                "data-trend-bucket": String(bucket.bucket_start_ms),
              }, [
                createElement(context.document, "span", { "data-bucket-time": "true" }, formatBucketTime(bucket.bucket_start_ms)),
                createElement(context.document, "span", {}, ["操作 ", createElement(context.document, "strong", { "data-trend-value": "operations" }, String(bucket.operations))]),
                createElement(context.document, "span", {}, ["失败 ", createElement(context.document, "strong", { "data-trend-value": "failed" }, String(bucket.failed))]),
              ]),
            ))
            : createElement(context.document, "p", { className: "muted", "data-trend-empty": "true" }, "最近 24 小时暂无趋势数据。"),
          createElement(context.document, "h3", {}, "最近失败"),
          createElement(context.document, "ul", { className: "recent-failures" }, recentFailures.length
            ? recentFailures.map((item) => createRecentFailure(context, item))
            : createElement(context.document, "li", { className: "muted", "data-failures-empty": "true" }, "当前没有失败记录。")),
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

function createRecentFailure(context, item) {
  const traceRef = typeof item?.trace_ref === "string" ? item.trace_ref : null;
  if (!traceRef) {
    return createElement(context.document, "li", { className: "muted" }, `${item?.title ?? "未知操作"} · ${item?.outcome ?? "unknown"}`);
  }
  const focusId = `overview-failure-${stableFocusKey(traceRef)}`;
  const button = createElement(context.document, "button", {
    type: "button",
    className: "overview-failure-button",
    "data-overview-action": "open-run",
    "data-router-focus-id": focusId,
  }, [
    createElement(context.document, "strong", {}, item?.title ?? "未知操作"),
    createElement(context.document, "span", {}, item?.outcome ?? "unknown"),
    createElement(context.document, "code", {}, traceRef),
  ]);
  button.addEventListener("click", () => {
    void Promise.resolve(context.navigate({
      view: "audit",
      level: "run",
      userId: Number.isSafeInteger(Number(item?.user_id)) ? String(item.user_id) : null,
      runId: traceRef,
      eventId: null,
      stream: null,
      cursor: null,
    }, { focusId })).catch((error) => {
      context.alert?.(error?.message ?? "无法打开失败记录。", { title: "无法打开审计记录" });
    });
  });
  return createElement(context.document, "li", {}, button);
}

function formatBucketTime(value) {
  const timestamp = Number(value);
  if (!Number.isSafeInteger(timestamp) || timestamp < 0) return "未提供";
  try {
    return new Date(timestamp).toISOString();
  } catch {
    return "未提供";
  }
}

function stableFocusKey(value) {
  const source = String(value);
  let first = 0x811c9dc5;
  let second = 0x9e3779b9;
  for (let index = 0; index < source.length; index += 1) {
    const code = source.charCodeAt(index);
    first = Math.imul(first ^ code, 0x01000193);
    second = Math.imul(second ^ code, 0x85ebca6b);
  }
  return `${(first >>> 0).toString(16).padStart(8, "0")}${(second >>> 0).toString(16).padStart(8, "0")}`;
}

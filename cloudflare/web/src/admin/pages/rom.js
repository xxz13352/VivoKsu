import { createElement, renderPageState } from "../components.js";

export function createRomPage(context) {
  const element = createElement(context.document, "section", { className: "workspace-page rom-page" });
  let active = false;
  let destroyed = false;
  let generation = 0;
  let controller = null;
  let externalSignal = null;
  let externalAbort = null;
  let currentRoute = { view: "rom" };

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
    controller = { abort: () => owned.abort(), signal: owned.signal };
    externalSignal = signal ?? null;
    externalAbort = () => controller?.abort();
    if (externalSignal?.aborted) controller.abort();
    else externalSignal?.addEventListener("abort", externalAbort, { once: true });
    return { generation: ++generation, signal: owned.signal };
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
    renderPageState(element, { state: "loading", title: "正在加载 ROM 查询" });
    try {
      const response = await context.api.getRomLogs(romQuery(currentRoute), { signal: request.signal });
      if (!isCurrent(request)) return false;
      if (!Array.isArray(response?.items) || (response.next_cursor !== null && typeof response.next_cursor !== "string")) {
        renderPageState(element, { state: "partial", title: "ROM 数据不完整", message: "服务器未返回完整的 ROM 查询结构。" });
        return false;
      }
      render(response?.items ?? [], response?.next_cursor ?? null);
      return true;
    } catch (error) {
      if (!isCurrent(request)) return false;
      renderPageState(element, {
        state: "retry",
        title: "加载失败",
        message: error?.message ?? "ROM 查询暂不可用。",
        onRetry: () => { if (isCurrent(request)) void load(); },
      });
      return false;
    }
  }

  function render(items, nextCursor) {
    const filters = createElement(context.document, "form", { className: "workspace-filters management-form", "data-form": "rom-filters" }, [
      filterInput("用户 ID", "userId"),
      filterInput("PD", "pd"),
      filterInput("版本", "version"),
      filterInput("HTTP 状态", "status"),
      filterInput("搜索 URL/失败原因", "q", "search"),
      createElement(context.document, "button", { type: "submit", className: "button button-primary" }, "应用筛选"),
      createElement(context.document, "button", { type: "reset", className: "button" }, "重置筛选"),
    ]);
    filters.addEventListener("submit", (event) => {
      event.preventDefault();
      const value = (name) => filters.querySelector(`[name="${name}"]`)?.value.trim() ?? "";
      context.navigate?.({
        view: "rom",
        userId: value("userId") || null,
        pd: value("pd") || null,
        version: value("version") || null,
        status: value("status") || null,
        q: value("q") || null,
        cursor: null,
      });
    });
    filters.addEventListener("reset", () => context.window.queueMicrotask(() => context.navigate?.({
      view: "rom", userId: null, pd: null, version: null, status: null, q: null, cursor: null,
    })));
    const list = createElement(context.document, "ul", { className: "rom-list" }, items.length
      ? items.map(romRow)
      : createElement(context.document, "li", { className: "muted" }, "没有匹配的 ROM 查询记录。"));
    const next = createElement(context.document, "button", {
      type: "button",
      className: "button",
      "data-action": "next-page",
      disabled: !nextCursor,
    }, "下一页");
    next.addEventListener("click", () => {
      if (!nextCursor || !active || destroyed) return;
      context.navigate?.({ ...currentRoute, cursor: nextCursor });
    });
    element.replaceChildren(
      createElement(context.document, "div", { className: "workspace-card", "data-page-state": "ready" }, [
        createElement(context.document, "h2", {}, "ROM 查询"),
        createElement(context.document, "p", { className: "muted" }, "URL 与失败原因均来自服务器持久化记录。"),
        filters,
        list,
        next,
      ]),
    );
    element.dataset.pageState = "ready";
  }

  function romRow(row) {
    const children = [
      createElement(context.document, "strong", {}, `${row.pd ?? "—"} · ${row.version ?? "—"}`),
      createElement(context.document, "span", { className: "muted" }, `用户：${row.user_name ?? "—"}（${row.user_id ?? "—"}）`),
      createElement(context.document, "time", { className: "muted" }, `时间：${formatTime(row.created_at_ms)}`),
      createElement(context.document, "span", { className: "muted" }, `状态 ${row.status ?? "—"}`),
    ];
    const downloadUrl = safeHttpUrl(row.url);
    if (downloadUrl) {
      const recordId = String(row.id ?? "未知").slice(0, 64);
      const downloadLabel = `打开记录 ${recordId} ${String(row.pd ?? "未知 PD").slice(0, 64)} ${String(row.version ?? "未知版本").slice(0, 64)} 下载地址`;
      children.push(createElement(context.document, "p", { className: "rom-url" }, [
        createElement(context.document, "a", {
          href: downloadUrl,
          target: "_blank",
          rel: "noopener noreferrer",
          "aria-label": downloadLabel,
        }, "打开下载地址"),
        createElement(context.document, "code", {}, downloadUrl),
      ]));
    }
    if (row.failure_reason) children.push(createElement(context.document, "p", { className: "failure-reason" }, row.failure_reason));
    if (row.detail_unavailable_reason) children.push(createElement(context.document, "p", { className: "muted" }, "旧记录未保存失败原因。"));
    return createElement(context.document, "li", { className: "rom-row" }, children);
  }

  function filterInput(label, name, type = "text") {
    return createElement(context.document, "label", { className: "management-field" }, [
      createElement(context.document, "span", {}, label),
      createElement(context.document, "input", { name, type, value: currentRoute[name] ?? "" }),
    ]);
  }

  function formatTime(value) {
    const milliseconds = Number(value);
    if (!Number.isFinite(milliseconds)) return "—";
    try { return new Date(milliseconds).toISOString(); }
    catch { return String(value); }
  }

  function deactivate() {
    active = false;
    generation += 1;
    stopRequest();
  }

  return {
    element,
    async activate(route, signal) {
      if (destroyed) throw new Error("ROM 页面已销毁。");
      currentRoute = { view: "rom", ...(route ?? {}) };
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

function romQuery(route) {
  const query = {};
  for (const key of ["userId", "pd", "version", "status", "q", "cursor"]) {
    if (route?.[key] !== null && route?.[key] !== undefined) query[key] = route[key];
  }
  return query;
}

function safeHttpUrl(value) {
  if (typeof value !== "string" || !value.trim()) return null;
  try {
    const url = new URL(value.trim());
    if ((url.protocol !== "http:" && url.protocol !== "https:") || url.username || url.password) return null;
    return url.href;
  } catch {
    return null;
  }
}

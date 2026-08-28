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
        list,
        next,
      ]),
    );
    element.dataset.pageState = "ready";
  }

  function romRow(row) {
    const children = [
      createElement(context.document, "strong", {}, `${row.pd ?? "—"} · ${row.version ?? "—"}`),
      createElement(context.document, "span", { className: "muted" }, `状态 ${row.status ?? "—"}`),
    ];
    const downloadUrl = safeHttpUrl(row.url);
    if (downloadUrl) {
      children.push(createElement(context.document, "p", { className: "rom-url" }, [
        createElement(context.document, "a", { href: downloadUrl, target: "_blank", rel: "noopener noreferrer" }, "打开下载地址"),
        createElement(context.document, "code", {}, downloadUrl),
      ]));
    }
    if (row.failure_reason) children.push(createElement(context.document, "p", { className: "failure-reason" }, row.failure_reason));
    if (row.detail_unavailable_reason) children.push(createElement(context.document, "p", { className: "muted" }, "旧记录未保存失败原因。"));
    return createElement(context.document, "li", { className: "rom-row" }, children);
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

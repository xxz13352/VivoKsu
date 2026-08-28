import { createElement, renderPageState } from "../components.js";

export function createVersionsPage(context) {
  const element = createElement(context.document, "section", { className: "workspace-page versions-page" });
  const pendingActions = new Set();
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
    renderPageState(element, { state: "loading", title: "正在加载版本策略" });
    try {
      const [versionsResult, summary] = await Promise.all([
        context.api.getAppVersions({ signal: request.signal }),
        context.api.getVersionSummary({ signal: request.signal }),
      ]);
      if (!isCurrent(request)) return false;
      const versions = versionsResult?.versions ?? [];
      const list = createElement(context.document, "ul", { className: "version-list" }, versions.length
        ? versions.map((version) => versionRow(version))
        : createElement(context.document, "li", { className: "muted" }, "没有已配置的版本策略。"));
      element.replaceChildren(
        createElement(context.document, "div", { className: "workspace-card", "data-page-state": "ready" }, [
          createElement(context.document, "h2", {}, "版本策略"),
          createElement(context.document, "p", { className: "muted" }, `当前版本：${summary?.current_version ?? "未设置"} · 最低版本：${summary?.minimum_version ?? "未设置"}`),
          createElement(context.document, "p", { className: "muted" }, `今日更新拦截：${summary?.today_426 ?? 0}`),
          list,
        ]),
      );
      element.dataset.pageState = "ready";
      return true;
    } catch (error) {
      if (!isCurrent(request)) return false;
      renderPageState(element, {
        state: "retry",
        title: "加载失败",
        message: error?.message ?? "版本策略暂不可用。",
        onRetry: () => { if (isCurrent(request)) void load(); },
      });
      return false;
    }
  }

  function versionRow(version) {
    const key = `delete:${version.id}`;
    const remove = createElement(context.document, "button", {
      type: "button",
      className: "button button-danger",
      "data-action": "delete-version",
      "data-version-id": String(version.id),
      disabled: pendingActions.has(key),
    }, "删除");
    remove.addEventListener("click", () => {
      const request = currentRequest();
      if (!isCurrent(request) || pendingActions.has(key)) return;
      pendingActions.add(key);
      remove.disabled = true;
      const restore = () => {
        if (!isCurrent(request)) return;
        pendingActions.delete(key);
        remove.disabled = false;
      };
      void context.confirm({
        title: "删除版本策略",
        message: `确认删除 ${version.version ?? "此版本"} 吗？`,
        confirmLabel: "删除",
        onCancel: restore,
        onConfirm: async () => {
          if (!isCurrent(request)) return true;
          try {
            await context.api.deleteAppVersion(version.id, { signal: request.signal });
          } catch (error) {
            if (!isCurrent(request)) return true;
            restore();
            throw error;
          }
          if (!isCurrent(request)) return true;
          pendingActions.delete(key);
          context.announce?.("版本策略已删除，正在刷新服务器状态。");
          await load();
          return true;
        },
      });
    });
    return createElement(context.document, "li", { className: "version-row" }, [
      createElement(context.document, "strong", {}, version.version ?? "未命名版本"),
      createElement(context.document, "span", { className: "muted" }, `最低 ${version.min_version ?? "—"}`),
      createElement(context.document, "span", { className: "muted" }, Number(version.enabled) ? "已启用" : "已停用"),
      remove,
    ]);
  }

  function deactivate() {
    active = false;
    generation += 1;
    pendingActions.clear();
    stopRequest();
  }

  return {
    element,
    async activate(_route, signal) {
      if (destroyed) throw new Error("版本策略页面已销毁。");
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

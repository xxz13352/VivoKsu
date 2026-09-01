import { createElement, renderPageState } from "../components.js";

export function createVersionsPage(context) {
  const element = createElement(context.document, "section", { className: "workspace-page versions-page" });
  const pendingActions = new Set();
  const awaitingAuthorityActions = new Set();
  let active = false;
  let destroyed = false;
  let generation = 0;
  let controller = null;
  let externalSignal = null;
  let externalAbort = null;
  let reloadGeneration = 0;
  let reloadController = null;
  let versions = [];
  let summary = {};
  let hasAuthoritativeSnapshot = false;
  let refreshError = null;
  let query = "";
  let status = "all";

  function stopRequest() {
    stopReload();
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

  function currentRequest() { return { generation, signal: controller?.signal ?? null }; }
  function isCurrent(request) {
    return active && !destroyed && request.generation === generation && controller?.signal === request.signal && !request.signal?.aborted;
  }

  function stopReload() {
    reloadGeneration += 1;
    reloadController?.abort();
    reloadController = null;
  }

  function beginReload() {
    const activation = currentRequest();
    if (!isCurrent(activation)) return null;
    reloadController?.abort();
    const owned = new AbortController();
    const abort = () => owned.abort();
    activation.signal?.addEventListener("abort", abort, { once: true });
    reloadController = owned;
    return {
      activation,
      epoch: ++reloadGeneration,
      controller: owned,
      signal: owned.signal,
      dispose: () => activation.signal?.removeEventListener("abort", abort),
    };
  }

  function isCurrentReload(request) {
    return request !== null
      && isCurrent(request.activation)
      && request.epoch === reloadGeneration
      && reloadController === request.controller
      && !request.signal.aborted;
  }

  async function load() {
    const request = beginReload();
    if (!isCurrentReload(request)) return false;
    if (hasAuthoritativeSnapshot) element.setAttribute("aria-busy", "true");
    else renderPageState(element, { state: "loading", title: "正在加载版本策略" });
    try {
      const [versionsResult, nextSummary] = await Promise.all([
        context.api.getAppVersions({ signal: request.signal }),
        context.api.getVersionSummary({ signal: request.signal }),
      ]);
      if (!isCurrentReload(request)) return false;
      if (!Array.isArray(versionsResult?.versions) || !nextSummary || !Array.isArray(nextSummary.supported_versions)) {
        if (hasAuthoritativeSnapshot) {
          refreshError = "服务器未返回完整的版本策略结构。";
          render();
        } else {
          renderPageState(element, { state: "partial", title: "版本数据不完整", message: "服务器未返回完整的版本策略结构。" });
        }
        return false;
      }
      versions = Array.isArray(versionsResult?.versions) ? versionsResult.versions : [];
      summary = nextSummary ?? {};
      hasAuthoritativeSnapshot = true;
      refreshError = null;
      awaitingAuthorityActions.clear();
      render();
      return true;
    } catch (error) {
      if (!isCurrentReload(request)) return false;
      if (hasAuthoritativeSnapshot) {
        refreshError = error?.message ?? "版本策略暂不可用。";
        render();
      } else {
        renderPageState(element, {
          state: "retry", title: "加载失败", message: error?.message ?? "版本策略暂不可用。",
          onRetry: () => { if (isCurrentReload(request)) void load(); },
        });
      }
      return false;
    } finally {
      request?.dispose();
    }
  }

  function render() {
    element.setAttribute("aria-busy", "false");
    const list = createElement(context.document, "ul", { className: "version-list" });
    const refreshList = () => {
      const needle = query.trim().toLocaleLowerCase();
      const filtered = versions.filter((version) => {
        const enabled = Number(version.enabled) !== 0;
        const statusMatch = status === "all" || (status === "enabled" ? enabled : !enabled);
        const text = [version.version, version.min_version, version.download_url, version.note].join(" ").toLocaleLowerCase();
        return statusMatch && (!needle || text.includes(needle));
      });
      list.replaceChildren(...(filtered.length
        ? filtered.map(versionRow)
        : [createElement(context.document, "li", { className: "muted" }, "当前筛选下没有版本策略。")]));
    };
    const search = createElement(context.document, "input", {
      type: "search", value: query, placeholder: "搜索版本、URL 或备注", "data-filter": "version-q", "aria-label": "搜索版本策略",
    });
    const statusFilter = createElement(context.document, "select", { "data-filter": "version-status", "aria-label": "筛选版本状态" }, [
      option("all", "全部状态"), option("enabled", "已启用"), option("disabled", "已停用"),
    ]);
    statusFilter.value = status;
    search.addEventListener("input", () => { query = search.value; refreshList(); });
    statusFilter.addEventListener("change", () => { status = statusFilter.value; refreshList(); });
    const createForm = versionForm({ formName: "create-version", submitLabel: "登记版本" });
    createForm.addEventListener("submit", (event) => {
      event.preventDefault();
      const body = versionBody(createForm, true);
      if (!body.version) return context.alert?.("版本号不能为空。", { title: "无法登记版本" });
      void runDirect("create", createForm.querySelector('[type="submit"]'),
        () => context.api.createAppVersion(body, { signal: currentRequest().signal }), "版本已登记。");
    });
    const staleNotice = refreshError ? createElement(context.document, "div", { className: "workspace-notice", role: "alert" }, [
      createElement(context.document, "strong", {}, "版本数据可能已过期"),
      createElement(context.document, "p", {}, refreshError),
      createElement(context.document, "button", { type: "button", className: "button", "data-action": "retry-versions" }, "重试刷新"),
    ]) : null;
    staleNotice?.querySelector('[data-action="retry-versions"]')?.addEventListener("click", () => void load());
    const pageState = refreshError ? "stale" : "ready";
    element.replaceChildren(createElement(context.document, "div", { className: "workspace-card", "data-page-state": pageState }, [
      createElement(context.document, "h2", {}, "版本策略"),
      staleNotice,
      createElement(context.document, "p", { className: "muted" }, `当前版本：${summary.current_version ?? "未设置"} · 最低版本：${summary.minimum_version ?? "未设置"}`),
      createElement(context.document, "p", { className: "muted" }, `支持版本：${summary.supported_versions.length ? summary.supported_versions.join("、") : "未设置"}`),
      createElement(context.document, "p", { className: "muted" }, `今日更新拦截：${summary.today_426 ?? 0}`),
      createElement(context.document, "div", { className: "workspace-filters" }, [search, statusFilter]),
      createForm, list,
    ]));
    element.dataset.pageState = pageState;
    refreshList();
  }

  function versionRow(version) {
    const deleteKey = `version:${version.id}`;
    const toggleKey = deleteKey;
    const editKey = deleteKey;
    const versionLabel = String(version.version ?? "未命名版本").slice(0, 128);
    const toggle = createElement(context.document, "button", {
      type: "button", className: "button", "data-action": "toggle-version", "aria-label": `${Number(version.enabled) ? "停用" : "启用"}版本 ${versionLabel}`,
      disabled: isActionPending(toggleKey),
    }, Number(version.enabled) ? "停用" : "启用");
    toggle.addEventListener("click", () => void runDirect(toggleKey, toggle,
      () => context.api.updateAppVersion(version.id, { enabled: !Number(version.enabled) }, { signal: currentRequest().signal }), "版本状态已更新。"));
    const edit = createElement(context.document, "button", {
      type: "button", className: "button", "data-action": "edit-version", "aria-expanded": "false", "aria-label": `编辑版本 ${versionLabel}`,
      disabled: isActionPending(editKey),
    }, "编辑");
    const editForm = versionForm({ formName: "edit-version", submitLabel: "保存", version, includeVersion: false });
    editForm.hidden = true;
    edit.addEventListener("click", () => {
      editForm.hidden = !editForm.hidden;
      edit.setAttribute("aria-expanded", String(!editForm.hidden));
      if (!editForm.hidden) editForm.querySelector("input")?.focus();
    });
    editForm.addEventListener("submit", (event) => {
      event.preventDefault();
      void runDirect(editKey, editForm.querySelector('[type="submit"]'),
        () => context.api.updateAppVersion(version.id, versionBody(editForm, false), { signal: currentRequest().signal }), "版本详情已更新。");
    });
    const remove = createElement(context.document, "button", {
      type: "button", className: "button button-danger", "data-action": "delete-version", "data-version-id": String(version.id),
      "aria-label": `删除版本 ${versionLabel}`, disabled: isActionPending(deleteKey),
    }, "删除");
    remove.addEventListener("click", () => {
      const request = currentRequest();
      if (!isCurrent(request) || isActionPending(deleteKey)) return;
      pendingActions.add(deleteKey);
      remove.disabled = true;
      const restore = () => { if (isCurrent(request)) { pendingActions.delete(deleteKey); remove.disabled = false; } };
      void context.confirm({
        trigger: remove, title: "删除版本策略", message: `确认删除 ${version.version ?? "此版本"} 吗？删除会改变客户端准入策略与受支持版本范围。`, confirmLabel: "删除", onCancel: restore,
        onConfirm: async () => {
          if (!isCurrent(request)) return true;
          try { await context.api.deleteAppVersion(version.id, { signal: request.signal }); }
          catch (error) { if (isCurrent(request)) restore(); throw error; }
          if (!isCurrent(request)) return true;
          pendingActions.delete(deleteKey);
          awaitingAuthorityActions.add(deleteKey);
          context.announce?.("版本策略已删除，正在刷新服务器状态。");
          await load();
          return true;
        },
      });
    });
    return createElement(context.document, "li", { className: "version-row" }, [
      createElement(context.document, "strong", {}, versionLabel),
      createElement(context.document, "span", { className: "muted" }, `最低 ${version.min_version ?? "—"}`),
      createElement(context.document, "span", { className: "muted" }, version.download_url ?? "无下载地址"),
      createElement(context.document, "span", { className: "muted" }, version.note ?? ""),
      createElement(context.document, "span", { className: "muted" }, Number(version.enabled) ? "已启用" : "已停用"),
      createElement(context.document, "div", { className: "row-actions" }, [toggle, edit, remove]), editForm,
    ]);
  }

  async function runDirect(key, button, operation, successMessage) {
    const request = currentRequest();
    if (!isCurrent(request) || isActionPending(key)) return false;
    pendingActions.add(key);
    if (button) button.disabled = true;
    try {
      await operation();
      if (!isCurrent(request)) return false;
      pendingActions.delete(key);
      awaitingAuthorityActions.add(key);
      context.announce?.(successMessage);
      await load();
      return true;
    } catch (error) {
      if (isCurrent(request)) {
        pendingActions.delete(key);
        if (button?.isConnected) button.disabled = false;
        context.alert?.(error?.message ?? "版本操作失败。", { title: "版本操作失败" });
      }
      return false;
    }
  }

  function isActionPending(key) {
    return pendingActions.has(key) || awaitingAuthorityActions.has(key);
  }

  function versionForm({ formName, submitLabel, version = {}, includeVersion = true }) {
    return createElement(context.document, "form", { className: "management-form", "data-form": formName }, [
      includeVersion ? labelledInput("版本号", "version", version.version ?? "", true) : null,
      labelledInput("最低版本", "min_version", version.min_version ?? ""),
      labelledInput("下载地址", "download_url", version.download_url ?? "", false, "url"),
      labelledInput("备注", "note", version.note ?? ""),
      createElement(context.document, "button", { type: "submit", className: "button button-primary" }, submitLabel),
    ]);
  }

  function labelledInput(label, name, value, required = false, type = "text") {
    return createElement(context.document, "label", { className: "management-field" }, [
      createElement(context.document, "span", {}, label),
      createElement(context.document, "input", { name, type, value: String(value ?? ""), required }),
    ]);
  }
  function option(value, label) { return createElement(context.document, "option", { value }, label); }
  function versionBody(form, includeVersion) {
    const value = (name) => form.querySelector(`[name="${name}"]`)?.value.trim() ?? "";
    return { ...(includeVersion ? { version: value("version") } : {}), min_version: value("min_version"), download_url: value("download_url"), note: value("note") };
  }

  function deactivate() {
    active = false;
    generation += 1;
    pendingActions.clear();
    awaitingAuthorityActions.clear();
    refreshError = null;
    hasAuthoritativeSnapshot = false;
    stopRequest();
  }
  return {
    element,
    async activate(_route, signal) { if (destroyed) throw new Error("版本策略页面已销毁。"); beginActivation(signal); await load(); },
    deactivate,
    destroy() { if (destroyed) return; destroyed = true; deactivate(); element.replaceChildren(); },
  };
}

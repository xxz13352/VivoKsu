import { createElement, renderPageState } from "../components.js";

export function createUsersPage(context) {
  const element = createElement(context.document, "section", { className: "workspace-page users-page" });
  const pendingUsers = new Set();
  const awaitingAuthorityUsers = new Set();
  let active = false;
  let destroyed = false;
  let generation = 0;
  let controller = null;
  let externalSignal = null;
  let externalAbort = null;
  let reloadGeneration = 0;
  let reloadController = null;
  let users = [];
  let hasAuthoritativeSnapshot = false;
  let refreshError = null;
  let query = "";
  let status = "all";
  let oneTimeToken = null;
  let tokenGeneration = null;
  let tokenUserId = null;
  let tokenAwaitingReload = false;

  function stopRequest() {
    stopReload();
    controller?.abort(); controller = null;
    externalSignal?.removeEventListener("abort", externalAbort);
    externalSignal = null; externalAbort = null;
  }
  function beginActivation(signal) {
    stopRequest(); active = true;
    const owned = new AbortController();
    controller = { abort: () => owned.abort(), signal: owned.signal };
    externalSignal = signal ?? null; externalAbort = () => controller?.abort();
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
    else renderPageState(element, { state: "loading", title: "正在加载用户" });
    try {
      const response = await context.api.getUsers({ signal: request.signal });
      if (!isCurrentReload(request)) return false;
      if (!Array.isArray(response?.users)) {
        if (hasAuthoritativeSnapshot) {
          refreshError = "服务器未返回完整的用户列表结构。";
          render();
        } else {
          renderPageState(element, { state: "partial", title: "用户数据不完整", message: "服务器未返回完整的用户列表结构。" });
        }
        return false;
      }
      users = Array.isArray(response?.users) ? response.users : [];
      hasAuthoritativeSnapshot = true;
      refreshError = null;
      awaitingAuthorityUsers.clear();
      tokenAwaitingReload = false;
      render();
      return true;
    } catch (error) {
      if (!isCurrentReload(request)) return false;
      if (hasAuthoritativeSnapshot) {
        refreshError = error?.message ?? "用户数据暂不可用。";
        render();
      } else {
        renderPageState(element, {
          state: "retry", title: "加载失败", message: error?.message ?? "用户数据暂不可用。",
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
    const tokenOwnerPresent = tokenUserId !== null && users.some((user) => String(user.id) === tokenUserId);
    if (!tokenOwnerPresent && !tokenAwaitingReload) clearOneTimeToken();
    const token = tokenGeneration === generation && (tokenOwnerPresent || tokenAwaitingReload) ? oneTimeToken : null;
    const tokenHost = createElement(context.document, "div", { className: "one-time-token", hidden: !token }, token ? [
      createElement(context.document, "code", {}, `一次性令牌：${token}`),
      createElement(context.document, "button", { type: "button", className: "button", "data-action": "copy-token" }, "复制令牌"),
    ] : "");
    tokenHost.querySelector('[data-action="copy-token"]')?.addEventListener("click", async () => {
      try {
        if (!context.window.navigator.clipboard?.writeText) throw new Error("浏览器未提供剪贴板权限。");
        await context.window.navigator.clipboard.writeText(token);
        context.announce?.("一次性令牌已复制。");
      } catch (error) {
        context.alert?.(error?.message ?? "复制令牌失败。", { title: "复制失败" });
      }
    });
    const list = createElement(context.document, "ul", { className: "user-list" });
    const refreshList = () => {
      const needle = query.trim().toLocaleLowerCase();
      const filtered = users.filter((user) => {
        const enabled = Number(user.enabled) !== 0;
        const banned = Number(user.banned) !== 0;
        const statusMatch = status === "all"
          || (status === "enabled" && enabled && !banned)
          || (status === "disabled" && !enabled && !banned)
          || (status === "banned" && banned);
        const text = [user.username, user.name, user.note].join(" ").toLocaleLowerCase();
        return statusMatch && (!needle || text.includes(needle));
      });
      list.replaceChildren(...(filtered.length
        ? filtered.map(userRow)
        : [createElement(context.document, "li", { className: "muted" }, "当前筛选下没有 API 用户。")]));
    };
    const search = createElement(context.document, "input", {
      type: "search", value: query, placeholder: "搜索账号、名称或备注", "data-filter": "user-q", "aria-label": "搜索 API 用户",
    });
    const statusFilter = createElement(context.document, "select", { "data-filter": "user-status", "aria-label": "筛选用户状态" }, [
      option("all", "全部状态"), option("enabled", "已启用"), option("disabled", "已停用"), option("banned", "已封禁"),
    ]);
    statusFilter.value = status;
    search.addEventListener("input", () => { query = search.value; refreshList(); });
    statusFilter.addEventListener("change", () => { status = statusFilter.value; refreshList(); });
    const createForm = createElement(context.document, "form", { className: "management-form", "data-form": "create-user" }, [
      labelledInput("账号", "username", true), labelledInput("名称", "name"), labelledInput("初始密码", "password", true, "password"),
      labelledInput("备注", "note"), createElement(context.document, "button", {
        type: "submit", className: "button button-primary", disabled: isUserPending("create"),
      }, "创建用户"),
    ]);
    createForm.addEventListener("submit", (event) => {
      event.preventDefault();
      const value = (name) => createForm.querySelector(`[name="${name}"]`)?.value.trim() ?? "";
      const body = { username: value("username"), name: value("name"), password: value("password"), note: value("note") };
      if (!body.username || body.password.length < 6) return context.alert?.("账号不能为空，初始密码至少 6 位。", { title: "无法创建用户" });
      void createUser(body, createForm.querySelector('[type="submit"]'));
    });
    const staleNotice = refreshError ? createElement(context.document, "div", { className: "workspace-notice", role: "alert" }, [
      createElement(context.document, "strong", {}, "用户数据可能已过期"),
      createElement(context.document, "p", {}, refreshError),
      createElement(context.document, "button", { type: "button", className: "button", "data-action": "retry-users" }, "重试刷新"),
    ]) : null;
    staleNotice?.querySelector('[data-action="retry-users"]')?.addEventListener("click", () => void load());
    const pageState = refreshError ? "stale" : "ready";
    element.replaceChildren(createElement(context.document, "div", { className: "workspace-card", "data-page-state": pageState }, [
      createElement(context.document, "h2", {}, "用户管理"),
      createElement(context.document, "p", { className: "muted" }, "令牌仅在当前页面短暂显示，切换页面后会清除。"),
      staleNotice, tokenHost, createElement(context.document, "div", { className: "workspace-filters" }, [search, statusFilter]), createForm, list,
    ]));
    element.dataset.pageState = pageState;
    refreshList();
  }

  async function createUser(body, button) {
    const request = currentRequest();
    const key = "create";
    if (!isCurrent(request) || isUserPending(key)) return;
    pendingUsers.add(key); button.disabled = true;
    try {
      const result = await context.api.createUser(body, { signal: request.signal });
      if (!isCurrent(request)) return;
      oneTimeToken = typeof result?.token === "string" && result.token ? result.token : null;
      tokenGeneration = request.generation;
      tokenUserId = result?.id !== undefined ? String(result.id) : null;
      tokenAwaitingReload = Boolean(oneTimeToken && tokenUserId !== null);
      pendingUsers.delete(key);
      awaitingAuthorityUsers.add(key);
      render();
      context.announce?.("用户已创建；一次性令牌仅显示一次。");
      await load();
    } catch (error) {
      if (isCurrent(request)) { pendingUsers.delete(key); button.disabled = false; context.alert?.(error?.message ?? "创建用户失败。", { title: "创建失败" }); }
    }
  }

  function userRow(user) {
    const userKey = String(user.id);
    const userPending = isUserPending(userKey);
    const rotate = confirmedAction({ user, action: "rotate-token", label: "轮换令牌" }, async (request) => {
      const result = await context.api.rotateUserToken(user.id, { signal: request.signal });
      if (!isCurrent(request)) return;
      oneTimeToken = typeof result?.token === "string" && result.token ? result.token : null;
      tokenGeneration = request.generation; tokenUserId = oneTimeToken ? userKey : null;
      tokenAwaitingReload = Boolean(oneTimeToken && tokenUserId !== null);
      context.announce?.("令牌已轮换，请立即保存。");
    });
    const ban = confirmedAction({
      user, action: "toggle-ban", label: Number(user.banned) ? "解除封禁" : "封禁", clearToken: true,
      title: Number(user.banned) ? "解除封禁" : "封禁用户", message: `确认修改 ${user.username ?? "该用户"} 的封禁状态吗？`,
    }, (request) => context.api.updateUser(user.id, { banned: !Number(user.banned) }, { signal: request.signal }));
    const remove = confirmedAction({
      user, action: "delete-user", label: "删除", clearToken: true, title: "删除用户",
      message: `确认删除 ${user.username ?? "该用户"} 吗？`, confirmLabel: "删除",
    }, (request) => context.api.deleteUser(user.id, { signal: request.signal }));
    const password = createElement(context.document, "input", {
      type: "password", minlength: "6", autocomplete: "new-password", "data-user-password": userKey, "aria-label": `为 ${user.username ?? "用户"} 设置新密码`,
      disabled: userPending,
    });
    const targetLabel = String(user.username ?? user.name ?? `用户 ${user.id ?? "未知"}`).slice(0, 128);
    const reset = createElement(context.document, "button", {
      type: "button", className: "button", "data-action": "reset-password", "aria-label": `重置用户 ${targetLabel} 的密码`,
      disabled: userPending,
    }, "重置密码");
    reset.addEventListener("click", () => {
      if (password.value.length < 6) return context.alert?.("新密码至少 6 位。", { title: "无法重置密码" });
      void directUserAction(userKey, reset,
        () => context.api.updateUser(user.id, { newPassword: password.value }, { signal: currentRequest().signal }), "密码已重置。");
    });
    const toggle = createElement(context.document, "button", {
      type: "button", className: "button", "data-action": "toggle-user-enabled",
      "aria-label": `${Number(user.enabled) ? "停用" : "启用"}用户 ${targetLabel}`,
      disabled: userPending,
    }, Number(user.enabled) ? "停用" : "启用");
    toggle.addEventListener("click", () => void directUserAction(userKey, toggle,
      () => context.api.updateUser(user.id, { enabled: !Number(user.enabled) }, { signal: currentRequest().signal }), "用户启用状态已更新。"));
    return createElement(context.document, "li", { className: "user-row" }, [
      createElement(context.document, "strong", {}, user.username ?? "未知用户"),
      createElement(context.document, "span", { className: "muted" }, user.name ?? ""),
      createElement(context.document, "span", { className: "muted" }, user.note ?? ""),
      createElement(context.document, "time", { className: "muted" }, user.created_at ?? ""),
      createElement(context.document, "span", { className: "muted" }, Number(user.banned) ? "已封禁" : Number(user.enabled) ? "已启用" : "已停用"),
      createElement(context.document, "div", { className: "row-actions" }, [rotate, ban, toggle, remove]),
      createElement(context.document, "div", { className: "inline-reset" }, [password, reset]),
    ]);
  }

  function confirmedAction({ user, action, label, clearToken = false, title = "轮换令牌", message = `确认轮换 ${user.username ?? "该用户"} 的令牌吗？`, confirmLabel = label }, mutation) {
    const userKey = String(user.id);
    const targetLabel = String(user.username ?? user.name ?? `用户 ${user.id ?? "未知"}`).slice(0, 128);
    const button = createElement(context.document, "button", {
      type: "button", className: action === "delete-user" ? "button button-danger" : "button", "data-action": action,
      "aria-label": `${label}用户 ${targetLabel}`, disabled: isUserPending(userKey),
    }, label);
    button.addEventListener("click", () => {
      const request = currentRequest();
      if (!isCurrent(request) || isUserPending(userKey)) return;
      pendingUsers.add(userKey); setUserActionsDisabled(button, true);
      const restore = () => { if (isCurrent(request)) { pendingUsers.delete(userKey); setUserActionsDisabled(button, false); } };
      void context.confirm({
        trigger: button, title, message, confirmLabel, onCancel: restore,
        onConfirm: async () => {
          if (!isCurrent(request)) return true;
          try { await mutation(request); }
          catch (error) { if (isCurrent(request)) restore(); throw error; }
          if (!isCurrent(request)) return true;
          if (clearToken && tokenUserId === userKey) clearOneTimeToken();
          pendingUsers.delete(userKey);
          awaitingAuthorityUsers.add(userKey);
          render();
          await load();
          return true;
        },
      });
    });
    return button;
  }

  async function directUserAction(userKey, button, operation, message) {
    const request = currentRequest();
    if (!isCurrent(request) || isUserPending(userKey)) return;
    pendingUsers.add(userKey); setUserActionsDisabled(button, true);
    try {
      await operation();
      if (isCurrent(request)) {
        context.announce?.(message);
        pendingUsers.delete(userKey);
        awaitingAuthorityUsers.add(userKey);
        render();
        await load();
      }
    }
    catch (error) { if (isCurrent(request)) { pendingUsers.delete(userKey); setUserActionsDisabled(button, false); context.alert?.(error?.message ?? "用户操作失败。", { title: "用户操作失败" }); } }
  }

  function setUserActionsDisabled(button, disabled) {
    button.closest(".user-row")?.querySelectorAll("button, input").forEach((control) => { control.disabled = disabled; });
  }
  function isUserPending(key) {
    return pendingUsers.has(key) || awaitingAuthorityUsers.has(key);
  }
  function clearOneTimeToken() {
    oneTimeToken = null;
    tokenGeneration = null;
    tokenUserId = null;
    tokenAwaitingReload = false;
  }
  function labelledInput(label, name, required = false, type = "text") {
    return createElement(context.document, "label", { className: "management-field" }, [
      createElement(context.document, "span", {}, label), createElement(context.document, "input", { name, type, required }),
    ]);
  }
  function option(value, label) { return createElement(context.document, "option", { value }, label); }
  function deactivate() {
    active = false;
    generation += 1;
    clearOneTimeToken();
    pendingUsers.clear();
    awaitingAuthorityUsers.clear();
    refreshError = null;
    hasAuthoritativeSnapshot = false;
    stopRequest();
    element.replaceChildren();
  }
  return {
    element,
    async activate(_route, signal) { if (destroyed) throw new Error("用户页面已销毁。"); beginActivation(signal); await load(); },
    deactivate,
    destroy() { if (destroyed) return; destroyed = true; deactivate(); },
  };
}

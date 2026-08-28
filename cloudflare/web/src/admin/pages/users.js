import { createElement, renderPageState } from "../components.js";

export function createUsersPage(context) {
  const element = createElement(context.document, "section", { className: "workspace-page users-page" });
  const pendingUsers = new Set();
  let active = false;
  let destroyed = false;
  let generation = 0;
  let controller = null;
  let externalSignal = null;
  let externalAbort = null;
  let oneTimeToken = null;
  let tokenGeneration = null;
  let tokenUserId = null;

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
    renderPageState(element, { state: "loading", title: "正在加载用户" });
    try {
      const response = await context.api.getUsers({ signal: request.signal });
      if (!isCurrent(request)) return false;
      render(response?.users ?? []);
      return true;
    } catch (error) {
      if (!isCurrent(request)) return false;
      renderPageState(element, {
        state: "retry",
        title: "加载失败",
        message: error?.message ?? "用户数据暂不可用。",
        onRetry: () => { if (isCurrent(request)) void load(); },
      });
      return false;
    }
  }

  function render(users) {
    const tokenOwnerPresent = tokenUserId !== null && users.some((user) => String(user.id) === tokenUserId);
    if (!tokenOwnerPresent) clearOneTimeToken();
    const token = tokenGeneration === generation && tokenOwnerPresent ? oneTimeToken : null;
    const tokenHost = createElement(context.document, "div", { className: "one-time-token", hidden: !token }, token ? `一次性令牌：${token}` : "");
    const list = createElement(context.document, "ul", { className: "user-list" }, users.length
      ? users.map((user) => userRow(user))
      : createElement(context.document, "li", { className: "muted" }, "没有 API 用户。"));
    element.replaceChildren(
      createElement(context.document, "div", { className: "workspace-card", "data-page-state": "ready" }, [
        createElement(context.document, "h2", {}, "用户管理"),
        createElement(context.document, "p", { className: "muted" }, "令牌仅在当前页面短暂显示，切换页面后会清除。"),
        tokenHost,
        list,
      ]),
    );
    element.dataset.pageState = "ready";
  }

  function userRow(user) {
    const rotate = actionButton({ user, action: "rotate-token", label: "轮换令牌" }, async (request) => {
      const result = await context.api.rotateUserToken(user.id, { signal: request.signal });
      if (!isCurrent(request)) return;
      oneTimeToken = typeof result?.token === "string" && result.token ? result.token : null;
      tokenGeneration = request.generation;
      tokenUserId = oneTimeToken ? String(user.id) : null;
      context.announce?.("令牌已轮换，请立即保存。");
    });
    const ban = actionButton({
      user,
      action: "toggle-ban",
      label: Number(user.banned) ? "解除封禁" : "封禁",
      clearToken: true,
      title: Number(user.banned) ? "解除封禁" : "封禁用户",
      message: `确认修改 ${user.username ?? "该用户"} 的状态吗？`,
    }, (request) => context.api.updateUser(user.id, { banned: !Number(user.banned) }, { signal: request.signal }));
    const remove = actionButton({
      user,
      action: "delete-user",
      label: "删除",
      clearToken: true,
      title: "删除用户",
      message: `确认删除 ${user.username ?? "该用户"} 吗？`,
      confirmLabel: "删除",
    }, (request) => context.api.deleteUser(user.id, { signal: request.signal }));
    return createElement(context.document, "li", { className: "user-row" }, [
      createElement(context.document, "strong", {}, user.username ?? "未知用户"),
      createElement(context.document, "span", { className: "muted" }, user.name ?? ""),
      createElement(context.document, "span", { className: "muted" }, Number(user.banned) ? "已封禁" : "正常"),
      rotate,
      ban,
      remove,
    ]);
  }

  function actionButton({ user, action, label, clearToken = false, title = "轮换令牌", message = `确认轮换 ${user.username ?? "该用户"} 的令牌吗？`, confirmLabel = label }, mutation) {
    const button = createElement(context.document, "button", {
      type: "button",
      className: action === "delete-user" ? "button button-danger" : "button",
      "data-action": action,
      disabled: pendingUsers.has(String(user.id)),
    }, label);
    button.addEventListener("click", () => {
      const request = currentRequest();
      const userKey = String(user.id);
      if (!isCurrent(request) || pendingUsers.has(userKey)) return;
      pendingUsers.add(userKey);
      setUserActionsDisabled(button, true);
      const restore = () => {
        if (!isCurrent(request)) return;
        pendingUsers.delete(userKey);
        setUserActionsDisabled(button, false);
      };
      void context.confirm({
        title,
        message,
        confirmLabel,
        onCancel: restore,
        onConfirm: async () => {
          if (!isCurrent(request)) return true;
          try {
            await mutation(request);
          } catch (error) {
            if (!isCurrent(request)) return true;
            restore();
            throw error;
          }
          if (!isCurrent(request)) return true;
          if (clearToken && tokenUserId === userKey) clearOneTimeToken();
          pendingUsers.delete(userKey);
          await load();
          return true;
        },
      });
    });
    return button;
  }

  function setUserActionsDisabled(button, disabled) {
    button.closest(".user-row")?.querySelectorAll('button[data-action]').forEach((control) => {
      control.disabled = disabled;
    });
  }

  function clearOneTimeToken() {
    oneTimeToken = null;
    tokenGeneration = null;
    tokenUserId = null;
  }

  function deactivate() {
    active = false;
    generation += 1;
    clearOneTimeToken();
    pendingUsers.clear();
    stopRequest();
    element.replaceChildren();
  }

  return {
    element,
    async activate(_route, signal) {
      if (destroyed) throw new Error("用户页面已销毁。");
      beginActivation(signal);
      await load();
    },
    deactivate,
    destroy() {
      if (destroyed) return;
      destroyed = true;
      deactivate();
    },
  };
}

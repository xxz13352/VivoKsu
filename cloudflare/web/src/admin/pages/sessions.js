import { createElement, renderPageState } from "../components.js";

const POLL_MS = 10_000;

export function createSessionsPage(context) {
  const element = createElement(context.document, "section", { className: "workspace-page sessions-page" });
  const pendingActions = new Set();
  let active = false;
  let destroyed = false;
  let generation = 0;
  let controller = null;
  let externalSignal = null;
  let externalAbort = null;
  let inFlight = null;
  let timer = null;
  let visibilityListener = null;
  let resumePolling = false;
  let sessions = [];
  const pendingKickIds = new Set();
  let hasAuthoritativeSnapshot = false;
  let refreshError = null;

  function stopRequest() {
    controller?.abort();
    controller = null;
    externalSignal?.removeEventListener("abort", externalAbort);
    externalSignal = null;
    externalAbort = null;
    inFlight = null;
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

  function isVisible() {
    return context.document.visibilityState !== "hidden";
  }

  function canPoll(request) {
    return isCurrent(request) && isVisible() && inFlight === null;
  }

  async function load() {
    const request = currentRequest();
    if (!isCurrent(request) || inFlight) return false;
    inFlight = request;
    if (!element.childElementCount) renderPageState(element, { state: "loading", title: "正在加载在线会话" });
    try {
      const response = await context.api.getOnlineSessions({ signal: request.signal });
      if (!isCurrent(request)) return false;
      if (!Array.isArray(response?.sessions)) {
        renderPageState(element, { state: "partial", title: "会话数据不完整", message: "服务器未返回完整的在线会话结构。" });
        return false;
      }
      sessions = response?.sessions ?? [];
      hasAuthoritativeSnapshot = true;
      refreshError = null;
      let removedPending = 0;
      for (const sessionId of pendingKickIds) {
        if (!sessions.some((session) => session.session_id === sessionId)) {
          pendingKickIds.delete(sessionId);
          removedPending += 1;
        }
      }
      if (removedPending > 0) {
        context.announce?.(removedPending === 1 ? "服务器已确认会话移除。" : `服务器已确认 ${removedPending} 个会话移除。`);
      }
      render();
      return true;
    } catch (error) {
      if (!isCurrent(request)) return false;
      if (hasAuthoritativeSnapshot) {
        const nextError = error?.message ?? "在线会话刷新失败。";
        if (refreshError !== nextError) {
          refreshError = nextError;
          render();
        }
        return false;
      }
      renderPageState(element, {
        state: "retry",
        title: "加载失败",
        message: error?.message ?? "在线会话暂不可用。",
        onRetry: () => { if (isCurrent(request)) void load(); },
      });
      return false;
    } finally {
      if (inFlight === request) {
        inFlight = null;
        if (resumePolling && isCurrent(request) && isVisible()) {
          resumePolling = false;
          startPolling(request);
        }
      }
    }
  }

  function render() {
    const pending = pendingKickIds.size > 0 ? createElement(
      context.document,
      "p",
      { className: "pending-kick" },
      pendingKickIds.size === 1 ? "正在等待服务器确认会话移除。" : `正在等待服务器确认 ${pendingKickIds.size} 个会话移除。`,
    ) : null;
    const refreshFailure = refreshError ? refreshFailureNotice(refreshError) : null;
    const list = createElement(context.document, "ul", { className: "session-list" }, sessions.length
      ? sessions.map((session) => sessionRow(session))
      : createElement(context.document, "li", { className: "muted" }, "当前没有在线会话。"));
    element.replaceChildren(
      createElement(context.document, "div", { className: "workspace-card", "data-page-state": "ready" }, [
        createElement(context.document, "h2", {}, "在线会话"),
        createElement(context.document, "p", { className: "muted" }, "仅在当前页面可见且空闲时轮询；强制下线以服务器返回状态为准。"),
        pending,
        refreshFailure,
        list,
      ]),
    );
    element.dataset.pageState = "ready";
  }

  function refreshFailureNotice(message) {
    const retry = createElement(context.document, "button", {
      type: "button",
      className: "button",
      "data-action": "retry-sessions",
    }, "重试刷新");
    retry.addEventListener("click", () => {
      const request = currentRequest();
      if (isCurrent(request)) void load();
    });
    return createElement(context.document, "div", { className: "session-refresh-error", role: "alert" }, [
      createElement(context.document, "span", {}, message),
      retry,
    ]);
  }

  function sessionRow(session) {
    const key = `kick:${session.session_id}`;
    const sessionName = String(session.username ?? session.name ?? "未知用户").slice(0, 96);
    const sessionId = String(session.session_id ?? "未知会话").slice(0, 128);
    const reason = createElement(context.document, "input", {
      type: "text",
      maxlength: "200",
      placeholder: "下线原因（可选，最多 200 字）",
      "aria-label": `下线 ${sessionName} 的原因`,
      "data-kick-reason": String(session.session_id),
    });
    const kick = createElement(context.document, "button", {
      type: "button",
      className: "button button-danger",
      "data-action": "kick-session",
      "aria-label": `强制下线 ${sessionName} 会话 ${sessionId}`,
      disabled: pendingKickIds.has(session.session_id) || pendingActions.has(key),
    }, pendingKickIds.has(session.session_id) ? "等待确认" : "强制下线");
    kick.addEventListener("click", () => {
      const request = currentRequest();
      if (!isCurrent(request) || pendingActions.has(key) || pendingKickIds.has(session.session_id)) return;
      pendingActions.add(key);
      kick.disabled = true;
      const restore = () => {
        if (!isCurrent(request)) return;
        pendingActions.delete(key);
        kick.disabled = false;
      };
      void context.confirm({
        trigger: kick,
        title: "强制下线",
        message: `确认下线 ${session.username ?? session.name ?? "该会话"} 吗？原因：${reason.value.trim() || "未填写"}`,
        confirmLabel: "下线",
        onCancel: restore,
        onConfirm: async () => {
          if (!isCurrent(request)) return true;
          try {
            await context.api.kickSession({ sessionId: session.session_id, reason: reason.value.slice(0, 200).trim() }, { signal: request.signal });
          } catch (error) {
            if (!isCurrent(request)) return true;
            restore();
            throw error;
          }
          if (!isCurrent(request)) return true;
          pendingActions.delete(key);
          pendingKickIds.add(session.session_id);
          render();
          context.announce?.("已发送下线请求，正在刷新服务器状态。");
          await load();
          return true;
        },
      });
    });
    return createElement(context.document, "li", { className: "session-row" }, [
      createElement(context.document, "strong", {}, session.username ?? session.name ?? "未知会话"),
      createElement(context.document, "code", {}, `会话：${session.session_id ?? "—"}`),
      createElement(context.document, "span", { className: "muted" }, `客户端：${session.client_version ?? "—"}`),
      createElement(context.document, "span", { className: "muted" }, `IP：${session.ip ?? "—"}`),
      createElement(context.document, "time", { className: "muted" }, `上线：${formatSessionTime(session.connected_at)}`),
      createElement(context.document, "time", { className: "muted" }, `最后心跳：${formatSessionTime(session.last_seen_at)}`),
      createElement(context.document, "span", { className: "muted" }, `在线时长：${formatDuration(session.duration_seconds)}`),
      reason,
      kick,
    ]);
  }

  function formatSessionTime(value) {
    const seconds = Number(value);
    if (!Number.isFinite(seconds)) return "—";
    try { return new Date(seconds * 1_000).toISOString(); }
    catch { return String(value); }
  }

  function formatDuration(value) {
    const seconds = Number(value);
    return Number.isFinite(seconds) && seconds >= 0 ? `${Math.floor(seconds)} 秒` : "—";
  }

  function stopPolling() {
    if (timer !== null) globalThis.clearInterval(timer);
    timer = null;
  }

  function startPolling(request = currentRequest()) {
    stopPolling();
    if (!canPoll(request)) return;
    timer = globalThis.setInterval(() => {
      const pollRequest = currentRequest();
      if (!canPoll(pollRequest)) return;
      void load();
    }, POLL_MS);
  }

  function attachVisibility(request) {
    visibilityListener = () => {
      if (!isCurrent(request)) return;
      if (!isVisible()) {
        resumePolling = false;
        stopPolling();
        return;
      }
      if (inFlight) {
        resumePolling = true;
        return;
      }
      startPolling(request);
      void load();
    };
    context.document.addEventListener("visibilitychange", visibilityListener);
  }

  function detachVisibility() {
    if (visibilityListener) context.document.removeEventListener("visibilitychange", visibilityListener);
    visibilityListener = null;
  }

  function deactivate() {
    active = false;
    generation += 1;
    pendingActions.clear();
    resumePolling = false;
    pendingKickIds.clear();
    sessions = [];
    hasAuthoritativeSnapshot = false;
    refreshError = null;
    stopPolling();
    detachVisibility();
    stopRequest();
  }

  return {
    element,
    async activate(_route, signal) {
      if (destroyed) throw new Error("在线会话页面已销毁。");
      const request = beginActivation(signal);
      attachVisibility(request);
      await load();
      if (canPoll(request)) startPolling(request);
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

const SAFE_TAGS = new Set([
  "a",
  "button",
  "code",
  "div",
  "fieldset",
  "form",
  "h1",
  "h2",
  "h3",
  "header",
  "input",
  "label",
  "li",
  "main",
  "nav",
  "ol",
  "option",
  "p",
  "pre",
  "section",
  "select",
  "small",
  "span",
  "strong",
  "table",
  "tbody",
  "td",
  "textarea",
  "th",
  "thead",
  "tr",
  "ul",
]);

const SAFE_ATTRIBUTES = new Set([
  "autocomplete",
  "checked",
  "class",
  "className",
  "colspan",
  "disabled",
  "download",
  "for",
  "hidden",
  "href",
  "id",
  "max",
  "maxlength",
  "min",
  "minlength",
  "name",
  "placeholder",
  "readonly",
  "rel",
  "required",
  "role",
  "rowspan",
  "scope",
  "selected",
  "step",
  "tabIndex",
  "tabindex",
  "target",
  "title",
  "type",
  "value",
]);

const BOOLEAN_ATTRIBUTES = new Set(["checked", "disabled", "hidden", "readonly", "required", "selected"]);
const URL_ATTRIBUTES = new Set(["href"]);
const SAFE_URL_PROTOCOLS = new Set(["http:", "https:", "mailto:", "tel:"]);
const URL_CONTROL_CHARACTERS = /[\u0000-\u001f\u007f]/;

export const PAGE_STATES = Object.freeze([
  "loading",
  "empty",
  "partial",
  "stale",
  "unauthorized",
  "error",
  "retry",
]);

export const ADMIN_MENU_ITEMS = Object.freeze([
  Object.freeze({ id: "overview", label: "概览" }),
  Object.freeze({ id: "versions", label: "版本策略" }),
  Object.freeze({ id: "users", label: "用户管理" }),
  Object.freeze({ id: "sessions", label: "在线会话" }),
  Object.freeze({ id: "audit", label: "操作审计" }),
  Object.freeze({ id: "rom", label: "ROM 查询" }),
]);

export function isCurrentPageActivation(currentPage, currentController, page, controller) {
  return currentPage === page
    && currentController === controller
    && controller instanceof AbortController
    && !controller.signal.aborted;
}

const PAGE_STATE_DEFAULTS = Object.freeze({
  loading: Object.freeze({ title: "正在加载", message: "正在获取服务器状态。" }),
  empty: Object.freeze({ title: "暂无数据", message: "当前条件下没有可显示的记录。" }),
  partial: Object.freeze({ title: "数据不完整", message: "部分服务器数据尚未上传完成。" }),
  stale: Object.freeze({ title: "数据可能已过期", message: "显示的是上一次成功获取的结果。" }),
  unauthorized: Object.freeze({ title: "会话已失效", message: "请重新登录管理后台。" }),
  error: Object.freeze({ title: "无法加载", message: "服务器未能返回数据。" }),
  retry: Object.freeze({ title: "请重试", message: "请求未完成，可再次尝试。" }),
});

let focusReturnSequence = 0;

function isSafeAttribute(name) {
  return SAFE_ATTRIBUTES.has(name) || name.startsWith("aria-") || name.startsWith("data-");
}

function assertSafeUrl(value) {
  const rawValue = String(value);
  if (URL_CONTROL_CHARACTERS.test(rawValue)) throw new TypeError("Unsafe URL value");
  const stringValue = rawValue.trim();
  if (stringValue.startsWith("//") || stringValue.includes("\\")) throw new TypeError("Unsafe URL value");
  if (stringValue === "" || stringValue.startsWith("#") || stringValue.startsWith("/") || stringValue.startsWith("./") || stringValue.startsWith("../")) {
    return;
  }
  let parsed;
  try {
    parsed = new URL(stringValue, "https://admin.invalid/");
  } catch {
    throw new TypeError("Unsafe URL value");
  }
  if (!SAFE_URL_PROTOCOLS.has(parsed.protocol)) throw new TypeError("Unsafe URL protocol");
  if ((parsed.protocol === "http:" || parsed.protocol === "https:") && (parsed.username || parsed.password)) {
    throw new TypeError("Unsafe URL credentials");
  }
}

function appendSafeChild(document, element, child) {
  if (child === null || child === undefined || child === false) return;
  if (Array.isArray(child)) {
    for (const nested of child) appendSafeChild(document, element, nested);
    return;
  }
  if (typeof child === "string" || typeof child === "number" || typeof child === "bigint") {
    element.append(document.createTextNode(String(child)));
    return;
  }
  if (child?.ownerDocument === document && typeof child.nodeType === "number") {
    element.append(child);
    return;
  }
  throw new TypeError("Safe DOM children must be text or nodes owned by the supplied document");
}

/**
 * Creates a node without accepting HTML strings or event-handler attributes.
 * External values belong in `children`, where they are always text nodes.
 */
export function createSafeElement(document, tagName, attributes = {}, children = []) {
  if (!document?.createElement) throw new TypeError("A DOM document is required");
  const normalizedTag = String(tagName).toLowerCase();
  if (!SAFE_TAGS.has(normalizedTag)) throw new TypeError(`Unsafe element: ${normalizedTag}`);
  const element = document.createElement(normalizedTag);

  for (const [name, rawValue] of Object.entries(attributes ?? {})) {
    if (!isSafeAttribute(name)) throw new TypeError(`Unsafe attribute: ${name}`);
    if (rawValue === null || rawValue === undefined || rawValue === false) continue;
    const attributeName = name === "className" ? "class" : name === "tabIndex" ? "tabindex" : name;
    if (URL_ATTRIBUTES.has(attributeName)) assertSafeUrl(rawValue);
    if (BOOLEAN_ATTRIBUTES.has(attributeName)) {
      element.toggleAttribute(attributeName, Boolean(rawValue));
    } else {
      element.setAttribute(attributeName, String(rawValue));
    }
  }
  if (normalizedTag === "a" && element.getAttribute("target")?.toLowerCase() === "_blank") {
    const rel = new Set((element.getAttribute("rel") ?? "").toLowerCase().split(/\s+/).filter(Boolean));
    if (!rel.has("noopener") || !rel.has("noreferrer")) {
      throw new TypeError("Blank-target links require noopener noreferrer");
    }
  }

  appendSafeChild(document, element, children);
  return element;
}

// A concise alias for page modules while keeping the security intent explicit at the definition site.
export const createElement = createSafeElement;

export function renderPageState(container, options) {
  if (!container?.ownerDocument) throw new TypeError("A page-state container is required");
  const settings = typeof options === "string" ? { state: options } : options ?? {};
  const { state } = settings;
  if (!PAGE_STATES.includes(state)) throw new TypeError(`Unknown page state: ${state}`);

  const document = container.ownerDocument;
  const defaults = PAGE_STATE_DEFAULTS[state];
  const isAlert = ["partial", "stale", "unauthorized", "error", "retry"].includes(state);
  const wrapper = createSafeElement(
    document,
    "section",
    {
      className: `page-state page-state-${state}`,
      role: isAlert ? "alert" : "status",
      "aria-live": isAlert ? "assertive" : "polite",
      "aria-atomic": "true",
      "data-state": state,
    },
    [
      createSafeElement(document, "h2", { className: "page-state-title" }, settings.title ?? defaults.title),
      createSafeElement(document, "p", { className: "page-state-message" }, settings.message ?? defaults.message),
    ],
  );

  let destroy = () => {};
  if (state === "retry") {
    const retryButton = createSafeElement(
      document,
      "button",
      { type: "button", className: "button button-primary page-state-retry" },
      settings.retryLabel ?? "重试",
    );
    const retry = (event) => settings.onRetry?.(event);
    retryButton.addEventListener("click", retry);
    wrapper.append(retryButton);
    destroy = () => retryButton.removeEventListener("click", retry);
  }

  container.replaceChildren(wrapper);
  container.dataset.pageState = state;
  return { element: wrapper, destroy };
}

export function announceStatus(container, message, { durationMs = 4_000 } = {}) {
  if (!container?.ownerDocument) throw new TypeError("A status container is required");
  container.setAttribute("role", "status");
  container.setAttribute("aria-live", "polite");
  container.setAttribute("aria-atomic", "true");
  container.textContent = String(message ?? "");
  const timer = durationMs > 0 ? globalThis.setTimeout(() => container.replaceChildren(), durationMs) : null;
  return () => {
    if (timer !== null) globalThis.clearTimeout(timer);
  };
}

export function showPersistentAlert(container, { message, title = null, kind = "error", dismissLabel = null } = {}) {
  if (!container?.ownerDocument) throw new TypeError("An alert container is required");
  const { ownerDocument: document } = container;
  const alert = createSafeElement(
    document,
    "div",
    {
      role: "alert",
      className: `persistent-alert persistent-alert-${kind}`,
      "aria-live": "assertive",
      "aria-atomic": "true",
    },
    [
      title ? createSafeElement(document, "strong", { className: "persistent-alert-title" }, title) : null,
      createSafeElement(document, "span", { className: "persistent-alert-message" }, message ?? ""),
    ],
  );
  let dismissButton = null;
  let dismiss = () => alert.remove();
  if (dismissLabel) {
    dismissButton = createSafeElement(document, "button", {
      type: "button",
      className: "button persistent-alert-dismiss",
      "aria-label": title ? `${dismissLabel}：${title}` : dismissLabel,
    }, dismissLabel);
    dismissButton.addEventListener("click", dismiss);
    alert.append(dismissButton);
  }
  container.append(alert);
  return () => {
    dismissButton?.removeEventListener("click", dismiss);
    dismiss();
  };
}

export function createCursorControls({ document, onPrevious, onNext, label = "分页" } = {}) {
  if (!document?.createElement) throw new TypeError("A DOM document is required");
  const previousButton = createSafeElement(
    document,
    "button",
    { type: "button", className: "button cursor-previous", disabled: true },
    "上一页",
  );
  const nextButton = createSafeElement(
    document,
    "button",
    { type: "button", className: "button cursor-next", disabled: true },
    "下一页",
  );
  const page = createSafeElement(document, "span", { className: "cursor-page", "aria-live": "polite" }, "");
  const element = createSafeElement(document, "nav", { className: "cursor-controls", "aria-label": label }, [
    previousButton,
    page,
    nextButton,
  ]);
  const previous = (event) => onPrevious?.(event);
  const next = (event) => onNext?.(event);
  previousButton.addEventListener("click", previous);
  nextButton.addEventListener("click", next);

  return {
    element,
    update({ hasPrevious = false, hasNext = false, pageLabel = "" } = {}) {
      previousButton.disabled = !hasPrevious;
      nextButton.disabled = !hasNext;
      page.textContent = String(pageLabel ?? "");
    },
    destroy() {
      previousButton.removeEventListener("click", previous);
      nextButton.removeEventListener("click", next);
    },
  };
}

function validateMenuItems(items) {
  if (!Array.isArray(items) || items.length !== 6) throw new TypeError("The primary menu must contain exactly six items");
  const ids = new Set(items.map((item) => item?.id));
  if (ids.size !== 6 || ids.has(undefined) || ids.has("")) throw new TypeError("Menu item ids must be unique");
  if (items.some((item) => typeof item.label !== "string" || item.label.trim() === "")) {
    throw new TypeError("Every menu item requires a label");
  }
}

export function createAdminMenu({ document, items = ADMIN_MENU_ITEMS, activeId = items?.[0]?.id, onSelect } = {}) {
  if (!document?.createElement) throw new TypeError("A DOM document is required");
  validateMenuItems(items);
  if (!items.some((item) => item.id === activeId)) throw new TypeError(`Unknown active menu item: ${activeId}`);

  const list = createSafeElement(document, "ul", { className: "admin-menu-list" });
  const element = createSafeElement(document, "nav", { className: "admin-menu", "aria-label": "主菜单" }, list);
  const buttons = [];
  const listeners = [];

  function setActive(id, { focus = false, notify = false, event = null } = {}) {
    const index = items.findIndex((item) => item.id === id);
    if (index < 0) throw new TypeError(`Unknown menu item: ${id}`);
    buttons.forEach((button, buttonIndex) => {
      const current = buttonIndex === index;
      button.tabIndex = current ? 0 : -1;
      if (current) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    });
    if (focus) buttons[index].focus();
    if (notify) onSelect?.(items[index].id, items[index], event);
  }

  function activateIndex(index, event) {
    const bounded = (index + items.length) % items.length;
    setActive(items[bounded].id, { focus: true, notify: true, event });
  }

  items.forEach((item, index) => {
    const button = createSafeElement(
      document,
      "button",
      {
        type: "button",
        className: "admin-menu-item",
        "data-menu-id": item.id,
        "data-router-focus-id": `admin-menu-${item.id}`,
      },
      item.label,
    );
    const click = (event) => activateIndex(index, event);
    const keydown = (event) => {
      let destination = null;
      if (event.key === "ArrowRight" || event.key === "ArrowDown") destination = index + 1;
      if (event.key === "ArrowLeft" || event.key === "ArrowUp") destination = index - 1;
      if (event.key === "Home") destination = 0;
      if (event.key === "End") destination = items.length - 1;
      if (destination === null) return;
      event.preventDefault();
      activateIndex(destination, event);
    };
    button.addEventListener("click", click);
    button.addEventListener("keydown", keydown);
    listeners.push({ button, click, keydown });
    buttons.push(button);
    list.append(createSafeElement(document, "li", { className: "admin-menu-entry" }, button));
  });
  setActive(activeId);

  return {
    element,
    setActive,
    getActiveId() {
      return buttons.find((button) => button.getAttribute("aria-current") === "page")?.dataset.menuId ?? null;
    },
    destroy() {
      for (const { button, click, keydown } of listeners) {
        button.removeEventListener("click", click);
        button.removeEventListener("keydown", keydown);
      }
    },
  };
}

function focusableElements(root) {
  return [...root.querySelectorAll("button, [href], input, select, textarea, [tabindex]")].filter(
    (element) => !element.disabled && !element.hidden && element.tabIndex >= 0,
  );
}

export function createConfirmationDialog({
  document,
  title,
  message,
  confirmLabel = "确认",
  cancelLabel = "取消",
  onConfirm,
  onCancel,
} = {}) {
  if (!document?.createElement) throw new TypeError("A DOM document is required");
  const titleId = `confirmation-title-${++focusReturnSequence}`;
  const messageId = `confirmation-message-${focusReturnSequence}`;
  const titleElement = createSafeElement(document, "h2", { id: titleId }, title ?? "确认操作");
  const messageElement = createSafeElement(document, "p", { id: messageId, className: "confirmation-message" }, message ?? "");
  const alert = createSafeElement(document, "div", { role: "alert", className: "confirmation-error", hidden: true });
  const busyStatus = createSafeElement(document, "p", {
    role: "status",
    className: "confirmation-busy",
    hidden: true,
    "aria-live": "polite",
    "data-dialog-status": "busy",
  }, "正在提交，请稍候。");
  const cancelButton = createSafeElement(
    document,
    "button",
    { type: "button", className: "button confirmation-cancel", "data-dialog-action": "cancel" },
    cancelLabel,
  );
  const confirmButton = createSafeElement(
    document,
    "button",
    { type: "button", className: "button button-danger confirmation-confirm", "data-dialog-action": "confirm" },
    confirmLabel,
  );
  const actions = createSafeElement(document, "div", { className: "confirmation-actions" }, [cancelButton, confirmButton]);
  const panel = createSafeElement(document, "div", { className: "confirmation-panel" }, [
    titleElement,
    messageElement,
    busyStatus,
    alert,
    actions,
  ]);
  const element = createSafeElement(
    document,
    "div",
    {
      className: "confirmation-backdrop",
      role: "dialog",
      "aria-modal": "true",
      "aria-labelledby": titleId,
      "aria-describedby": messageId,
      hidden: true,
    },
    panel,
  );

  let open = false;
  let busy = false;
  let returnFocus = null;

  function setBusy(nextBusy) {
    busy = Boolean(nextBusy);
    cancelButton.disabled = busy;
    confirmButton.disabled = busy;
    busyStatus.hidden = !busy;
    element.setAttribute("aria-busy", String(busy));
  }

  function close(reason = "dismiss", { notify = true } = {}) {
    if (!open) return;
    open = false;
    element.hidden = true;
    element.removeAttribute("open");
    setBusy(false);
    if (notify && reason === "cancel") onCancel?.();
    const target = returnFocus?.isConnected
      ? returnFocus
      : document.querySelector("[data-route-heading], [data-page-title], main h1, h1");
    returnFocus = null;
    if (target?.isConnected && typeof target.focus === "function") {
      if (!target.hasAttribute("tabindex")) target.setAttribute("tabindex", "-1");
      target.focus();
    }
  }

  function cancel(event) {
    event?.preventDefault();
    if (!busy) close("cancel");
  }

  async function confirm(event) {
    event?.preventDefault();
    if (busy) return;
    alert.hidden = true;
    alert.replaceChildren();
    setBusy(true);
    try {
      const result = await onConfirm?.();
      if (result !== false) close("confirm", { notify: false });
      else setBusy(false);
    } catch (error) {
      alert.textContent = error instanceof Error ? error.message : String(error ?? "操作失败");
      alert.hidden = false;
      setBusy(false);
      confirmButton.focus();
    }
  }

  function keydown(event) {
    if (!open) return;
    if (event.key === "Escape") {
      cancel(event);
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = focusableElements(element);
    if (focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  cancelButton.addEventListener("click", cancel);
  confirmButton.addEventListener("click", confirm);
  element.addEventListener("keydown", keydown);

  return {
    element,
    open(trigger = document.activeElement) {
      if (open) return;
      returnFocus = trigger?.isConnected ? trigger : null;
      open = true;
      element.hidden = false;
      element.setAttribute("open", "");
      alert.hidden = true;
      alert.replaceChildren();
      setBusy(false);
      // Destructive confirmation never receives initial focus.
      cancelButton.focus();
    },
    close,
    setBusy,
    isOpen() {
      return open;
    },
    destroy() {
      cancelButton.removeEventListener("click", cancel);
      confirmButton.removeEventListener("click", confirm);
      element.removeEventListener("keydown", keydown);
      close("destroy", { notify: false });
      element.remove();
    },
  };
}

function findFocusReturnElement(document, id) {
  if (!id) return null;
  return [...document.querySelectorAll("[data-router-focus-id]")].find(
    (element) => element.dataset.routerFocusId === id,
  ) ?? null;
}

export function createHistoryFocusReturn({ window } = {}) {
  if (!window?.document || !window?.history) throw new TypeError("A browser window is required");
  const { document, history } = window;
  const ownedElements = new Set();
  let destroyed = false;

  function remember(trigger) {
    if (destroyed) throw new Error("History focus helper has been destroyed");
    if (!trigger?.isConnected || typeof trigger.focus !== "function") {
      throw new TypeError("A connected focusable trigger is required");
    }
    const id = trigger.dataset.routerFocusId || `focus-return-${++focusReturnSequence}`;
    trigger.dataset.routerFocusId = id;
    ownedElements.add(trigger);
    const currentState = history.state && typeof history.state === "object" ? history.state : {};
    history.replaceState({ ...currentState, focusId: id, scrollY: Math.max(0, Math.round(window.scrollY || 0)) }, "", window.location.href);
    return id;
  }

  function restore(state = history.state) {
    if (destroyed) return false;
    const id = state && typeof state === "object" ? state.focusId : null;
    const target = findFocusReturnElement(document, id);
    if (!target) return false;
    window.queueMicrotask(() => {
      if (!destroyed && target.isConnected) {
        target.focus();
        window.scrollTo(0, Math.max(0, Math.round(state?.scrollY || 0)));
      }
    });
    return true;
  }

  function popstate(event) {
    restore(event.state);
  }

  window.addEventListener("popstate", popstate);
  return {
    remember,
    restore,
    push(url, { trigger = document.activeElement, state = {} } = {}) {
      remember(trigger);
      history.pushState({ ...state }, "", url);
    },
    destroy() {
      if (destroyed) return;
      destroyed = true;
      window.removeEventListener("popstate", popstate);
      for (const element of ownedElements) {
        if (element.dataset.routerFocusId?.startsWith("focus-return-")) delete element.dataset.routerFocusId;
      }
      ownedElements.clear();
    },
  };
}

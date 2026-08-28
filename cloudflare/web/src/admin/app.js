import { createApiClient, AdminApiError } from "./api.js";
import { createRouter, parseRoute, serializeRoute } from "./router.js";
import {
  ADMIN_MENU_ITEMS,
  announceStatus,
  createAdminMenu,
  createConfirmationDialog,
  createElement,
  isCurrentPageActivation,
  showPersistentAlert,
} from "./components.js";
import { createAuditPage } from "./pages/audit.js";
import { createOverviewPage } from "./pages/overview.js";
import { createRomPage } from "./pages/rom.js";
import { createSessionsPage } from "./pages/sessions.js";
import { createUsersPage } from "./pages/users.js";
import { createVersionsPage } from "./pages/versions.js";

const VIEW_COPY = Object.freeze({
  overview: Object.freeze({ title: "概览", eyebrow: "SYSTEM OVERVIEW", description: "查看版本、用户、会话与结构化追踪的权威摘要。" }),
  versions: Object.freeze({ title: "版本策略", eyebrow: "VERSION POLICY", description: "管理最低版本、下载地址与客户端更新策略。" }),
  users: Object.freeze({ title: "用户管理", eyebrow: "IDENTITY CONTROL", description: "管理 API 用户、状态与凭据轮换。" }),
  sessions: Object.freeze({ title: "在线会话", eyebrow: "LIVE SESSIONS", description: "查看当前连接并执行审计化的强制下线。" }),
  audit: Object.freeze({ title: "操作审计", eyebrow: "TRACE EVIDENCE", description: "按用户、运行、事件、命令和输出逐级核对持久化证据。" }),
  rom: Object.freeze({ title: "ROM 查询", eyebrow: "ROM ACTIVITY", description: "检索 ROM 查询状态与服务端结果。" }),
});
const PAGE_FACTORIES = Object.freeze({
  overview: createOverviewPage,
  versions: createVersionsPage,
  users: createUsersPage,
  sessions: createSessionsPage,
  audit: createAuditPage,
  rom: createRomPage,
});

const root = document.getElementById("admin-app");
let router = null;
let menu = null;
let cleanup = [];
let currentAdmin = null;
let currentRoute = null;
let currentPage = null;
let currentPageController = null;
const openDialogs = new Set();
let statusCancel = () => {};

const api = createApiClient({
  fetchImpl: (...args) => window.fetch(...args),
  onUnauthorized: () => {
    currentAdmin = null;
    if (!root.classList.contains("login-screen")) {
      showLogin({ alert: "会话已失效，请重新登录。" });
    }
  },
});

function listen(target, type, handler, options) {
  target.addEventListener(type, handler, options);
  cleanup.push(() => target.removeEventListener(type, handler, options));
}

function teardown() {
  clearCurrentPage();
  router?.destroy();
  router = null;
  menu?.destroy();
  menu = null;
  statusCancel();
  statusCancel = () => {};
  for (const dispose of cleanup.splice(0)) dispose();
}

function setDocumentTitle(label) {
  document.title = `${label} · Nwflash 运营控制台`;
}

function buildBrand(compact = false) {
  return createElement(document, "div", { className: compact ? "brand brand-compact" : "brand" }, [
    createElement(document, "span", { className: "brand-mark", "aria-hidden": "true" }, "NW"),
    createElement(document, "span", { className: "brand-copy" }, [
      createElement(document, "strong", {}, "Nwflash"),
      createElement(document, "small", {}, "OPERATIONS CONSOLE"),
    ]),
  ]);
}

function showLogin({ alert = null } = {}) {
  teardown();
  currentAdmin = null;
  setDocumentTitle("管理员登录");
  root.className = "login-screen";
  root.removeAttribute("aria-busy");

  const alertHost = createElement(document, "div", { className: "login-alerts" });
  const username = createElement(document, "input", {
    id: "admin-username",
    name: "username",
    type: "text",
    autocomplete: "username",
    required: true,
  });
  const password = createElement(document, "input", {
    id: "admin-password",
    name: "password",
    type: "password",
    autocomplete: "current-password",
    required: true,
  });
  const submit = createElement(document, "button", { type: "submit", className: "button button-primary" }, "登录");
  const form = createElement(document, "form", { className: "login-form" }, [
    createElement(document, "div", { className: "field" }, [
      createElement(document, "label", { for: "admin-username" }, "用户名"),
      username,
    ]),
    createElement(document, "div", { className: "field" }, [
      createElement(document, "label", { for: "admin-password" }, "密码"),
      password,
    ]),
    submit,
  ]);
  const card = createElement(document, "main", { className: "login-card" }, [
    buildBrand(),
    createElement(document, "p", { className: "eyebrow" }, "AUTHENTICATED ACCESS"),
    createElement(document, "h1", {}, "管理员登录"),
    createElement(document, "p", { className: "muted" }, "使用服务端会话进入运营控制台。凭据不会写入地址或浏览器存储。"),
    alertHost,
    form,
  ]);
  root.replaceChildren(card);
  if (alert) showPersistentAlert(alertHost, { message: alert, title: "需要重新认证" });

  listen(form, "submit", async (event) => {
    event.preventDefault();
    submit.disabled = true;
    alertHost.replaceChildren();
    try {
      const result = await api.login(username.value, password.value);
      password.value = "";
      currentAdmin = result?.username ?? username.value;
      showShell();
    } catch (error) {
      password.value = "";
      showPersistentAlert(alertHost, {
        message: error instanceof AdminApiError ? error.message : "登录请求失败。",
        title: "登录失败",
      });
      submit.disabled = false;
      password.focus();
    }
  });
  username.focus();
}

function showShell() {
  teardown();
  root.className = "admin-shell";
  root.removeAttribute("aria-busy");

  const status = createElement(document, "div", { className: "status-announcer" });
  const alerts = createElement(document, "div", { className: "alert-stack" });
  const page = createElement(document, "main", { id: "workspace", className: "workspace", tabindex: "-1" });
  const accountMenu = createElement(document, "div", { id: "account-menu", className: "account-menu", hidden: true });
  const accountButton = createElement(document, "button", {
    type: "button",
    className: "button account-trigger",
    "aria-expanded": "false",
    "aria-controls": "account-menu",
  }, [createElement(document, "span", { className: "account-name" }, currentAdmin ?? "admin"), "账户"]);
  const passwordButton = createElement(document, "button", { type: "button", className: "account-action" }, "修改密码");
  const logoutButton = createElement(document, "button", { type: "button", className: "account-action account-logout" }, "退出登录");
  accountMenu.append(passwordButton, logoutButton);
  const globalSearchInput = createElement(document, "input", {
    type: "search",
    name: "q",
    placeholder: "搜索用户、运行、分区、错误码、序列号或 URL",
    "aria-label": "全局搜索",
  });
  const globalSearch = createElement(document, "form", { className: "global-search", role: "search" }, [
    globalSearchInput,
    createElement(document, "button", { type: "submit", className: "button" }, "搜索"),
  ]);

  menu = createAdminMenu({
    document,
    items: ADMIN_MENU_ITEMS,
    activeId: "overview",
    onSelect: (view, _item, event) => {
      const keepMenuFocus = event?.type === "keydown";
      void router?.navigate({ view }).then(() => {
        if (keepMenuFocus) menu?.setActive(view, { focus: true });
      });
    },
  });
  const currentPathPage = createElement(document, "strong", { className: "current-path-page", "aria-current": "page" }, VIEW_COPY.overview.title);
  const currentPath = createElement(document, "nav", { className: "current-path", "aria-label": "当前位置" }, [
    createElement(document, "span", { className: "current-path-prefix" }, "NWFLASH"),
    createElement(document, "span", { className: "current-path-separator", "aria-hidden": "true" }, " / "),
    createElement(document, "span", { className: "current-path-prefix" }, "ADMIN"),
    createElement(document, "span", { className: "current-path-separator", "aria-hidden": "true" }, " / "),
    currentPathPage,
  ]);
  const serviceHealth = createElement(document, "div", {
    className: "service-health",
    role: "status",
    "aria-label": "服务健康",
  }, [
    createElement(document, "span", { className: "service-health-indicator", "aria-hidden": "true" }),
    createElement(document, "span", { className: "service-health-copy" }, [
      createElement(document, "strong", {}, "管理员会话"),
      createElement(document, "small", {}, "会话已验证"),
    ]),
  ]);
  const sidebar = createElement(document, "div", { className: "shell-sidebar" }, [
    buildBrand(true),
    menu.element,
    serviceHealth,
  ]);
  const header = createElement(document, "header", { className: "topbar" }, [
    createElement(document, "div", { className: "topbar-inner" }, [
      currentPath,
      globalSearch,
      createElement(document, "div", { className: "account" }, [accountButton, accountMenu]),
    ]),
  ]);
  const shellContent = createElement(document, "div", { className: "shell-content" }, [header, alerts, status, page]);
  root.replaceChildren(sidebar, shellContent);

  listen(accountButton, "click", () => {
    const nextHidden = !accountMenu.hidden;
    accountMenu.hidden = nextHidden;
    accountButton.setAttribute("aria-expanded", String(!nextHidden));
    if (!nextHidden) passwordButton.focus();
  });
  listen(document, "keydown", (event) => {
    if (event.key === "Escape" && !accountMenu.hidden) {
      accountMenu.hidden = true;
      accountButton.setAttribute("aria-expanded", "false");
      accountButton.focus();
    }
  });
  listen(passwordButton, "click", () => {
    accountMenu.hidden = true;
    accountButton.setAttribute("aria-expanded", "false");
    showPasswordForm({ page, alerts, status, returnFocus: accountButton });
  });
  listen(globalSearch, "submit", (event) => {
    event.preventDefault();
    const q = globalSearchInput.value.trim();
    if (!q) return;
    void router?.navigate({ view: "audit", q, level: "overview" });
  });
  listen(logoutButton, "click", async () => {
    logoutButton.disabled = true;
    try {
      await api.logout();
    } catch (error) {
      if (!(error instanceof AdminApiError && error.kind === "unauthorized")) {
        showPersistentAlert(alerts, { title: "退出失败", message: error?.message ?? "退出请求失败。" });
        logoutButton.disabled = false;
        return;
      }
    }
    showLogin({ alert: "已安全退出。" });
  });

  const pageContext = createPageContext({ alerts, status });
  router = createRouter({
    window,
    onRoute: (route) => {
      globalSearchInput.value = route.view === "audit" ? route.q ?? "" : "";
      currentPathPage.textContent = (VIEW_COPY[route.view] ?? VIEW_COPY.overview).title;
      return renderRoute(route, page, pageContext);
    },
  });
  void router.start();
}

async function renderRoute(route, page, pageContext) {
  clearCurrentPage();
  currentRoute = route;
  const copy = VIEW_COPY[route.view] ?? VIEW_COPY.overview;
  menu?.setActive(route.view);
  setDocumentTitle(copy.title);
  const heading = createElement(document, "h1", {
    id: `view-heading-${route.view}`,
    "data-route-heading": "true",
    tabindex: "-1",
  }, copy.title);
  const routeDetails = route.view === "audit" && route.level
    ? createElement(document, "p", { className: "route-context" }, `审计层级：${route.level}`)
    : null;
  const headingGroup = createElement(document, "div", { className: "workspace-heading" }, [
    createElement(document, "p", { className: "eyebrow" }, copy.eyebrow),
    heading,
    createElement(document, "p", { className: "workspace-description" }, copy.description),
    routeDetails,
  ]);
  const createPage = PAGE_FACTORIES[route.view] ?? PAGE_FACTORIES.overview;
  const controller = new AbortController();
  const pageInstance = createPage(pageContext);
  currentPageController = controller;
  currentPage = pageInstance;
  page.replaceChildren(headingGroup, pageInstance.element);
  try {
    await pageInstance.activate(route, controller.signal);
  } catch (error) {
    if (error?.name !== "AbortError" && isCurrentPageActivation(
      currentPage,
      currentPageController,
      pageInstance,
      controller,
    )) {
      pageContext.alert(error?.message ?? "页面加载失败。", { title: "无法打开工作区" });
    }
  }
  return route;
}

function clearCurrentPage() {
  currentPageController?.abort();
  currentPageController = null;
  clearOpenDialogs();
  currentPage?.deactivate?.();
  currentPage?.destroy?.();
  currentPage = null;
}

function clearOpenDialogs() {
  for (const dialog of openDialogs) dialog.destroy();
  openDialogs.clear();
}

function createPageContext({ alerts, status }) {
  return Object.freeze({
    document,
    window,
    api,
    navigate: (route, options) => router?.navigate(route, options),
    announce: (message) => {
      statusCancel();
      statusCancel = announceStatus(status, message);
    },
    alert: (message, options = {}) => showPersistentAlert(alerts, {
      message,
      title: options.title ?? null,
      kind: options.kind ?? "error",
      dismissLabel: options.dismissLabel ?? "关闭",
    }),
    confirm: (options) => openConfirmation(options),
  });
}

function openConfirmation(options = {}) {
  let dialog;
  dialog = createConfirmationDialog({
    document,
    title: options.title ?? "确认操作",
    message: options.message ?? "请确认是否继续。",
    confirmLabel: options.confirmLabel ?? "确认",
    cancelLabel: options.cancelLabel ?? "取消",
    onConfirm: async () => {
      const result = await options.onConfirm?.();
      window.setTimeout(() => {
        dialog.destroy();
        openDialogs.delete(dialog);
      }, 0);
      return result;
    },
    onCancel: () => {
      options.onCancel?.();
      window.queueMicrotask(() => {
        dialog.destroy();
        openDialogs.delete(dialog);
      });
    },
  });
  openDialogs.add(dialog);
  root.append(dialog.element);
  dialog.open(options.trigger ?? document.activeElement);
  return dialog;
}

function showPasswordForm({ page, alerts, status, returnFocus }) {
  clearCurrentPage();
  const password = createElement(document, "input", {
    id: "new-admin-password",
    type: "password",
    autocomplete: "new-password",
    minlength: "8",
    required: true,
  });
  const confirm = createElement(document, "input", {
    id: "confirm-admin-password",
    type: "password",
    autocomplete: "new-password",
    minlength: "8",
    required: true,
  });
  const cancel = createElement(document, "button", { type: "button", className: "button" }, "取消");
  const submit = createElement(document, "button", { type: "submit", className: "button button-primary" }, "保存并重新登录");
  const form = createElement(document, "form", { className: "workspace-card password-form" }, [
    createElement(document, "h1", { "data-route-heading": "true", tabindex: "-1" }, "修改密码"),
    createElement(document, "p", { className: "muted" }, "修改成功后服务端会吊销现有管理员会话。"),
    createElement(document, "div", { className: "field" }, [
      createElement(document, "label", { for: "new-admin-password" }, "新密码"), password,
    ]),
    createElement(document, "div", { className: "field" }, [
      createElement(document, "label", { for: "confirm-admin-password" }, "确认新密码"), confirm,
    ]),
    createElement(document, "div", { className: "form-actions" }, [cancel, submit]),
  ]);
  page.replaceChildren(form);
  const pageContext = createPageContext({ alerts, status });
  listen(cancel, "click", async () => {
    await renderRoute(currentRoute ?? { view: "overview" }, page, pageContext);
    if (returnFocus?.isConnected && typeof returnFocus.focus === "function") returnFocus.focus();
  });
  listen(form, "submit", async (event) => {
    event.preventDefault();
    alerts.replaceChildren();
    if (password.value.length < 8 || password.value !== confirm.value) {
      showPersistentAlert(alerts, { title: "无法保存", message: "两次密码必须一致且至少 8 位。" });
      password.value = "";
      confirm.value = "";
      password.focus();
      return;
    }
    submit.disabled = true;
    try {
      await api.changePassword(password.value);
      password.value = "";
      confirm.value = "";
      statusCancel();
      statusCancel = announceStatus(status, "密码已更新，请重新登录。", { durationMs: 0 });
      showLogin({ alert: "密码已更新，所有旧会话已吊销。" });
    } catch (error) {
      password.value = "";
      confirm.value = "";
      showPersistentAlert(alerts, { title: "修改失败", message: error?.message ?? "密码修改失败。" });
      submit.disabled = false;
      password.focus();
    }
  });
  password.focus();
}

async function start() {
  canonicalizePublicLocation();
  try {
    const me = await api.restoreSession();
    if (me?.loggedIn) {
      currentAdmin = me.username ?? "admin";
      showShell();
    } else {
      showLogin();
    }
  } catch {
    showLogin({ alert: "无法恢复会话，请重新登录。" });
  }
}

function canonicalizePublicLocation() {
  const route = parseRoute(window.location.search);
  const search = serializeRoute(route);
  if (window.location.search !== search || window.location.hash) {
    window.history.replaceState(null, "", `${window.location.pathname}${search}`);
  }
}

void start();

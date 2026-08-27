import { createApiClient, AdminApiError } from "./api.js";
import { createRouter, parseRoute, serializeRoute } from "./router.js";
import {
  ADMIN_MENU_ITEMS,
  announceStatus,
  createAdminMenu,
  createElement,
  renderPageState,
  showPersistentAlert,
} from "./components.js";

const VIEW_COPY = Object.freeze({
  overview: Object.freeze({ title: "概览", eyebrow: "SYSTEM OVERVIEW", description: "查看版本、用户、会话与结构化追踪的权威摘要。" }),
  versions: Object.freeze({ title: "版本策略", eyebrow: "VERSION POLICY", description: "管理最低版本、下载地址与客户端更新策略。" }),
  users: Object.freeze({ title: "用户管理", eyebrow: "IDENTITY CONTROL", description: "管理 API 用户、状态与凭据轮换。" }),
  sessions: Object.freeze({ title: "在线会话", eyebrow: "LIVE SESSIONS", description: "查看当前连接并执行审计化的强制下线。" }),
  audit: Object.freeze({ title: "操作审计", eyebrow: "TRACE EVIDENCE", description: "按用户、运行、事件、命令和输出逐级核对持久化证据。" }),
  rom: Object.freeze({ title: "ROM 查询", eyebrow: "ROM ACTIVITY", description: "检索 ROM 查询状态与服务端结果。" }),
});

const root = document.getElementById("admin-app");
let router = null;
let menu = null;
let cleanup = [];
let currentAdmin = null;
let currentRoute = null;
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
  const header = createElement(document, "header", { className: "topbar" }, [
    createElement(document, "div", { className: "topbar-inner" }, [
      buildBrand(true),
      createElement(document, "div", { className: "account" }, [accountButton, accountMenu]),
    ]),
    menu.element,
  ]);
  root.replaceChildren(header, alerts, status, page);

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
    showPasswordForm({ page, alerts, status });
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

  router = createRouter({ window, onRoute: (route) => renderRoute(route, page) });
  void router.start();
}

function renderRoute(route, page) {
  currentRoute = route;
  const copy = VIEW_COPY[route.view] ?? VIEW_COPY.overview;
  menu?.setActive(route.view);
  setDocumentTitle(copy.title);
  const heading = createElement(document, "h1", {
    id: `view-heading-${route.view}`,
    "data-route-heading": "true",
    tabindex: "-1",
  }, copy.title);
  const placeholder = createElement(document, "section", { className: "workspace-card" }, [
    createElement(document, "h2", {}, "模块入口已就绪"),
    createElement(document, "p", { className: "muted" }, "当前 Shell 只建立认证、路由与安全组件边界；数据工作区由后续模块接入稳定 API。"),
  ]);
  const routeDetails = route.view === "audit" && route.level
    ? createElement(document, "p", { className: "route-context" }, `审计层级：${route.level}`)
    : null;
  page.replaceChildren(
    createElement(document, "div", { className: "workspace-heading" }, [
      createElement(document, "p", { className: "eyebrow" }, copy.eyebrow),
      heading,
      createElement(document, "p", { className: "workspace-description" }, copy.description),
      routeDetails,
    ]),
    placeholder,
  );
  return route;
}

function showPasswordForm({ page, alerts, status }) {
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
  listen(cancel, "click", () => renderRoute(currentRoute ?? { view: "overview" }, page));
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

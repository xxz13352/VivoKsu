class PortalApiError extends Error {
  constructor(message, status = 0) {
    super(message);
    this.name = 'PortalApiError';
    this.status = status;
  }
}

const STATUS_LABELS = new Map([
  ['success', '成功'],
  ['failed', '失败'],
  ['canceled', '已取消']
]);

const TYPE_LABELS = new Map([
  ['operation', '工具操作'],
  ['rom', 'ROM 查询']
]);

export function createPortal({
  document,
  window,
  fetchImpl = window.fetch.bind(window),
  setTimeoutImpl = window.setTimeout.bind(window),
  clearTimeoutImpl = window.clearTimeout.bind(window)
}) {
  const select = (selector) => document.querySelector(selector);
  const maxSessionPollAttempts = 6;
  const state = {
    view: 'overview',
    type: 'all',
    status: 'all',
    activity: '',
    pendingKicks: new Set(),
    kickTarget: '',
    focusTarget: null,
    pollTimer: null,
    pollAttempts: 0,
    overviewGeneration: 0,
    activityListGeneration: 0,
    detailGeneration: 0,
    sessionsGeneration: 0,
    bootstrapGeneration: 0,
    loginGeneration: 0,
    kickGeneration: 0,
    passwordGeneration: 0
  };
  let started = false;

  function setText(selector, value) {
    select(selector).textContent = String(value ?? '—');
  }

  function clearNodes(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  function announce(message) {
    select('[data-live-status]').textContent = String(message ?? '');
  }

  function paramsFromUrl() {
    const params = new window.URLSearchParams(window.location.search);
    const view = params.get('view');
    state.view = ['overview', 'activity', 'sessions', 'security'].includes(view) ? view : 'overview';
    state.type = ['all', 'operation', 'rom'].includes(params.get('type')) ? params.get('type') : 'all';
    state.status = ['all', 'success', 'failed', 'canceled'].includes(params.get('status')) ? params.get('status') : 'all';
    state.activity = params.get('activity') || '';
  }

  function writeUrl(push = true) {
    const params = new window.URLSearchParams();
    params.set('view', state.view);
    if (state.view === 'activity') {
      params.set('type', state.type);
      params.set('status', state.status);
      if (state.activity) params.set('activity', state.activity);
    }
    const url = `${window.location.pathname}?${params.toString()}`;
    window.history[push ? 'pushState' : 'replaceState']({}, '', url);
  }

  function showView() {
    document.querySelectorAll('[data-app] [data-view]').forEach((node) => {
      node.hidden = node.dataset.view !== state.view;
    });
    document.querySelectorAll('[data-nav]').forEach((button) => {
      if (button.dataset.nav === state.view) button.setAttribute('aria-current', 'page');
      else button.removeAttribute('aria-current');
    });
    select('[data-activity-type]').value = state.type;
    select('[data-activity-status]').value = state.status;
    if (state.view !== 'sessions') clearTimeoutImpl(state.pollTimer);
  }

  function setSurface(surface, mode, message = '') {
    const root = select(`[data-view="${surface}"]`);
    const loading = select(`[data-${surface}-loading]`);
    const empty = select(`[data-${surface}-empty]`);
    const error = select(`[data-${surface}-error]`);
    root?.setAttribute('aria-busy', mode === 'loading' ? 'true' : 'false');
    if (loading) loading.hidden = mode !== 'loading';
    if (empty) empty.hidden = mode !== 'empty';
    if (error) error.hidden = mode !== 'error';
    const messageNode = select(`[data-${surface}-error-message]`);
    if (messageNode && message) messageNode.textContent = message;
  }

  function responseMessage(data) {
    return typeof data?.message === 'string' && data.message
      ? data.message
      : '暂时无法加载，请稍后重试。';
  }

  async function api(path, options = {}) {
    const method = (options.method || 'GET').toUpperCase();
    const headers = new Headers(options.headers || {});
    if (method !== 'GET') headers.set('X-Requested-With', 'XMLHttpRequest');
    let response;
    try {
      response = await fetchImpl(path, { ...options, method, headers, credentials: 'same-origin' });
    } catch (error) {
      throw new PortalApiError(error instanceof Error ? error.message : '网络请求失败。');
    }
    const data = await response.json().catch(() => ({}));
    if (!response.ok) throw new PortalApiError(responseMessage(data), response.status);
    return data;
  }

  function handleAuthenticatedError(error) {
    if (error instanceof PortalApiError && error.status === 401) {
      showLogin();
      return true;
    }
    return false;
  }

  async function loadOverview() {
    const generation = ++state.overviewGeneration;
    setSurface('overview', 'loading');
    select('[data-overview-content]').hidden = true;
    announce('正在加载概览。');
    try {
      const overview = await api('/api/me/overview');
      if (generation !== state.overviewGeneration || state.view !== 'overview') return;
      const fields = [
        ['total', 'total'],
        ['operations', 'operations'],
        ['rom', 'rom'],
        ['successes', 'successes'],
        ['failures', 'failures'],
        ['sessions', 'activeSessions']
      ];
      fields.forEach(([target, source]) => setText(`[data-overview-${target}]`, overview[source]));
      const hasData = fields.some(([, source]) => Number(overview[source]) > 0);
      setSurface('overview', hasData ? 'success' : 'empty');
      select('[data-overview-content]').hidden = !hasData;
      announce(hasData ? '概览已更新。' : '概览暂无活动。');
    } catch (error) {
      if (generation !== state.overviewGeneration) return;
      if (!handleAuthenticatedError(error)) {
        setSurface('overview', 'error', error.message);
        announce(error.message);
      }
    }
  }

  function formatTimestamp(value) {
    if (value === null || value === undefined || value === '') return '—';
    const seconds = Number(value);
    if (!Number.isFinite(seconds)) return '—';
    const date = new Date(seconds * 1000);
    if (Number.isNaN(date.getTime())) return '—';
    return `${date.toISOString().slice(0, 19).replace('T', ' ')} UTC`;
  }

  function formatDurationMs(value) {
    if (value === null || value === undefined || !Number.isFinite(Number(value))) return '—';
    const milliseconds = Math.max(0, Number(value));
    if (milliseconds < 1000) return `${Math.round(milliseconds)} 毫秒`;
    const seconds = Math.round(milliseconds / 1000);
    if (seconds < 60) return `${seconds} 秒`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes} 分钟`;
    return `${Math.floor(minutes / 60)} 小时`;
  }

  function statusLabel(status) {
    return STATUS_LABELS.get(status) ?? '未知状态';
  }

  function typeLabel(type) {
    return TYPE_LABELS.get(type) ?? '活动';
  }

  function updateActivitySelection() {
    document.querySelectorAll('[data-activity]').forEach((button) => {
      button.setAttribute('aria-pressed', button.dataset.activity === state.activity ? 'true' : 'false');
    });
  }

  function renderActivities(activities) {
    const list = select('[data-activity-list]');
    clearNodes(list);
    activities.forEach((activity) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.dataset.activity = activity.id;
      button.className = 'activity-row';
      button.setAttribute('aria-pressed', activity.id === state.activity ? 'true' : 'false');

      const title = document.createElement('span');
      title.className = 'activity-row-title';
      title.textContent = activity.summary || typeLabel(activity.type);
      const metadata = document.createElement('span');
      metadata.className = 'activity-row-meta';
      const parts = [
        activity.id,
        typeLabel(activity.type),
        statusLabel(activity.status),
        formatTimestamp(activity.timestamp)
      ];
      if (activity.type === 'operation') parts.push(formatDurationMs(activity.duration_ms));
      if (activity.type === 'rom' && activity.version) parts.push(`版本 ${activity.version}`);
      metadata.textContent = parts.join(' · ');
      button.append(title, metadata);
      list.append(button);
    });
  }

  function setDetailState(mode, message = '') {
    const detail = select('[data-activity-detail]');
    detail.setAttribute('aria-busy', mode === 'loading' ? 'true' : 'false');
    select('[data-detail-idle]').hidden = mode !== 'idle';
    select('[data-detail-loading]').hidden = mode !== 'loading';
    select('[data-detail-error]').hidden = mode !== 'error';
    select('[data-detail-content]').hidden = mode !== 'success';
    if (message) setText('[data-detail-error-message]', message);
  }

  function resetActivityDetail() {
    state.detailGeneration += 1;
    clearNodes(select('[data-step-list]'));
    setDetailState('idle');
    updateActivitySelection();
  }

  function renderActivityDetail(data) {
    setText('[data-detail-id]', data.id);
    setText('[data-detail-type]', typeLabel(data.type));
    setText('[data-detail-status]', statusLabel(data.status));
    setText('[data-detail-summary]', data.summary);
    setText('[data-detail-time]', formatTimestamp(data.timestamp));
    setText('[data-detail-end-time]', formatTimestamp(data.ended_at));
    setText('[data-detail-duration]', formatDurationMs(data.duration_ms));
    setText('[data-detail-pd]', data.pd);
    setText('[data-detail-version]', data.version);
    setText('[data-detail-http-status]', data.http_status);

    const steps = select('[data-step-list]');
    clearNodes(steps);
    const unavailable = data.steps_state === 'unavailable';
    const telemetryMessage = unavailable ? '无更详细数据' : '不适用';
    setText('[data-detail-telemetry]', telemetryMessage);
    setText('[data-step-state]', telemetryMessage);
    if (!unavailable) {
      (Array.isArray(data.steps) ? data.steps : []).forEach((step) => {
        const row = document.createElement('p');
        row.dataset.stepRow = '';
        row.textContent = [step.phase, step.status, step.duration].filter(Boolean).join(' · ');
        steps.append(row);
      });
    }
  }

  async function loadActivityDetail(id) {
    const generation = ++state.detailGeneration;
    setDetailState('loading');
    updateActivitySelection();
    announce('正在加载活动详情。');
    try {
      const data = await api(`/api/me/activities/${encodeURIComponent(id)}`);
      if (generation !== state.detailGeneration || state.activity !== id || state.view !== 'activity') return;
      renderActivityDetail(data);
      setDetailState('success');
      announce('活动详情已加载。');
    } catch (error) {
      if (generation !== state.detailGeneration || state.activity !== id) return;
      if (!handleAuthenticatedError(error)) {
        const message = error instanceof PortalApiError && error.status === 404 ? '未找到该活动。' : error.message;
        setDetailState('error', message);
        announce(message);
      }
    }
  }

  async function loadActivities() {
    const generation = ++state.activityListGeneration;
    setSurface('activity', 'loading');
    select('[data-activity-retry]').hidden = true;
    renderActivities([]);
    if (!state.activity) resetActivityDetail();
    announce('正在加载活动。');
    try {
      const data = await api(`/api/me/activities?type=${encodeURIComponent(state.type)}&status=${encodeURIComponent(state.status)}&limit=50&offset=0`);
      if (generation !== state.activityListGeneration || state.view !== 'activity') return;
      const activities = Array.isArray(data.activities) ? data.activities : [];
      renderActivities(activities);
      setSurface('activity', activities.length ? 'success' : 'empty');
      announce(activities.length ? '活动列表已更新。' : '暂无活动记录。');
      if (state.activity) await loadActivityDetail(state.activity);
    } catch (error) {
      if (generation !== state.activityListGeneration) return;
      if (!handleAuthenticatedError(error)) {
        setSurface('activity', 'error', error.message);
        select('[data-activity-retry]').hidden = false;
        announce(error.message);
      }
    }
  }

  function sessionRow(id) {
    return [...select('[data-session-list]').children]
      .find((row) => row.dataset.session === id) ?? null;
  }

  function createSessionRow() {
    const row = document.createElement('article');
    row.className = 'session-row';
    const summary = document.createElement('p');
    summary.dataset.sessionSummary = '';
    summary.className = 'session-summary';
    const status = document.createElement('p');
    status.dataset.sessionStatus = '';
    status.setAttribute('role', 'status');
    const kick = document.createElement('button');
    kick.type = 'button';
    kick.textContent = '结束会话';
    row.append(summary, status, kick);
    return row;
  }

  function renderSessions(sessions) {
    const list = select('[data-session-list]');
    const focusedElement = document.activeElement;
    const existing = new Map([...list.children].map((row) => [row.dataset.session, row]));
    sessions.forEach((session) => {
      const row = existing.get(session.id) ?? createSessionRow();
      row.dataset.session = session.id;
      const summary = row.querySelector('[data-session-summary]');
      summary.textContent = [
        session.ip_masked,
        session.clientVersion,
        session.connectedAt,
        session.lastSeenAt,
        session.duration
      ].filter(Boolean).join(' · ');
      const pending = state.pendingKicks.has(session.id);
      const status = row.querySelector('[data-session-status]');
      status.textContent = pending
        ? (state.pollAttempts >= maxSessionPollAttempts ? '请求未确认，请重试' : '请求已发送')
        : '活跃';
      const kick = row.querySelector('[data-kick]') ?? row.querySelector('button');
      kick.dataset.kick = session.id;
      list.append(row);
      existing.delete(session.id);
    });

    let focusedRowRemoved = false;
    existing.forEach((row) => {
      if (row.contains(document.activeElement)) focusedRowRemoved = true;
      row.remove();
    });
    if (!focusedRowRemoved && focusedElement && list.contains(focusedElement)) focusedElement.focus();
    return focusedRowRemoved;
  }

  async function pollSessionsOnce() {
    const generation = ++state.sessionsGeneration;
    setSurface('sessions', 'loading');
    select('[data-sessions-retry]').hidden = true;
    announce('正在加载会话。');
    try {
      const data = await api('/api/me/sessions');
      if (generation !== state.sessionsGeneration || state.view !== 'sessions') return;
      const sessions = Array.isArray(data.sessions) ? data.sessions : [];
      const returned = new Set(sessions.map((session) => session.id));
      const previouslyPending = new Set(state.pendingKicks);
      sessions.forEach((session) => {
        if (session.pendingExit === true) state.pendingKicks.add(session.id);
      });
      state.pendingKicks.forEach((id) => {
        if (!returned.has(id)) state.pendingKicks.delete(id);
      });
      const confirmed = [...previouslyPending].filter((id) => !returned.has(id));

      if (state.pendingKicks.size) state.pollAttempts += 1;
      else state.pollAttempts = 0;
      const focusedRowRemoved = renderSessions(sessions);
      setSurface('sessions', sessions.length ? 'success' : 'empty');
      if (focusedRowRemoved) select('#sessions-title').focus();

      if (confirmed.length) {
        announce('会话已结束。');
      } else if (state.pendingKicks.size && state.pollAttempts >= maxSessionPollAttempts) {
        setSurface('sessions', 'error', '结束请求尚未确认，可重试。');
        select('[data-sessions-retry]').hidden = false;
        announce('结束请求尚未确认，可重试。');
      } else if (state.pendingKicks.size) {
        announce('正在等待会话退出。');
      } else {
        announce(sessions.length ? '会话列表已更新。' : '当前没有活跃会话。');
      }

      if (state.pendingKicks.size && state.pollAttempts < maxSessionPollAttempts) scheduleSessionPoll();
    } catch (error) {
      if (generation !== state.sessionsGeneration) return;
      if (!handleAuthenticatedError(error)) {
        setSurface('sessions', 'error', error.message);
        select('[data-sessions-retry]').hidden = false;
        announce(error.message);
      }
    }
  }

  function showDialog(dialog, focusSelector) {
    state.focusTarget = document.activeElement;
    if (typeof dialog.showModal === 'function') dialog.showModal();
    else dialog.open = true;
    select(focusSelector)?.focus();
  }

  function closeDialog(dialog, restoreFocus = true) {
    if (dialog.open && typeof dialog.close === 'function') dialog.close();
    else dialog.open = false;
    if (restoreFocus) state.focusTarget?.focus();
    state.focusTarget = null;
  }

  function scheduleSessionPoll() {
    clearTimeoutImpl(state.pollTimer);
    state.pollTimer = setTimeoutImpl(async () => {
      await pollSessionsOnce();
    }, 2000);
  }

  function clearKickError() {
    select('[data-kick-error]').hidden = true;
    setText('[data-kick-error-message]', '');
  }

  function showKickError(message) {
    setText('[data-kick-error-message]', message);
    select('[data-kick-error]').hidden = false;
    announce(message);
    select('[data-retry-kick]').focus();
  }

  async function confirmKick() {
    if (!state.kickTarget) return;
    const target = state.kickTarget;
    const generation = ++state.kickGeneration;
    clearKickError();
    try {
      await api('/api/me/sessions/kick', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ id: target })
      });
      if (generation !== state.kickGeneration || target !== state.kickTarget) return;
      state.pendingKicks.add(target);
      state.pollAttempts = 0;
      closeDialog(select('[data-kick-dialog]'));
      const current = sessionRow(target)?.querySelector('[data-session-status]');
      if (current) current.textContent = '请求已发送';
      announce('结束请求已发送。');
      scheduleSessionPoll();
    } catch (error) {
      if (generation !== state.kickGeneration) return;
      if (!handleAuthenticatedError(error)) showKickError(error.message);
    }
  }

  function cancelKick() {
    state.kickGeneration += 1;
    clearKickError();
    closeDialog(select('[data-kick-dialog]'));
  }

  function invalidateRequests() {
    state.overviewGeneration += 1;
    state.activityListGeneration += 1;
    state.detailGeneration += 1;
    state.sessionsGeneration += 1;
    state.loginGeneration += 1;
    state.kickGeneration += 1;
    state.passwordGeneration += 1;
  }

  function showLogin() {
    invalidateRequests();
    clearTimeoutImpl(state.pollTimer);
    [select('[data-kick-dialog]'), select('[data-password-dialog]')].forEach((dialog) => {
      if (dialog.open && typeof dialog.close === 'function') dialog.close();
      else dialog.open = false;
    });
    state.focusTarget = null;
    state.pendingKicks.clear();
    state.pollAttempts = 0;
    window.localStorage.clear();
    window.sessionStorage.clear();
    select('[data-app]').hidden = true;
    select('[data-view="login"]').hidden = false;
    select('[data-view="login"]').setAttribute('aria-busy', 'false');
    select('[data-bootstrap-loading]').hidden = true;
    select('[data-bootstrap-error]').hidden = true;
    select('[data-login-content]').hidden = false;
    select('[data-login-form]').setAttribute('aria-busy', 'false');
    select('[data-login-username]').focus();
  }

  function showBootstrapLoading() {
    select('[data-app]').hidden = true;
    select('[data-view="login"]').hidden = false;
    select('[data-view="login"]').setAttribute('aria-busy', 'true');
    select('[data-login-content]').hidden = true;
    select('[data-bootstrap-error]').hidden = true;
    select('[data-bootstrap-loading]').hidden = false;
  }

  function showBootstrapError(message) {
    select('[data-view="login"]').setAttribute('aria-busy', 'false');
    select('[data-bootstrap-loading]').hidden = true;
    select('[data-login-content]').hidden = true;
    setText('[data-bootstrap-error-message]', message);
    select('[data-bootstrap-error]').hidden = false;
    select('[data-bootstrap-retry]').focus();
  }

  async function submitPassword(event) {
    event.preventDefault();
    const generation = ++state.passwordGeneration;
    setText('[data-password-error]', '');
    select('[data-password-form]').setAttribute('aria-busy', 'true');
    try {
      const result = await api('/api/me/password', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          current: select('[data-password-current]').value,
          newPassword: select('[data-password-new]').value
        })
      });
      if (generation !== state.passwordGeneration) return;
      if (result.ok && result.reauthenticate) showLogin();
    } catch (error) {
      if (generation !== state.passwordGeneration) return;
      if (!handleAuthenticatedError(error)) setText('[data-password-error]', error.message);
    } finally {
      select('[data-password-form]').setAttribute('aria-busy', 'false');
    }
  }

  async function showAuthenticated(identity) {
    select('[data-view="login"]').hidden = true;
    select('[data-app]').hidden = false;
    select('[data-bootstrap-loading]').hidden = true;
    select('[data-bootstrap-error]').hidden = true;
    setText('[data-user-name]', identity.name || identity.username);
    paramsFromUrl();
    await selectCurrentView();
  }

  async function submitLogin(event) {
    event.preventDefault();
    const generation = ++state.loginGeneration;
    setText('[data-login-error]', '');
    select('[data-login-form]').setAttribute('aria-busy', 'true');
    try {
      const result = await api('/api/login', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          username: select('[data-login-username]').value,
          password: select('[data-login-password]').value,
          remember: select('[data-login-remember]').checked
        })
      });
      if (generation !== state.loginGeneration) return;
      if (result.ok) await showAuthenticated(result);
    } catch (error) {
      if (generation === state.loginGeneration) setText('[data-login-error]', error.message);
    } finally {
      select('[data-login-form]').setAttribute('aria-busy', 'false');
    }
  }

  async function selectCurrentView() {
    showView();
    if (state.view === 'overview') return loadOverview();
    if (state.view === 'activity') return loadActivities();
    if (state.view === 'sessions') return pollSessionsOnce();
    resetActivityDetail();
    announce('已打开安全设置。');
  }

  function addListeners() {
    document.querySelectorAll('[data-nav]').forEach((button) => button.addEventListener('click', () => {
      state.view = button.dataset.nav;
      state.activity = '';
      resetActivityDetail();
      writeUrl();
      selectCurrentView();
    }));
    select('[data-activity-type]').addEventListener('change', (event) => {
      state.type = event.target.value;
      state.activity = '';
      resetActivityDetail();
      writeUrl();
      loadActivities();
    });
    select('[data-activity-status]').addEventListener('change', (event) => {
      state.status = event.target.value;
      state.activity = '';
      resetActivityDetail();
      writeUrl();
      loadActivities();
    });
    select('[data-activity-list]').addEventListener('click', (event) => {
      const item = event.target.closest('[data-activity]');
      if (!item) return;
      state.activity = item.dataset.activity;
      updateActivitySelection();
      writeUrl();
      loadActivityDetail(state.activity);
    });
    select('[data-session-list]').addEventListener('click', (event) => {
      const trigger = event.target.closest('[data-kick]');
      if (!trigger) return;
      state.kickTarget = trigger.dataset.kick;
      state.kickGeneration += 1;
      clearKickError();
      showDialog(select('[data-kick-dialog]'), '[data-confirm-kick]');
    });
    select('[data-open-password-dialog]').addEventListener('click', () => {
      state.passwordGeneration += 1;
      setText('[data-password-error]', '');
      showDialog(select('[data-password-dialog]'), '[data-password-current]');
    });
    select('[data-confirm-kick]').addEventListener('click', confirmKick);
    select('[data-retry-kick]').addEventListener('click', confirmKick);
    select('[data-cancel-kick]').addEventListener('click', cancelKick);
    select('[data-kick-dialog]').addEventListener('cancel', (event) => {
      event.preventDefault();
      cancelKick();
    });
    select('[data-cancel-password]').addEventListener('click', () => {
      state.passwordGeneration += 1;
      closeDialog(select('[data-password-dialog]'));
    });
    select('[data-password-dialog]').addEventListener('cancel', (event) => {
      event.preventDefault();
      state.passwordGeneration += 1;
      closeDialog(select('[data-password-dialog]'));
    });
    select('[data-password-form]').addEventListener('submit', submitPassword);
    select('[data-login-form]').addEventListener('submit', submitLogin);
    select('[data-bootstrap-retry]').addEventListener('click', start);
    select('[data-overview-retry]').addEventListener('click', loadOverview);
    select('[data-activity-retry]').addEventListener('click', loadActivities);
    select('[data-detail-retry]').addEventListener('click', () => {
      if (state.activity) loadActivityDetail(state.activity);
    });
    select('[data-sessions-retry]').addEventListener('click', () => {
      state.pollAttempts = 0;
      pollSessionsOnce();
    });
    window.addEventListener('popstate', handlePopstate);
  }

  function handlePopstate() {
    paramsFromUrl();
    if (!state.activity) resetActivityDetail();
    selectCurrentView();
  }

  async function start() {
    if (!started) {
      addListeners();
      started = true;
    }
    const generation = ++state.bootstrapGeneration;
    showBootstrapLoading();
    try {
      const me = await api('/api/me');
      if (generation !== state.bootstrapGeneration) return;
      if (me.loggedIn === false) return showLogin();
      await showAuthenticated(me);
    } catch (error) {
      if (generation !== state.bootstrapGeneration) return;
      if (error instanceof PortalApiError && error.status === 401) showLogin();
      else showBootstrapError(error.message);
    }
  }

  function destroy() {
    invalidateRequests();
    state.bootstrapGeneration += 1;
    clearTimeoutImpl(state.pollTimer);
    window.removeEventListener('popstate', handlePopstate);
  }

  return { start, destroy, retryCurrentView: selectCurrentView, pollSessionsOnce };
}

function startBrowserPortal() {
  const portal = createPortal({ document: window.document, window });
  portal.start();
}

if (typeof window !== 'undefined' && typeof document !== 'undefined' && typeof process === 'undefined') {
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', startBrowserPortal, { once: true });
  else startBrowserPortal();
}

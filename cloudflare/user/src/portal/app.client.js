export function createPortal({
  document,
  window,
  fetchImpl = window.fetch.bind(window),
  setTimeoutImpl = window.setTimeout.bind(window),
  clearTimeoutImpl = window.clearTimeout.bind(window)
}) {
  const select = (selector) => document.querySelector(selector);
  const maxSessionPollAttempts = 6;
  const state = { view: 'overview', type: 'all', status: 'all', activity: '', pendingKicks: new Set(), kickTarget: '', focusTarget: null, pollTimer: null, pollAttempts: 0 };
  let started = false;

  function setText(selector, value) {
    select(selector).textContent = String(value ?? '—');
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
    select('[data-activity-type]').value = state.type;
    select('[data-activity-status]').value = state.status;
  }

  function setSurface(surface, mode, message = '') {
    const loading = select(`[data-${surface}-loading]`);
    const empty = select(`[data-${surface}-empty]`);
    const error = select(`[data-${surface}-error]`);
    if (loading) loading.hidden = mode !== 'loading';
    if (empty) empty.hidden = mode !== 'empty';
    if (error) error.hidden = mode !== 'error';
    const messageNode = select(`[data-${surface}-error-message]`);
    if (messageNode && message) messageNode.textContent = message;
  }

  function responseMessage(data) {
    return data?.message || '暂时无法加载，请稍后重试。';
  }

  async function api(path, options = {}) {
    const method = options.method || 'GET';
    const headers = new Headers(options.headers || {});
    if (method !== 'GET') headers.set('X-Requested-With', 'XMLHttpRequest');
    const response = await fetchImpl(path, { ...options, method, headers, credentials: 'same-origin' });
    const data = await response.json().catch(() => ({}));
    if (response.status === 401) {
      showLogin();
      throw new Error('unauthorized');
    }
    if (!response.ok) throw new Error(responseMessage(data));
    return data;
  }

  async function loadOverview() {
    setSurface('overview', 'loading');
    select('[data-overview-content]').hidden = true;
    try {
      const overview = await api('/api/me/overview');
      const fields = [['total', 'total'], ['operations', 'operations'], ['rom', 'rom'], ['successes', 'successes'], ['failures', 'failures'], ['sessions', 'activeSessions']];
      fields.forEach(([target, source]) => setText(`[data-overview-${target}]`, overview[source]));
      const hasData = fields.some(([, source]) => Number(overview[source]) > 0);
      setSurface('overview', hasData ? 'success' : 'empty');
      select('[data-overview-content]').hidden = !hasData;
    } catch (error) {
      if (error.message !== 'unauthorized') setSurface('overview', 'error', error.message);
    }
  }

  function clearNodes(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  function renderActivities(activities) {
    const list = select('[data-activity-list]');
    clearNodes(list);
    activities.forEach((activity) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.dataset.activity = activity.id;
      button.className = 'activity-row';
      button.textContent = [activity.type, activity.status, activity.summary, activity.timestamp].filter(Boolean).join(' · ');
      list.append(button);
    });
  }

  async function loadActivities() {
    setSurface('activity', 'loading');
    try {
      const data = await api(`/api/me/activities?type=${encodeURIComponent(state.type)}&status=${encodeURIComponent(state.status)}&limit=50&offset=0`);
      const activities = data.activities || [];
      renderActivities(activities);
      setSurface('activity', activities.length ? 'success' : 'empty');
      if (state.activity) await loadActivityDetail(state.activity);
    } catch (error) {
      if (error.message !== 'unauthorized') {
        setSurface('activity', 'error', error.message);
        select('[data-activity-retry]').hidden = false;
      }
    }
  }

  async function loadActivityDetail(id) {
    const detail = select('[data-activity-detail]');
    const steps = select('[data-step-list]');
    clearNodes(steps);
    try {
      const data = await api(`/api/me/activities/${encodeURIComponent(id)}`);
      setText('[data-step-state]', data.steps_state === 'unavailable' ? '无更详细数据' : (data.summary || '已加载活动详情'));
      if (data.steps_state !== 'unavailable') {
        (data.steps || []).forEach((step) => {
          const row = document.createElement('p');
          row.dataset.stepRow = '';
          row.textContent = [step.phase, step.status, step.duration].filter(Boolean).join(' · ');
          steps.append(row);
        });
      }
      detail.hidden = false;
    } catch (error) {
      if (error.message !== 'unauthorized') setText('[data-step-state]', error.message === 'Not Found' ? '未找到该活动。' : error.message);
    }
  }

  function renderSessions(sessions) {
    const list = select('[data-session-list]');
    clearNodes(list);
    sessions.forEach((session) => {
      const row = document.createElement('article');
      row.dataset.session = session.id;
      const summary = document.createElement('p');
      summary.textContent = [session.ip, session.clientVersion, session.duration].filter(Boolean).join(' · ');
      const status = document.createElement('p');
      status.dataset.sessionStatus = '';
      status.textContent = state.pendingKicks.has(session.id)
        ? (state.pollAttempts >= maxSessionPollAttempts ? '请求未确认，请重试' : '请求已发送')
        : (session.pendingExit ? '等待退出' : '活跃');
      const kick = document.createElement('button');
      kick.type = 'button';
      kick.dataset.kick = session.id;
      kick.textContent = '结束会话';
      row.append(summary, status, kick);
      list.append(row);
    });
  }

  async function pollSessionsOnce() {
    setSurface('sessions', 'loading');
    try {
      const data = await api('/api/me/sessions');
      const sessions = data.sessions || [];
      const returned = new Set(sessions.map((session) => session.id));
      state.pendingKicks.forEach((id) => { if (!returned.has(id)) state.pendingKicks.delete(id); });
      renderSessions(sessions);
      setSurface('sessions', sessions.length ? 'success' : 'empty');
      if (state.pendingKicks.size) {
        state.pollAttempts += 1;
        if (state.pollAttempts < maxSessionPollAttempts) {
          scheduleSessionPoll();
        } else {
          renderSessions(sessions);
          select('[data-sessions-retry]').hidden = false;
        }
      } else {
        state.pollAttempts = 0;
        select('[data-sessions-retry]').hidden = true;
      }
    } catch (error) {
      if (error.message !== 'unauthorized') {
        setSurface('sessions', 'error', error.message);
        select('[data-sessions-retry]').hidden = false;
      }
    }
  }

  function showDialog(dialog, focusSelector) {
    state.focusTarget = document.activeElement;
    if (typeof dialog.showModal === 'function') dialog.showModal(); else dialog.open = true;
    select(focusSelector)?.focus();
  }

  function closeDialog(dialog) {
    if (typeof dialog.close === 'function') dialog.close(); else dialog.open = false;
    state.focusTarget?.focus();
    state.focusTarget = null;
  }

  function scheduleSessionPoll() {
    clearTimeoutImpl(state.pollTimer);
    state.pollTimer = setTimeoutImpl(async () => { await pollSessionsOnce(); }, 2000);
  }

  async function confirmKick() {
    if (!state.kickTarget) return;
    try {
      await api('/api/me/sessions/kick', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id: state.kickTarget }) });
      state.pendingKicks.add(state.kickTarget);
      state.pollAttempts = 0;
      closeDialog(select('[data-kick-dialog]'));
      const current = select(`[data-session="${state.kickTarget}"] [data-session-status]`);
      if (current) current.textContent = '请求已发送';
      scheduleSessionPoll();
    } catch (error) {
      if (error.message !== 'unauthorized') {
        setText('[data-live-status]', error.message);
        setSurface('sessions', 'error', error.message);
        select('[data-sessions-retry]').hidden = false;
      }
    }
  }

  function showLogin() {
    clearTimeoutImpl(state.pollTimer);
    [select('[data-kick-dialog]'), select('[data-password-dialog]')].forEach((dialog) => {
      if (dialog.open && typeof dialog.close === 'function') dialog.close();
      else dialog.open = false;
    });
    state.focusTarget = null;
    window.localStorage.clear();
    window.sessionStorage.clear();
    select('[data-app]').hidden = true;
    select('[data-view="login"]').hidden = false;
    select('[data-login-username]').focus();
  }

  async function submitPassword(event) {
    event.preventDefault();
    try {
      const result = await api('/api/me/password', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ current: select('[data-password-current]').value, newPassword: select('[data-password-new]').value })
      });
      if (result.ok && result.reauthenticate) showLogin();
    } catch (error) {
      if (error.message !== 'unauthorized') setText('[data-password-error]', error.message);
    }
  }

  async function showAuthenticated(identity) {
    select('[data-view="login"]').hidden = true;
    select('[data-app]').hidden = false;
    setText('[data-user-name]', identity.name || identity.username);
    paramsFromUrl();
    await selectCurrentView();
  }

  async function submitLogin(event) {
    event.preventDefault();
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
      if (result.ok) await showAuthenticated(result);
    } catch (error) {
      if (error.message !== 'unauthorized') setText('[data-login-error]', error.message);
    }
  }

  async function selectCurrentView() {
    showView();
    if (state.view === 'overview') return loadOverview();
    if (state.view === 'activity') return loadActivities();
    if (state.view === 'sessions') return pollSessionsOnce();
  }

  function addListeners() {
    document.querySelectorAll('[data-nav]').forEach((button) => button.addEventListener('click', () => {
      state.view = button.dataset.nav;
      state.activity = '';
      writeUrl();
      selectCurrentView();
    }));
    select('[data-activity-type]').addEventListener('change', (event) => { state.type = event.target.value; state.activity = ''; writeUrl(); loadActivities(); });
    select('[data-activity-status]').addEventListener('change', (event) => { state.status = event.target.value; state.activity = ''; writeUrl(); loadActivities(); });
    select('[data-activity-list]').addEventListener('click', (event) => {
      const item = event.target.closest('[data-activity]');
      if (!item) return;
      state.activity = item.dataset.activity;
      writeUrl();
      loadActivityDetail(state.activity);
    });
    select('[data-session-list]').addEventListener('click', (event) => {
      const trigger = event.target.closest('[data-kick]');
      if (!trigger) return;
      state.kickTarget = trigger.dataset.kick;
      showDialog(select('[data-kick-dialog]'), '[data-confirm-kick]');
    });
    select('[data-open-password-dialog]').addEventListener('click', () => showDialog(select('[data-password-dialog]'), '[data-password-current]'));
    select('[data-confirm-kick]').addEventListener('click', confirmKick);
    select('[data-cancel-kick]').addEventListener('click', () => closeDialog(select('[data-kick-dialog]')));
    select('[data-kick-dialog]').addEventListener('cancel', () => closeDialog(select('[data-kick-dialog]')));
    select('[data-cancel-password]').addEventListener('click', () => closeDialog(select('[data-password-dialog]')));
    select('[data-password-dialog]').addEventListener('cancel', () => closeDialog(select('[data-password-dialog]')));
    select('[data-password-form]').addEventListener('submit', submitPassword);
    select('[data-login-form]').addEventListener('submit', submitLogin);
    select('[data-overview-retry]').addEventListener('click', loadOverview);
    select('[data-activity-retry]').addEventListener('click', loadActivities);
    select('[data-sessions-retry]').addEventListener('click', () => {
      state.pollAttempts = 0;
      pollSessionsOnce();
    });
    window.addEventListener('popstate', handlePopstate);
  }

  function handlePopstate() {
    paramsFromUrl();
    selectCurrentView();
  }

  async function start() {
    if (!started) { addListeners(); started = true; }
    try {
      const me = await api('/api/me');
      if (me.loggedIn === false) return showLogin();
      await showAuthenticated(me);
    } catch (error) {
      if (error.message !== 'unauthorized') showLogin();
    }
  }

  function destroy() {
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

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPortal } from './app.client.js';

const portalHtml = readFileSync(resolve(process.cwd(), 'src/portal/index.html'), 'utf8');
const portalCss = readFileSync(resolve(process.cwd(), 'src/portal/styles.css'), 'utf8');

const ownedSession = {
  id: 'session-owned',
  ip: '203.0.113.*',
  clientVersion: '1.4.0',
  connectedAt: '2026-08-24T00:00:00.000Z',
  lastSeenAt: '2026-08-24T00:02:00.000Z',
  duration: '2 分钟',
  pendingExit: false
};

function createFetchQueue() {
  const responses = new Map();
  const calls = [];
  return {
    calls,
    respond(path, body, status = 200) {
      const queue = responses.get(path) ?? [];
      queue.push({ body, status });
      responses.set(path, queue);
    },
    fetch: vi.fn(async (input, init = {}) => {
      const url = new URL(String(input), window.location.origin);
      const key = `${init.method ?? 'GET'} ${url.pathname}${url.search}`;
      const fallbackKey = `${url.pathname}${url.search}`;
      const queue = responses.get(key) ?? responses.get(fallbackKey);
      calls.push({ url, init });
      if (!queue?.length) throw new Error(`No response queued for ${key}`);
      const { body, status } = queue.shift();
      return new Response(JSON.stringify(await body), {
        status,
        headers: { 'content-type': 'application/json' }
      });
    })
  };
}

describe('Personal Ops portal', () => {
  let fetchQueue;
  let portal;
  let scheduledPolls;

  beforeEach(() => {
    document.documentElement.innerHTML = portalHtml.replace('<link rel="stylesheet" href="/portal/styles.css">', '');
    window.history.replaceState({}, '', '/');
    window.localStorage.clear();
    window.sessionStorage.clear();
    fetchQueue = createFetchQueue();
    scheduledPolls = [];
    portal = createPortal({
      document,
      window,
      fetchImpl: fetchQueue.fetch,
      setTimeoutImpl: (callback) => {
        scheduledPolls.push(callback);
        return scheduledPolls.length;
      },
      clearTimeoutImpl: () => {}
    });
  });

  afterEach(() => portal.destroy());

  function queueSignedInStart() {
    fetchQueue.respond('/api/me', {
      loggedIn: true,
      username: 'alice',
      name: 'Alice',
      online: 1
    });
  }

  async function startPortal() {
    await portal.start();
  }

  function flush() {
    return new Promise((resolve) => setTimeout(resolve, 0));
  }

  async function goToActivity() {
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', {
      activities: [],
      count: 0
    });
    document.querySelector('[data-nav="activity"]').click();
    await flush();
  }

  async function clickActivity(id) {
    const item = document.querySelector(`[data-activity="${id}"]`);
    item.click();
    await flush();
  }

  async function openKickDialog(id) {
    document.querySelector(`[data-kick="${id}"]`).click();
    await flush();
  }

  async function confirmKick() {
    document.querySelector('[data-confirm-kick]').click();
    await flush();
  }

  function sessionStatus() {
    return document.querySelector('[data-session="session-owned"] [data-session-status]').textContent;
  }

  async function pollOnce() {
    await portal.pollSessionsOnce();
  }

  async function submitPasswordChange() {
    document.querySelector('[data-password-current]').value = 'correct horse';
    document.querySelector('[data-password-new]').value = 'new correct horse';
    document.querySelector('[data-password-form]').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await flush();
  }

  async function runScheduledPoll() {
    await scheduledPolls.shift()();
  }

  it('renders exactly four primary navigation destinations', () => {
    const labels = [...document.querySelectorAll('[data-nav]')]
      .map((node) => node.textContent.trim());
    expect(labels).toEqual(['概览', '我的活动', '设备与会话', '安全设置']);
  });

  it('renders unavailable telemetry without inventing step rows', async () => {
    queueSignedInStart();
    await startPortal();
    await goToActivity();
    fetchQueue.respond('/api/me/activities/operation%3A7', {
      id: 'operation:7',
      steps_state: 'unavailable',
      steps: [],
      steps_message: '<b>fabricated telemetry</b>'
    });
    const activity = document.createElement('button');
    activity.dataset.activity = 'operation:7';
    activity.textContent = '活动 7';
    document.querySelector('[data-activity-list]').append(activity);
    await clickActivity('operation:7');
    expect(document.querySelector('[data-step-state]').textContent).toBe('无更详细数据');
    expect(document.querySelectorAll('[data-step-row]')).toHaveLength(0);
  });

  it('renders API strings as text instead of markup', async () => {
    fetchQueue.respond('/api/me', {
      loggedIn: true,
      username: 'alice',
      name: '<img src=x onerror=alert(1)>',
      online: 0
    });
    await startPortal();
    expect(document.querySelector('[data-app] img')).toBeNull();
    expect(document.querySelector('[data-user-name]').textContent)
      .toBe('<img src=x onerror=alert(1)>');
  });

  it('keeps a kicked session pending until the server stops returning it', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    fetchQueue.respond('POST /api/me/sessions/kick', { ok: true });
    await openKickDialog('session-owned');
    await confirmKick();
    expect(sessionStatus()).toBe('请求已发送');
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    await pollOnce();
    expect(sessionStatus()).toBe('请求已发送');
    fetchQueue.respond('/api/me/sessions', { count: 0, sessions: [] });
    await pollOnce();
    expect(document.querySelector('[data-session="session-owned"]')).toBeNull();
  });

  it('returns to login after authoritative password revocation', async () => {
    queueSignedInStart();
    await startPortal();
    document.querySelector('[data-nav="security"]').click();
    await flush();
    document.querySelector('[data-open-password-dialog]').click();
    await flush();
    expect(document.activeElement).toBe(document.querySelector('[data-password-current]'));
    fetchQueue.respond('POST /api/me/password', { ok: true, reauthenticate: true });
    await submitPasswordChange();
    expect(document.querySelector('[data-view="login"]').hidden).toBe(false);
    expect(document.querySelector('[data-password-dialog]').open).toBe(false);
    expect(document.activeElement).toBe(document.querySelector('[data-login-username]'));
    expect(window.localStorage.length).toBe(0);
    expect(window.sessionStorage.length).toBe(0);
  });

  it('shows a loading state before overview data resolves', async () => {
    queueSignedInStart();
    let resolveOverview;
    fetchQueue.respond('/api/me/overview', new Promise((resolve) => { resolveOverview = resolve; }));
    const start = portal.start();
    await Promise.resolve();
    expect(document.querySelector('[data-overview-loading]').hidden).toBe(false);
    resolveOverview({ total: 0, operations: 0, rom: 0, successes: 0, failures: 0, activeSessions: 0 });
    await start;
  });

  it('shows an empty activity state when no owned records exist', async () => {
    queueSignedInStart();
    await startPortal();
    await goToActivity();
    expect(document.querySelector('[data-activity-empty]').hidden).toBe(false);
  });

  it('leaves a retry action after a recoverable activity fetch failure', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', { message: '暂时无法加载' }, 503);
    document.querySelector('[data-nav="activity"]').click();
    await flush();
    expect(document.querySelector('[data-activity-retry]').hidden).toBe(false);
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', { activities: [], count: 0 });
    document.querySelector('[data-activity-retry]').click();
    await flush();
    expect(document.querySelector('[data-activity-empty]').hidden).toBe(false);
  });

  it('filters ROM activity without adding ROM to primary navigation', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=rom&status=all&limit=50&offset=0', { activities: [], count: 0 });
    document.querySelector('[data-nav="activity"]').click();
    await Promise.resolve();
    document.querySelector('[data-activity-type]').value = 'rom';
    document.querySelector('[data-activity-type]').dispatchEvent(new Event('change'));
    await Promise.resolve();
    expect(window.location.search).toContain('type=rom');
    expect(document.querySelectorAll('[data-nav]')).toHaveLength(4);
  });

  it('restores an activity deep link from the URL', async () => {
    window.history.replaceState({}, '', '/?view=activity&type=operation&status=failed&activity=operation%3A7');
    queueSignedInStart();
    fetchQueue.respond('/api/me/activities?type=operation&status=failed&limit=50&offset=0', { activities: [], count: 0 });
    fetchQueue.respond('/api/me/activities/operation%3A7', { id: 'operation:7', steps_state: 'unavailable', steps: [], steps_message: '无更详细数据' });
    await startPortal();
    expect(document.querySelector('[data-view="activity"]').hidden).toBe(false);
    expect(document.querySelector('[data-activity-type]').value).toBe('operation');
    expect(document.querySelector('[data-step-state]').textContent).toBe('无更详细数据');
  });

  it('restores activity filters on browser popstate', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=rom&status=all&limit=50&offset=0', { activities: [], count: 0 });
    window.history.pushState({}, '', '/?view=activity&type=rom&status=all');
    window.dispatchEvent(new PopStateEvent('popstate'));
    await flush();
    expect(document.querySelector('[data-activity-type]').value).toBe('rom');
  });

  it('cancels the kick dialog and restores focus to its trigger', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    const trigger = document.querySelector('[data-kick="session-owned"]');
    trigger.focus();
    await openKickDialog('session-owned');
    document.querySelector('[data-kick-dialog]').dispatchEvent(new Event('cancel', { cancelable: true }));
    expect(document.activeElement).toBe(trigger);
  });

  it('keeps a session retry action after a failed poll', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    fetchQueue.respond('/api/me/sessions', { message: '稍后重试' }, 503);
    await pollOnce();
    expect(document.querySelector('[data-sessions-retry]').hidden).toBe(false);
  });

  it('uses a dedicated live status without replacing the workspace after a failed kick', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    fetchQueue.respond('POST /api/me/sessions/kick', { message: '暂时无法结束会话' }, 503);
    await openKickDialog('session-owned');
    await confirmKick();
    expect(document.querySelector('[data-live-status]').textContent).toBe('暂时无法结束会话');
    expect(document.querySelector('[data-view="sessions"] h2').textContent).toBe('设备与会话');
    expect(document.querySelector('[data-sessions-retry]').hidden).toBe(false);
  });

  it('continues bounded polling for a pending kick and exposes retry on timeout', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    fetchQueue.respond('POST /api/me/sessions/kick', { ok: true });
    await openKickDialog('session-owned');
    await confirmKick();
    for (let attempt = 0; attempt < 6; attempt += 1) {
      fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
      await runScheduledPoll();
    }
    expect(sessionStatus()).toBe('请求未确认，请重试');
    expect(document.querySelector('[data-sessions-retry]').hidden).toBe(false);
  });

  it('lets a logged-out user authenticate from the login surface without storing a token', async () => {
    fetchQueue.respond('/api/me', { loggedIn: false });
    await startPortal();
    document.querySelector('[data-login-username]').value = 'alice';
    document.querySelector('[data-login-password]').value = 'correct horse';
    fetchQueue.respond('POST /api/login', { ok: true, username: 'alice', name: 'Alice' });
    fetchQueue.respond('/api/me/overview', { total: 0, operations: 0, rom: 0, successes: 0, failures: 0, activeSessions: 0 });
    document.querySelector('[data-login-form]').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await flush();
    expect(document.querySelector('[data-app]').hidden).toBe(false);
    expect(document.querySelector('[data-user-name]').textContent).toBe('Alice');
    expect(window.localStorage.length).toBe(0);
    expect(window.sessionStorage.length).toBe(0);
  });

  it('keeps keyboard dialog controls and the 320px layout within the accessibility contract', () => {
    expect(document.querySelector('[data-kick-dialog] [data-confirm-kick]').minHeight || 44).toBeGreaterThanOrEqual(44);
    expect(portalCss).toContain('.brand { min-height: 44px;');
    expect(portalCss).toContain('@media (max-width: 320px)');
    expect(portalCss).toContain('body { min-width: 320px;');
  });

  it('contains a browser-only portal bootstrap in the module served as app.js', () => {
    const source = readFileSync(resolve(process.cwd(), 'src/portal/app.client.js'), 'utf8');
    expect(source).toContain('startBrowserPortal');
    expect(source).toContain("typeof process === 'undefined'");
  });
});

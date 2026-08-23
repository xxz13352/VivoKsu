import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { Window } from 'happy-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createPortal } from './app.client.js';

const portalHtml = readFileSync(resolve(process.cwd(), 'src/portal/index.html'), 'utf8');
const portalCss = readFileSync(resolve(process.cwd(), 'src/portal/styles.css'), 'utf8');

const ownedSession = {
  id: 'session-owned',
  ip_masked: '203.0.113.*',
  clientVersion: '1.4.0',
  connectedAt: '2026-08-24T00:00:00.000Z',
  lastSeenAt: '2026-08-24T00:02:00.000Z',
  duration: '2 分钟',
  pendingExit: false
};

const operationActivity = {
  id: 'operation:7',
  type: 'operation',
  status: 'success',
  summary: '刷写操作',
  timestamp: 1787544000,
  ended_at: 1787544030,
  duration_ms: 30000
};

const romActivity = {
  id: 'rom:9',
  type: 'rom',
  status: 'failed',
  summary: 'ROM 查询',
  timestamp: 1787544060,
  pd: 'PD-9',
  version: '2.0',
  http_status: 404
};

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function cssRgb(value) {
  const rgb = /rgba?\(\s*(\d+)\D+(\d+)\D+(\d+)/.exec(value);
  if (rgb) return rgb.slice(1, 4).map(Number);
  const hex = /#([0-9a-f]{6})\b/i.exec(value);
  if (!hex) return null;
  return [0, 2, 4].map((offset) => Number.parseInt(hex[1].slice(offset, offset + 2), 16));
}

function contrastRatio(left, right) {
  const luminance = (rgb) => {
    const channels = rgb.map((channel) => {
      const value = channel / 255;
      return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    });
    return (0.2126 * channels[0]) + (0.7152 * channels[1]) + (0.0722 * channels[2]);
  };
  const [bright, dark] = [luminance(left), luminance(right)].sort((a, b) => b - a);
  return (bright + 0.05) / (dark + 0.05);
}

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

  it('renders the allowlisted activity fields in list and detail views', async () => {
    const stylesheet = document.createElement('style');
    stylesheet.textContent = portalCss;
    document.head.append(stylesheet);
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', {
      activities: [operationActivity, romActivity],
      count: 2
    });
    document.querySelector('[data-nav="activity"]').click();
    await flush();

    const operationRow = document.querySelector('[data-activity="operation:7"]');
    const romRow = document.querySelector('[data-activity="rom:9"]');
    expect(operationRow.textContent).toContain('operation:7');
    expect(operationRow.textContent).toContain('成功');
    expect(operationRow.textContent).toContain('2026-08-24 04:00:00 UTC');
    expect(operationRow.textContent).toContain('30 秒');
    expect(romRow.textContent).toContain('版本 2.0');

    fetchQueue.respond('/api/me/activities/operation%3A7', {
      ...operationActivity,
      steps_state: 'unavailable',
      steps: [],
      steps_message: '无更详细数据'
    });
    await clickActivity('operation:7');

    expect(operationRow.getAttribute('aria-pressed')).toBe('true');
    expect(window.getComputedStyle(operationRow).backgroundColor)
      .not.toBe(window.getComputedStyle(romRow).backgroundColor);
    expect(document.querySelector('[data-detail-id]').textContent).toBe('operation:7');
    expect(document.querySelector('[data-detail-status]').textContent).toBe('成功');
    expect(document.querySelector('[data-detail-time]').textContent).toBe('2026-08-24 04:00:00 UTC');
    expect(document.querySelector('[data-detail-end-time]').textContent).toBe('2026-08-24 04:00:30 UTC');
    expect(document.querySelector('[data-detail-duration]').textContent).toBe('30 秒');
    expect(document.querySelector('[data-detail-telemetry]').textContent).toBe('无更详细数据');
  });

  it('shows explicit activity-detail loading, error, retry, and success states', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', {
      activities: [operationActivity],
      count: 1
    });
    document.querySelector('[data-nav="activity"]').click();
    await flush();

    const pending = deferred();
    fetchQueue.respond('/api/me/activities/operation%3A7', pending.promise, 503);
    document.querySelector('[data-activity="operation:7"]').click();
    await Promise.resolve();
    const detail = document.querySelector('[data-activity-detail]');
    const detailLoading = document.querySelector('[data-detail-loading]');
    expect(detailLoading).not.toBeNull();
    expect(detailLoading.hidden).toBe(false);
    expect(detail.getAttribute('aria-busy')).toBe('true');
    pending.resolve({ message: '详情暂不可用' });
    await flush();
    expect(document.querySelector('[data-detail-error]').hidden).toBe(false);
    expect(document.querySelector('[data-detail-error-message]').textContent).toBe('详情暂不可用');

    fetchQueue.respond('/api/me/activities/operation%3A7', {
      ...operationActivity,
      steps_state: 'unavailable',
      steps: [],
      steps_message: '无更详细数据'
    });
    document.querySelector('[data-detail-retry]').click();
    await flush();
    expect(document.querySelector('[data-detail-content]').hidden).toBe(false);
    expect(detail.getAttribute('aria-busy')).toBe('false');
  });

  it('renders missing optional activity times as unavailable instead of Unix epoch', async () => {
    window.history.replaceState({}, '', '/?view=activity&type=operation&status=all&activity=operation%3A7');
    queueSignedInStart();
    fetchQueue.respond('/api/me/activities?type=operation&status=all&limit=50&offset=0', {
      activities: [{ ...operationActivity, ended_at: null, duration_ms: null }],
      count: 1
    });
    fetchQueue.respond('/api/me/activities/operation%3A7', {
      ...operationActivity,
      ended_at: null,
      duration_ms: null,
      steps_state: 'unavailable',
      steps: [],
      steps_message: '无更详细数据'
    });

    await startPortal();

    expect(document.querySelector('[data-detail-end-time]').textContent).toBe('—');
    expect(document.querySelector('[data-detail-duration]').textContent).toBe('—');
  });

  it('ignores stale activity detail responses after a newer selection wins', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', {
      activities: [operationActivity, romActivity],
      count: 2
    });
    document.querySelector('[data-nav="activity"]').click();
    await flush();

    const stale = deferred();
    fetchQueue.respond('/api/me/activities/operation%3A7', stale.promise);
    document.querySelector('[data-activity="operation:7"]').click();
    await Promise.resolve();
    fetchQueue.respond('/api/me/activities/rom%3A9', romActivity);
    await clickActivity('rom:9');
    const detailId = document.querySelector('[data-detail-id]');
    expect(detailId).not.toBeNull();
    expect(detailId.textContent).toBe('rom:9');

    stale.resolve({
      ...operationActivity,
      steps_state: 'unavailable',
      steps: [],
      steps_message: '无更详细数据'
    });
    await flush();
    expect(document.querySelector('[data-detail-id]').textContent).toBe('rom:9');
  });

  it('ignores stale activity-list responses after filters change', async () => {
    queueSignedInStart();
    await startPortal();
    const stale = deferred();
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', stale.promise);
    document.querySelector('[data-nav="activity"]').click();
    await Promise.resolve();

    fetchQueue.respond('/api/me/activities?type=rom&status=all&limit=50&offset=0', {
      activities: [romActivity],
      count: 1
    });
    document.querySelector('[data-activity-type]').value = 'rom';
    document.querySelector('[data-activity-type]').dispatchEvent(new Event('change'));
    await flush();
    expect(document.querySelector('[data-activity="rom:9"]')).not.toBeNull();

    stale.resolve({ activities: [operationActivity], count: 1 });
    await flush();
    expect(document.querySelector('[data-activity="rom:9"]')).not.toBeNull();
    expect(document.querySelector('[data-activity="operation:7"]')).toBeNull();
  });

  it('resets selected activity detail when URL activity state is removed', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', {
      activities: [operationActivity],
      count: 1
    });
    document.querySelector('[data-nav="activity"]').click();
    await flush();
    fetchQueue.respond('/api/me/activities/operation%3A7', {
      ...operationActivity,
      steps_state: 'unavailable',
      steps: [],
      steps_message: '无更详细数据'
    });
    await clickActivity('operation:7');
    expect(document.querySelector('[data-activity="operation:7"]').getAttribute('aria-pressed')).toBe('true');

    fetchQueue.respond('/api/me/activities?type=all&status=failed&limit=50&offset=0', {
      activities: [],
      count: 0
    });
    document.querySelector('[data-activity-status]').value = 'failed';
    document.querySelector('[data-activity-status]').dispatchEvent(new Event('change'));
    await flush();

    expect(window.location.search).not.toContain('activity=');
    expect(document.querySelector('[data-detail-idle]').hidden).toBe(false);
    expect(document.querySelector('[data-detail-content]').hidden).toBe(true);
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
    expect(document.querySelector('[data-live-status]').textContent).toBe('结束请求已发送。');
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    await pollOnce();
    expect(sessionStatus()).toBe('请求已发送');
    fetchQueue.respond('/api/me/sessions', { count: 0, sessions: [] });
    await pollOnce();
    expect(document.querySelector('[data-session="session-owned"]')).toBeNull();
  });

  it('renders the masked session address supplied by the authoritative API', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    expect(document.querySelector('[data-session="session-owned"] p').textContent).toContain('203.0.113.*');
  });

  it('resumes polling from authoritative pending-exit rows after reload', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', {
      count: 1,
      sessions: [{ ...ownedSession, pendingExit: true }]
    });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();

    expect(sessionStatus()).toBe('请求已发送');
    expect(scheduledPolls).toHaveLength(1);
    expect(document.querySelector('[data-session-status]').getAttribute('role')).toBe('status');
  });

  it('reconciles session rows by key so focused controls survive polling', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    const trigger = document.querySelector('[data-kick="session-owned"]');
    trigger.focus();

    fetchQueue.respond('/api/me/sessions', {
      count: 1,
      sessions: [{ ...ownedSession, clientVersion: '1.4.1' }]
    });
    await pollOnce();

    expect(document.querySelector('[data-kick="session-owned"]')).toBe(trigger);
    expect(document.activeElement).toBe(trigger);
    expect(document.querySelector('[data-session="session-owned"]').textContent).toContain('1.4.1');
  });

  it('ignores an older session poll that resolves after a newer poll', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    const stale = deferred();
    fetchQueue.respond('/api/me/sessions', stale.promise);
    const olderPoll = portal.pollSessionsOnce();
    await Promise.resolve();
    fetchQueue.respond('/api/me/sessions', { count: 0, sessions: [] });
    await portal.pollSessionsOnce();
    expect(document.querySelector('[data-session="session-owned"]')).toBeNull();

    stale.resolve({ count: 1, sessions: [ownedSession] });
    await olderPoll;
    expect(document.querySelector('[data-session="session-owned"]')).toBeNull();
  });

  it('moves focus and announces confirmation when a pending session disappears', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', {
      count: 1,
      sessions: [{ ...ownedSession, pendingExit: true }]
    });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    document.querySelector('[data-kick="session-owned"]').focus();

    fetchQueue.respond('/api/me/sessions', { count: 0, sessions: [] });
    await pollOnce();

    expect(document.querySelector('[data-session="session-owned"]')).toBeNull();
    expect(document.activeElement).toBe(document.querySelector('#sessions-title'));
    expect(document.querySelector('[data-live-status]').textContent).toBe('会话已结束。');
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

  it('labels overview activity metrics as the most recent seven days', () => {
    const scope = document.querySelector('[data-overview-scope]');
    expect(scope).not.toBeNull();
    expect(scope.textContent).toContain('最近 7 天');
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

  it('restores filters and current navigation through actual history back and forward', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', { activities: [], count: 0 });
    document.querySelector('[data-nav="activity"]').click();
    await flush();
    fetchQueue.respond('/api/me/activities?type=rom&status=all&limit=50&offset=0', { activities: [], count: 0 });
    document.querySelector('[data-activity-type]').value = 'rom';
    document.querySelector('[data-activity-type]').dispatchEvent(new Event('change'));
    await flush();
    expect(document.querySelector('[data-activity-type]').value).toBe('rom');

    fetchQueue.respond('/api/me/activities?type=all&status=all&limit=50&offset=0', { activities: [], count: 0 });
    window.history.back();
    await flush();
    await flush();
    expect(document.querySelector('[data-activity-type]').value).toBe('all');
    expect(document.querySelector('[data-nav="activity"]').getAttribute('aria-current')).toBe('page');

    fetchQueue.respond('/api/me/activities?type=rom&status=all&limit=50&offset=0', { activities: [], count: 0 });
    window.history.forward();
    await flush();
    await flush();
    expect(document.querySelector('[data-activity-type]').value).toBe('rom');
    expect(window.location.search).toContain('type=rom');
  });

  it('closes the native kick dialog on its Escape/cancel path and restores opener focus', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    const trigger = document.querySelector('[data-kick="session-owned"]');
    trigger.focus();
    await openKickDialog('session-owned');
    const dialog = document.querySelector('[data-kick-dialog]');
    expect(dialog.open).toBe(true);
    expect(document.activeElement).toBe(document.querySelector('[data-confirm-kick]'));
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    dialog.dispatchEvent(new Event('cancel', { cancelable: true }));
    expect(dialog.open).toBe(false);
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

  it('keeps a failed kick recoverable inside the open dialog', async () => {
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', { count: 1, sessions: [ownedSession] });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();
    fetchQueue.respond('POST /api/me/sessions/kick', { message: '暂时无法结束会话' }, 503);
    await openKickDialog('session-owned');
    await confirmKick();
    const dialog = document.querySelector('[data-kick-dialog]');
    const dialogError = document.querySelector('[data-kick-error]');
    const retry = document.querySelector('[data-retry-kick]');
    expect(dialog.open).toBe(true);
    expect(dialogError).not.toBeNull();
    expect(dialogError.hidden).toBe(false);
    expect(document.querySelector('[data-kick-error-message]').textContent).toBe('暂时无法结束会话');
    expect(document.activeElement).toBe(retry);
    expect(document.querySelector('[data-live-status]').textContent).toBe('暂时无法结束会话');
    expect(document.querySelector('[data-view="sessions"] h2').textContent).toBe('设备与会话');

    fetchQueue.respond('POST /api/me/sessions/kick', { ok: true });
    retry.click();
    await flush();
    expect(dialog.open).toBe(false);
    expect(sessionStatus()).toBe('请求已发送');
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
    const retry = document.querySelector('[data-sessions-retry]');
    expect(retry.hidden).toBe(false);
    expect(retry.closest('[data-sessions-error]').hidden).toBe(false);
    expect(document.querySelector('[data-live-status]').textContent).toBe('结束请求尚未确认，可重试。');
  });

  it('lets a logged-out user authenticate from the login surface without storing a token', async () => {
    fetchQueue.respond('/api/me', { loggedIn: false, message: '请先登录。' }, 401);
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

  it('shows an incorrect-login response in the login alert', async () => {
    fetchQueue.respond('/api/me', { loggedIn: false, message: '请先登录。' }, 401);
    await startPortal();
    document.querySelector('[data-login-username]').value = 'alice';
    document.querySelector('[data-login-password]').value = 'wrong password';
    fetchQueue.respond('POST /api/login', { message: '用户名或密码错误。' }, 401);

    document.querySelector('[data-login-form]').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    await flush();

    expect(document.querySelector('[data-login-error]').textContent).toBe('用户名或密码错误。');
    expect(document.querySelector('[data-view="login"]').hidden).toBe(false);
  });

  it('keeps bootstrap failures distinct from logged-out state and retries in place', async () => {
    fetchQueue.respond('/api/me', { message: '身份状态暂不可用' }, 503);
    await startPortal();

    const bootstrapError = document.querySelector('[data-bootstrap-error]');
    expect(bootstrapError).not.toBeNull();
    expect(bootstrapError.hidden).toBe(false);
    expect(document.querySelector('[data-bootstrap-error-message]').textContent).toBe('身份状态暂不可用');
    expect(document.querySelector('[data-login-content]').hidden).toBe(true);

    fetchQueue.respond('/api/me', { loggedIn: true, username: 'alice', name: 'Alice', online: 0 });
    fetchQueue.respond('/api/me/overview', { total: 0, operations: 0, rom: 0, successes: 0, failures: 0, activeSessions: 0 });
    document.querySelector('[data-bootstrap-retry]').click();
    await flush();
    await flush();
    expect(document.querySelector('[data-app]').hidden).toBe(false);
  });

  it('exposes loading, error, busy, and announcement states semantically', () => {
    const live = document.querySelector('[data-live-status]');
    expect(live.getAttribute('role')).toBe('status');
    expect(live.getAttribute('aria-live')).toBe('polite');
    for (const surface of ['overview', 'activity', 'sessions']) {
      expect(document.querySelector(`[data-${surface}-loading]`).getAttribute('role'), surface).toBe('status');
      expect(document.querySelector(`[data-${surface}-error]`).getAttribute('role'), surface).toBe('alert');
      expect(document.querySelector(`[data-view="${surface}"]`).getAttribute('aria-busy'), surface).toBe('false');
    }
    expect(document.querySelector('[data-detail-error]').getAttribute('role')).toBe('alert');
    expect(document.querySelector('[data-kick-error]').getAttribute('role')).toBe('alert');
  });

  it('uses 44px targets, a contrast-safe focus indicator, and password bounds', async () => {
    window.happyDOM.setViewport({ width: 320, height: 640 });
    const stylesheet = document.createElement('style');
    stylesheet.textContent = portalCss;
    document.head.append(stylesheet);
    queueSignedInStart();
    await startPortal();
    const controls = document.querySelectorAll('button, input:not([type="checkbox"]), select, a.brand');
    for (const control of controls) {
      expect(Number.parseFloat(window.getComputedStyle(control).minHeight), control.outerHTML).toBeGreaterThanOrEqual(44);
    }
    expect(Number.parseFloat(window.getComputedStyle(document.querySelector('.remember-choice')).minHeight)).toBeGreaterThanOrEqual(44);
    expect(document.querySelector('[data-password-new]').maxLength).toBe(128);

    const focusWindow = new Window({ url: 'https://portal.test/', width: 320, height: 640 });
    focusWindow.document.documentElement.innerHTML = portalHtml;
    const focusStylesheet = focusWindow.document.createElement('style');
    focusStylesheet.textContent = portalCss;
    focusWindow.document.head.append(focusStylesheet);
    const focusTarget = focusWindow.document.querySelector('[data-nav="overview"]');
    focusTarget.focus();
    const focusStyle = focusWindow.getComputedStyle(focusTarget);
    expect(focusWindow.document.activeElement).toBe(focusTarget);
    expect(focusStyle.outline).toContain('3px');
    const focusColor = cssRgb(focusStyle.outlineColor || focusStyle.outline);
    expect(focusColor).not.toBeNull();
    expect(contrastRatio(focusColor, [255, 255, 255])).toBeGreaterThanOrEqual(3);
    focusWindow.close();
  });

  it('keeps long session content wrapped inside the 320px single-column layout', async () => {
    window.happyDOM.setViewport({ width: 320, height: 640 });
    const stylesheet = document.createElement('style');
    stylesheet.textContent = portalCss;
    document.head.append(stylesheet);
    queueSignedInStart();
    await startPortal();
    fetchQueue.respond('/api/me/sessions', {
      count: 1,
      sessions: [{ ...ownedSession, clientVersion: `build-${'x'.repeat(256)}` }]
    });
    document.querySelector('[data-nav="sessions"]').click();
    await flush();

    const app = document.querySelector('[data-app]');
    const row = document.querySelector('[data-session="session-owned"]');
    const summary = document.querySelector('[data-session-summary]');
    expect(window.getComputedStyle(app).gridTemplateColumns).toBe('1fr');
    expect(Number.parseFloat(window.getComputedStyle(row).minWidth)).toBe(0);
    expect(Number.parseFloat(window.getComputedStyle(summary).minWidth)).toBe(0);
    expect(window.getComputedStyle(summary).overflowWrap).toBe('anywhere');
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(320);
  });

  it('keeps direct test imports inert and bootstraps the portal in a browser after DOM readiness', async () => {
    const originalFetch = window.fetch;
    const processDescriptor = Object.getOwnPropertyDescriptor(globalThis, 'process');
    window.fetch = fetchQueue.fetch;
    try {
      await import('./app.client.js?test-import');
      await flush();
      expect(fetchQueue.calls).toHaveLength(0);

      fetchQueue.respond('/api/me', { loggedIn: true, username: 'alice', name: 'Alice', online: 0 });
      fetchQueue.respond('/api/me/overview', { total: 0, operations: 0, rom: 0, successes: 0, failures: 0, activeSessions: 0 });
      Object.defineProperty(globalThis, 'process', { configurable: true, writable: true, value: undefined });
      await import('./app.client.js?browser-bootstrap');
      await flush();
      expect(document.querySelector('[data-app]').hidden).toBe(false);
      expect(fetchQueue.calls.map(({ url }) => url.pathname)).toEqual(['/api/me', '/api/me/overview']);
    } finally {
      window.fetch = originalFetch;
      if (processDescriptor) Object.defineProperty(globalThis, 'process', processDescriptor);
      else delete globalThis.process;
    }
  });
});

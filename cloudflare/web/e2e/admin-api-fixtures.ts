import type { Page, Request as PlaywrightRequest, Route } from "@playwright/test";

export const adminUser = "operator";

export function adminMe(loggedIn = true) {
  return loggedIn
    ? { loggedIn: true, username: adminUser }
    : { loggedIn: false };
}

export const loginSuccess = { ok: true, username: adminUser };
export const logoutSuccess = { ok: true };

export function legacyError(message: string) {
  return { error: message };
}

export const task12RunId = "v2:019d9c40-7b3c-7000-8000-000000000002";
export const task12EventId = "019d9c40-7b3c-7000-8000-000000000003";
export const task12MaliciousText = '<img src=x onerror="globalThis.pwned=true">危险文本</pre><script>bad()</script>';
export const task12LongEvidence = `${task12MaliciousText}-${"very-long-output-segment-".repeat(80)}`;

export interface Task12RequestRecord {
  method: string;
  pathname: string;
  headers: Record<string, string>;
  body: string | null;
}

export interface Task12ApiState {
  authenticated: boolean;
  requests: Task12RequestRecord[];
  unmocked: string[];
  mutationStatus: Partial<Record<"deleteVersion" | "rotateToken" | "deleteUser" | "kickSession", number>>;
  versions: Array<Record<string, unknown>>;
  users: Array<Record<string, unknown>>;
  sessions: Array<Record<string, unknown>>;
}

export function createTask12ApiState(overrides: Partial<Task12ApiState> = {}): Task12ApiState {
  return {
    authenticated: true,
    requests: [],
    unmocked: [],
    mutationStatus: {},
    versions: [
      { id: 9, version: "2.0.0", min_version: "1.0.0", enabled: 1 },
      { id: 10, version: task12MaliciousText, min_version: "1.0.0", enabled: 0 },
    ],
    users: [
      { id: 7, username: "alice", name: task12MaliciousText, enabled: 1, banned: 0 },
      { id: 8, username: "bob", name: "Bob", enabled: 1, banned: 0 },
    ],
    sessions: [
      { session_id: "session-alice-001", user_id: 7, username: "alice", name: "Alice", client_version: "3.10.4", ip: "203.0.113.7" },
      { session_id: "session-bob-002", user_id: 8, username: "bob", name: "Bob", client_version: "3.10.4", ip: "203.0.113.8" },
    ],
    ...overrides,
  };
}

export async function installTask12Api(page: Page, state: Task12ApiState): Promise<void> {
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const pathname = url.pathname;
    state.requests.push(requestRecord(request, pathname));

    if (pathname === "/api/me" && request.method() === "GET") return route.fulfill({ json: adminMe(state.authenticated) });
    if (pathname === "/api/login" && request.method() === "POST") {
      state.authenticated = true;
      return route.fulfill({ json: loginSuccess });
    }
    if (pathname === "/api/logout" && request.method() === "POST") {
      state.authenticated = false;
      return route.fulfill({ json: logoutSuccess });
    }
    if (pathname === "/api/change-password" && request.method() === "POST") {
      return route.fulfill({ json: { ok: true } });
    }
    if (pathname === "/api/usage-logs/v2/overview" && request.method() === "GET") return route.fulfill({ json: {
      totals: { api_users: state.users.length, online_sessions: state.sessions.length, operations: 12, failed: 1 },
      trend: [],
      recent_failures: [],
    } });
    if (pathname === "/api/app-versions/summary" && request.method() === "GET") return route.fulfill({ json: {
      current_version: "2.0.0", minimum_version: "1.0.0", supported_versions: ["2.0.0"], today_426: 0,
    } });
    if (pathname === "/api/app-versions" && request.method() === "GET") {
      return route.fulfill({ json: { versions: state.versions } });
    }
    const versionMatch = pathname.match(/^\/api\/app-versions\/(\d+)$/);
    if (versionMatch && request.method() === "DELETE") {
      const status = state.mutationStatus.deleteVersion ?? 200;
      if (status !== 200) return errorResponse(route, status);
      state.versions = state.versions.filter((version) => String(version.id) !== versionMatch[1]);
      return route.fulfill({ json: { ok: true } });
    }
    if (pathname === "/api/users" && request.method() === "GET") {
      return route.fulfill({ json: { users: state.users } });
    }
    const rotateMatch = pathname.match(/^\/api\/users\/(\d+)\/rotate-token$/);
    if (rotateMatch && request.method() === "POST") {
      const status = state.mutationStatus.rotateToken ?? 200;
      if (status !== 200) return errorResponse(route, status);
      return route.fulfill({ json: { ok: true, token: "task12-one-time-token" } });
    }
    const userMatch = pathname.match(/^\/api\/users\/(\d+)$/);
    if (userMatch && request.method() === "DELETE") {
      const status = state.mutationStatus.deleteUser ?? 200;
      if (status !== 200) return errorResponse(route, status);
      state.users = state.users.filter((user) => String(user.id) !== userMatch[1]);
      return route.fulfill({ json: { ok: true } });
    }
    if (userMatch && request.method() === "PUT") return route.fulfill({ json: { ok: true } });
    if (pathname === "/api/online" && request.method() === "GET") {
      return route.fulfill({ json: { count: state.sessions.length, sessions: state.sessions } });
    }
    if (pathname === "/api/online/kick" && request.method() === "POST") {
      const status = state.mutationStatus.kickSession ?? 200;
      if (status !== 200) return errorResponse(route, status);
      return route.fulfill({ json: { ok: true, affected: 1 } });
    }
    if (pathname === "/api/rom-logs/v2" && request.method() === "GET") return route.fulfill({ json: {
      items: [
        { id: 1, user_name: "Alice", pd: "PD1", version: "1.0", status: 200, url: "https://example.test/rom.zip", failure_reason: null, detail_unavailable_reason: null },
        { id: 4, user_name: "Alice Duplicate", pd: "PD1", version: "1.0", status: 200, url: "https://example.test/rom.zip", failure_reason: null, detail_unavailable_reason: null },
        { id: 2, user_name: "Unsafe", pd: task12MaliciousText, version: "1.0", status: 500, url: "javascript:alert(1)", failure_reason: task12MaliciousText, detail_unavailable_reason: null },
        { id: 3, user_name: "Credentialed", pd: "PD3", version: "1.0", status: 200, url: "https://user:password@example.test/private.zip", failure_reason: null, detail_unavailable_reason: null },
      ],
      next_cursor: null,
    } });
    if (pathname === "/api/usage-logs/v2/users" && request.method() === "GET") return route.fulfill({ json: {
      items: [{
        user_id: 7,
        username: "alice",
        name: task12MaliciousText,
        operation_count: 1,
        failed_count: 0,
        last_operation: null,
        last_activity_at_ms: 1_787_500_000_000,
      }],
      next_cursor: url.searchParams.has("cursor") ? null : "task12-users-next",
    } });
    const decodedPath = decodeURIComponent(pathname);
    const runPath = `/api/usage-logs/v2/runs/${task12RunId}`;
    const eventPath = `${runPath}/events/${task12EventId}`;
    if (pathname === "/api/usage-logs/v2/runs" && request.method() === "GET") return route.fulfill({ json: {
      items: [task12RunSummary()],
      next_cursor: null,
    } });
    if (decodedPath === runPath && request.method() === "GET") return route.fulfill({ json: {
      source_schema: 2,
      detail_available: true,
      detail_unavailable_reason: null,
      run: task12RunSummary(),
      events: [task12Event()],
    } });
    if (decodedPath === eventPath && request.method() === "GET") return route.fulfill({ json: {
      run: task12RunSummary(),
      event: task12Event(),
    } });
    if (decodedPath === `${eventPath}/output` && request.method() === "GET") {
      const stream = url.searchParams.get("stream") === "stderr" ? "stderr" : "stdout";
      return route.fulfill({ json: {
        run_id: task12RunId.slice(3),
        event_id: task12EventId,
        stream,
        chunks: stream === "stdout" ? [{
          chunk_id: "019d9c40-7b3c-7000-8000-000000000004",
          event_id: task12EventId,
          stream,
          chunk_index: 0,
          text: task12LongEvidence,
          byte_count: new TextEncoder().encode(task12LongEvidence).byteLength,
          sha256: "0".repeat(64),
        }] : [],
        next_after_chunk: null,
        output_complete: true,
      } });
    }

    state.unmocked.push(`${request.method()} ${pathname}`);
    return route.fulfill({ status: 404, json: { error: `Unmocked API route: ${pathname}` } });
  });
}

function task12RunSummary() {
  return {
    source_schema: 2,
    trace_ref: task12RunId,
    run_id: task12RunId.slice(3),
    legacy_id: null,
    user_id: 7,
    username: "alice",
    user_name: "Alice",
    operation_kind: "fastboot_flash",
    title: "Flash boot_a",
    outcome: "failed",
    client_version: "3.10.4",
    started_at_ms: 1_787_500_000_000,
    ended_at_ms: 1_787_500_000_100,
    duration_ms: 100,
    trace_complete: false,
    trace_loss_reason: "fixture_partial_trace",
  };
}

function task12Event() {
  return {
    event_id: task12EventId,
    run_id: task12RunId.slice(3),
    sequence: 1,
    kind: "command",
    step_name: "Flash boot_a",
    partition_name: "boot_a",
    status: "failed",
    started_at_ms: 1_787_500_000_000,
    ended_at_ms: 1_787_500_000_100,
    duration_ms: 100,
    command: {
      program: task12MaliciousText,
      argv: ["flash", "boot_a"],
      display_command: "fastboot flash boot_a",
      working_directory: null,
      paths: [`C:\\firmware\\${"very-long-segment-".repeat(28)}boot.img`],
      urls: ["https://example.test/boot.img"],
      serial: "TEST-SERIAL",
    },
    exit_code: 1,
    stdout_chunks: 1,
    stderr_chunks: 0,
    verification: null,
    device_state: "fastboot",
    retry_safe: false,
    remedies: ["检查设备状态"],
    error_class: "command_failed",
    error_code: "FLASH_FAILED",
    error_message: task12MaliciousText,
    credential_redactions: [],
  };
}

function requestRecord(request: PlaywrightRequest, pathname: string): Task12RequestRecord {
  return {
    method: request.method(),
    pathname,
    headers: request.headers(),
    body: request.postData(),
  };
}

function errorResponse(route: Route, status: number) {
  const message = status === 401 ? "会话已过期。" : status === 403 ? "无权执行该操作。" : "服务器暂时无法处理请求。";
  return route.fulfill({ status, json: { error: message } });
}

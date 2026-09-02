import { env, exports } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

import apiWorker, { type Env as WorkerEnv } from "../src/index";
import adminWorker from "../web/src/index";

declare module "cloudflare:workers" {
  interface ProvidedEnv extends WorkerEnv {
    TEST_MIGRATIONS: D1Migration[];
  }
}

const ADMIN_SESSION_TOKEN = "workerd-admin-session";
const API_TOKEN = "usage-details-bearer";
const LONG_MESSAGE = "x".repeat(20_000);

beforeEach(async () => {
  await reset();
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
  await seedUser(API_TOKEN);
  await seedAdminSession();
});

describe("POST /api/usage/logs details_json regression", () => {
  it("persists submitted details and returns them to the admin query", async () => {
    const response = await postUsageLogs([
      {
        operation: "Hashing",
        title: "提取本地固件",
        status: "success",
        event_id: "usage-details-1",
        started_at: 1_000,
        ended_at: 1_060,
        duration_ms: 60_000,
        details: [
          { timestamp_utc: 1_000, level: "Info", message: "正在提取已选择的固件分区" },
          { timestamp_utc: 1_030, level: "Info", message: "分区 boot：Completed" },
          { timestamp_utc: 1_060, level: "Success", message: "提取本地固件完成。" },
        ],
      },
    ]);

    expect(response.status).toBe(200);
    const stored = JSON.parse(await readStoredDetails("usage-details-1")) as Array<{
      timestamp_utc: number;
      level: string;
      message: string;
    }>;
    expect(stored).toEqual([
      { timestamp_utc: 1_000, level: "Info", message: "正在提取已选择的固件分区" },
      { timestamp_utc: 1_030, level: "Info", message: "分区 boot：Completed" },
      { timestamp_utc: 1_060, level: "Success", message: "提取本地固件完成。" },
    ]);

    const adminPage = await adminGetUsageLogs();
    const row = adminPage.logs.find((entry) => entry.title === "提取本地固件");
    expect(row).toBeDefined();
    expect(row?.details_json).toBe(await readStoredDetails("usage-details-1"));
  });

  it("treats missing or malformed details as an empty array for old clients", async () => {
    const response = await postUsageLogs([
      {
        operation: "Hashing",
        title: "旧客户端条目",
        status: "started",
        event_id: "usage-details-legacy-1",
        started_at: 1_000,
      },
      {
        operation: "Hashing",
        title: "details 非数组",
        status: "started",
        event_id: "usage-details-legacy-2",
        started_at: 1_001,
        details: { timestamp_utc: 1, level: "Info", message: "not-an-array" },
      },
      {
        operation: "Hashing",
        title: "details 含非对象成员",
        status: "started",
        event_id: "usage-details-legacy-3",
        started_at: 1_002,
        details: [null, 42, "text", { timestamp_utc: 2, level: "Info", message: "valid" }],
      },
    ]);

    expect(response.status).toBe(200);
    expect(await readStoredDetails("usage-details-legacy-1")).toBe("[]");
    expect(await readStoredDetails("usage-details-legacy-2")).toBe("[]");
    expect(await readStoredDetails("usage-details-legacy-3")).toBe(
      JSON.stringify([{ timestamp_utc: 2, level: "Info", message: "valid" }]),
    );
  });

  it("truncates details to at most 500 entries and caps each message at 16384 bytes", async () => {
    const overflowDetails = Array.from({ length: 520 }, (_, index) => ({
      timestamp_utc: 1_000 + index,
      level: "Info",
      message: `detail-${index}`,
    }));

    const response = await postUsageLogs([
      {
        operation: "Hashing",
        title: "超长详情",
        status: "success",
        event_id: "usage-details-cap-1",
        started_at: 1_000,
        details: [...overflowDetails, { timestamp_utc: 2_000, level: "Info", message: "should-drop" }],
      },
      {
        operation: "Hashing",
        title: "消息截断",
        status: "success",
        event_id: "usage-details-cap-2",
        started_at: 1_000,
        details: [{ timestamp_utc: 2_000, level: "Info", message: LONG_MESSAGE }],
      },
    ]);

    expect(response.status).toBe(200);
    const stored = JSON.parse(await readStoredDetails("usage-details-cap-1")) as Array<{
      timestamp_utc: number;
      level: string;
      message: string;
    }>;
    expect(stored).toHaveLength(500);
    expect(stored[0]).toEqual({ timestamp_utc: 1_000, level: "Info", message: "detail-0" });
    expect(stored[499]).toEqual({ timestamp_utc: 1_499, level: "Info", message: "detail-499" });

    const capped = JSON.parse(await readStoredDetails("usage-details-cap-2")) as Array<{
      message: string;
    }>;
    expect(capped).toHaveLength(1);
    expect(capped[0].message).toBe("x".repeat(16_384));
  });

  it("normalizes non-finite timestamps and blank levels while dropping empty messages", async () => {
    const response = await postUsageLogs([
      {
        operation: "Hashing",
        title: "字段归一化",
        status: "success",
        event_id: "usage-details-normalize-1",
        started_at: 1_000,
        details: [
          { timestamp_utc: "not-a-number", level: "", message: "空白级别" },
          { timestamp_utc: Number.NaN, level: "Success", message: "" },
          { timestamp_utc: 5, message: "缺级别字段" },
          { timestamp_utc: 6, level: "Info", message: 42 },
        ],
      },
    ]);

    expect(response.status).toBe(200);
    const stored = JSON.parse(await readStoredDetails("usage-details-normalize-1")) as Array<{
      timestamp_utc: number;
      level: string;
      message: string;
    }>;
    expect(stored).toEqual([
      { timestamp_utc: 0, level: "Info", message: "空白级别" },
      { timestamp_utc: 5, level: "Info", message: "缺级别字段" },
      { timestamp_utc: 6, level: "Info", message: "42" },
    ]);
  });

  it("keeps event_key idempotency so retried uploads do not duplicate rows", async () => {
    const body = {
      operation: "Hashing",
      title: "幂等重试",
      status: "success",
      event_id: "usage-details-idempotent-1",
      started_at: 1_000,
      details: [{ timestamp_utc: 1_000, level: "Info", message: "第一次上传" }],
    };

    const first = await postUsageLogs([body]);
    const retry = await postUsageLogs([{ ...body, details: [{ timestamp_utc: 1_001, level: "Info", message: "第二次上传" }] }]);
    expect(first.status).toBe(200);
    expect(retry.status).toBe(200);

    const rows = await countUsageLogRows("usage-details-idempotent-1");
    expect(rows).toBe(1);
    expect(await readStoredDetails("usage-details-idempotent-1")).toBe(
      JSON.stringify([{ timestamp_utc: 1_000, level: "Info", message: "第一次上传" }]),
    );
  });
});

async function seedUser(token: string): Promise<void> {
  await env.DB.prepare(
    `INSERT INTO api_users (id, username, name, token, enabled, banned)
     VALUES (7, 'alice', 'Alice', ?, 1, 0)`,
  ).bind(token).run();
}

async function seedAdminSession(): Promise<void> {
  await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO admins (id, username, salt, password_hash) VALUES (11, 'reviewer', 'unused', 'unused')",
    ),
    env.DB.prepare(
      "INSERT INTO admin_sessions (admin_id, token, expires_at) VALUES (11, ?, '2999-01-01T00:00:00.000Z')",
    ).bind(ADMIN_SESSION_TOKEN),
  ]);
}

interface UsageLogSubmission {
  operation: string;
  title: string;
  status: string;
  event_id: string;
  started_at: number;
  ended_at?: number;
  duration_ms?: number;
  details?: unknown;
}

async function postUsageLogs(logs: UsageLogSubmission[]): Promise<Response> {
  return exports.default.fetch(
    new Request("https://api.nwflash.cc.cd/api/usage/logs", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Nwflash-Version": "1.4.0",
        Authorization: `Bearer ${API_TOKEN}`,
      },
      body: JSON.stringify({ logs }),
    }),
    env,
  );
}

async function readStoredDetails(eventKey: string): Promise<string> {
  const row = await env.DB.prepare(
    "SELECT details_json FROM usage_logs WHERE event_key = ?",
  ).bind(eventKey).first<{ details_json: string }>();
  expect(row).not.toBeNull();
  return row?.details_json ?? "";
}

async function countUsageLogRows(eventKey: string): Promise<number> {
  const row = await env.DB.prepare(
    "SELECT COUNT(*) AS n FROM usage_logs WHERE event_key = ?",
  ).bind(eventKey).first<{ n: number }>();
  return Number(row?.n ?? 0);
}

interface AdminUsageLogRow {
  id: number;
  api_user_name: string;
  operation_kind: string;
  title: string;
  status: string;
  started_at: number;
  ended_at: number | null;
  duration_ms: number | null;
  details_json: string;
}

async function adminGetUsageLogs(): Promise<{ logs: AdminUsageLogRow[]; total: number }> {
  const response = await adminWorker.fetch(
    new Request("https://web.nwflash.cc.cd/api/usage-logs?limit=50", {
      headers: {
        Cookie: `nwflash_session=${ADMIN_SESSION_TOKEN}`,
        "X-Requested-With": "XMLHttpRequest",
      },
    }),
    env,
  );
  expect(response.status).toBe(200);
  return response.json() as Promise<{ logs: AdminUsageLogRow[]; total: number }>;
}

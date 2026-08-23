import { env } from "cloudflare:workers";
import { applyD1Migrations, reset, type D1Migration } from "cloudflare:test";
import { beforeEach, describe, expect, it } from "vitest";

declare module "cloudflare:workers" {
  interface ProvidedEnv {
    TEST_TRACE_V2_MIGRATIONS: D1Migration[];
  }
}

const legacyUsageLogsSql = "CREATE TABLE usage_logs (id INTEGER PRIMARY KEY AUTOINCREMENT, api_user_id INTEGER, api_user_name TEXT, operation_kind TEXT NOT NULL, title TEXT, status TEXT NOT NULL DEFAULT 'started', event_key TEXT, started_at INTEGER NOT NULL, ended_at INTEGER, duration_ms INTEGER, created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')));";

beforeEach(async () => {
  await reset();
  await env.DB.exec(legacyUsageLogsSql);
});

describe("usage trace V2 D1 migration", () => {
  it("applies the V2 migration twice and preserves V1 rows", async () => {
    await env.DB.prepare(
      "INSERT INTO usage_logs (operation_kind,status,event_key,started_at) VALUES ('Flashing','success','legacy-1',1)",
    ).run();
    await applyD1Migrations(env.DB, env.TEST_TRACE_V2_MIGRATIONS ?? []);
    await applyD1Migrations(env.DB, env.TEST_TRACE_V2_MIGRATIONS ?? []);

    expect(await scalar("SELECT COUNT(*) AS value FROM usage_logs WHERE event_key='legacy-1'")).toBe(1);
    expect(await tableExists("usage_operation_runs")).toBe(true);
    expect(await tableExists("usage_operation_events")).toBe(true);
    expect(await tableExists("usage_output_chunks")).toBe(true);
  });
});

async function scalar(query: string): Promise<number> {
  const row = await env.DB.prepare(query).first<{ value: number }>();
  return Number(row?.value ?? 0);
}

async function tableExists(name: string): Promise<boolean> {
  return (await scalar(`SELECT COUNT(*) AS value FROM sqlite_master WHERE type = 'table' AND name = '${name}'`)) === 1;
}

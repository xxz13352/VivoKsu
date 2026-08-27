import { webcrypto } from "node:crypto";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    cloudflareTest(async () => {
      const migrationDirectory = await mkdtemp(path.join(tmpdir(), "nwflash-workerd-migrations-"));
      const schema = await readFile(path.join(import.meta.dirname, "web", "schema.sql"), "utf8");
      await writeFile(path.join(migrationDirectory, "0001_schema.sql"), schema, "utf8");
      const migrations = await readD1Migrations(migrationDirectory);
      const traceMigrationDirectory = await mkdtemp(path.join(tmpdir(), "nwflash-workerd-trace-v2-migrations-"));
      const traceMigration = await readFile(path.join(import.meta.dirname, "web", "migrate-usage-traces-v2.sql"));
      const traceP0Migration = await readFile(path.join(import.meta.dirname, "web", "migrate-usage-traces-v2-p0.sql"));
      await writeFile(path.join(traceMigrationDirectory, "0001_trace_v2.sql"), traceMigration);
      await writeFile(path.join(traceMigrationDirectory, "0002_trace_v2.sql"), traceMigration);
      await writeFile(path.join(traceMigrationDirectory, "0003_trace_v2_p0.sql"), traceP0Migration);
      await writeFile(path.join(traceMigrationDirectory, "0004_trace_v2_p0.sql"), traceP0Migration);
      const traceV2Migrations = await readD1Migrations(traceMigrationDirectory);
      const traceUpgradeMigrationDirectory = await mkdtemp(path.join(tmpdir(), "nwflash-workerd-trace-v2-upgrade-migrations-"));
      const traceRetentionStageMigration = await readFile(
        path.join(import.meta.dirname, "web", "migrate-usage-traces-v2-retention-stage.sql"),
      );
      await writeFile(path.join(traceUpgradeMigrationDirectory, "0001_trace_v2.sql"), traceMigration);
      await writeFile(path.join(traceUpgradeMigrationDirectory, "0002_trace_v2_p0.sql"), traceP0Migration);
      await writeFile(
        path.join(traceUpgradeMigrationDirectory, "0003_trace_v2_retention_stage.sql"),
        traceRetentionStageMigration,
      );
      const traceV2UpgradeMigrations = await readD1Migrations(traceUpgradeMigrationDirectory);

      const generated = await webcrypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
      const pkcs8 = await webcrypto.subtle.exportKey("pkcs8", generated.privateKey);
      const signingSecret = Buffer.from(pkcs8).toString("base64url");

      return {
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: {
          modulesRules: [
            { type: "Text", include: ["**/*.sql", "**/*.html", "**/*.css", "**/admin/**/*.js"], fallthrough: true },
          ],
          bindings: {
            TEST_MIGRATIONS: migrations,
            TEST_TRACE_V2_MIGRATIONS: traceV2Migrations,
            TEST_TRACE_V2_UPGRADE_MIGRATIONS: traceV2UpgradeMigrations,
            SESSION_SIGNING_PRIVATE_KEY_PKCS8: signingSecret,
            VOTA_API_TOKEN: "unused-in-workerd-tests",
          },
        },
      };
    }),
  ],
  test: {
    include: ["test/*.workerd.test.ts"],
    testTimeout: 10_000,
  },
});

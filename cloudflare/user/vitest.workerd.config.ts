import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [
    cloudflareTest(async () => {
      const migrationDirectory = await mkdtemp(path.join(tmpdir(), "nwflash-user-workerd-migrations-"));
      const schema = await readFile(path.join(import.meta.dirname, "..", "web", "schema.sql"), "utf8");
      await writeFile(path.join(migrationDirectory, "0001_schema.sql"), schema, "utf8");
      const migrations = await readD1Migrations(migrationDirectory);

      return {
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: {
          modulesRules: [
            { type: "Text", include: ["**/*.html", "**/*.css", "**/*.client.js"], fallthrough: true },
          ],
          bindings: { TEST_MIGRATIONS: migrations },
        },
      };
    }),
  ],
  test: {
    include: ["test/user.workerd.test.ts"],
    testTimeout: 20_000,
  },
});

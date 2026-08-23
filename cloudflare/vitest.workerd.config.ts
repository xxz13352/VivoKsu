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

      const generated = await webcrypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
      const pkcs8 = await webcrypto.subtle.exportKey("pkcs8", generated.privateKey);
      const signingSecret = Buffer.from(pkcs8).toString("base64url");

      return {
        wrangler: { configPath: "./wrangler.toml" },
        miniflare: {
          bindings: {
            TEST_MIGRATIONS: migrations,
            SESSION_SIGNING_PRIVATE_KEY_PKCS8: signingSecret,
            VOTA_API_TOKEN: "unused-in-workerd-tests",
          },
        },
      };
    }),
  ],
  test: {
    include: ["test/security.workerd.test.ts"],
    testTimeout: 10_000,
  },
});

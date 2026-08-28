import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const cloudflareRoot = fileURLToPath(new URL("../", import.meta.url));
const mode = process.argv[2];
const targets = {
  api: { config: null, outdir: "website-stage-api" },
  web: { config: resolve(cloudflareRoot, "web", "wrangler.toml"), outdir: "website-stage-web" },
};
const target = targets[mode];
if (!target) {
  console.error("Usage: node scripts/wrangler-dry-run.mjs <api|web>");
  process.exit(2);
}

const wranglerCli = resolve(cloudflareRoot, "node_modules", "wrangler", "bin", "wrangler.js");
const outdir = resolve(cloudflareRoot, ".wrangler", target.outdir);
const args = [wranglerCli, "deploy", "--dry-run"];
if (target.config) args.push("--config", target.config);
args.push("--outdir", outdir);

const result = spawnSync(process.execPath, args, {
  cwd: cloudflareRoot,
  stdio: "inherit",
  windowsHide: true,
});
if (result.error) throw result.error;
process.exit(result.status ?? 1);

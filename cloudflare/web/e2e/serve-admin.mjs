import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { ADMIN_STATIC_FIXTURES } from "./admin-static-manifest.mjs";

const root = fileURLToPath(new URL("../src/admin/", import.meta.url));
const rootPrefix = root.endsWith(sep) ? root : `${root}${sep}`;
const mime = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
};

createServer(async (request, response) => {
  const url = new URL(request.url ?? "/", "http://127.0.0.1");
  if (url.pathname === "/__health") {
    response.writeHead(204, { "Cache-Control": "no-store" });
    response.end();
    return;
  }
  if (url.pathname.startsWith("/api/")) {
    response.writeHead(404, { "Content-Type": "application/json; charset=utf-8" });
    response.end(JSON.stringify({ error: "Unmocked API route" }));
    return;
  }

  const relative = ADMIN_STATIC_FIXTURES[url.pathname];
  if (relative === undefined) {
    response.writeHead(404).end("Not found");
    return;
  }

  const target = resolve(root, relative);
  if (target !== resolve(root, "index.html") && !target.startsWith(rootPrefix)) {
    response.writeHead(404).end("Not found");
    return;
  }

  try {
    const body = await readFile(target);
    response.writeHead(200, {
      "Cache-Control": "no-store",
      "Content-Type": mime[extname(target)] ?? "application/octet-stream",
      "X-Content-Type-Options": "nosniff",
    });
    response.end(body);
  } catch {
    response.writeHead(404).end("Not found");
  }
}).listen(4179, "127.0.0.1");

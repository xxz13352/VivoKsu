/**
 * nwflash.cc.cd —— Nwflash(奶娃Flash)官网托管 Worker。
 * 仅托管静态官网 index.html;与 api.nwflash.cc.cd / web.nwflash.cc.cd 无关。
 */

import websiteHtml from "./index.html";

const SECURE_HEADERS: Record<string, string> = {
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
  "X-Content-Type-Options": "nosniff",
  "X-Frame-Options": "DENY",
  "Referrer-Policy": "no-referrer",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
  "Content-Security-Policy":
    "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'",
  "Cache-Control": "public, max-age=3600",
};

export default {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);

    // 强制 HTTPS(边缘通常已处理,双保险)
    if (request.headers.get("x-forwarded-proto") === "http") {
      return Response.redirect(`https://${url.host}${url.pathname}${url.search}`, 301);
    }

    // 官网单页:任何路径都返回 index.html(无 SPA 路由,统一入口)。
    return new Response(websiteHtml, {
      headers: { "Content-Type": "text/html; charset=utf-8", ...SECURE_HEADERS },
    });
  },
};

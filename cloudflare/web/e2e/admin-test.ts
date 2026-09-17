import { expect, test as base, type Page } from "@playwright/test";

interface AllowedHttpError {
  url: string;
  status: number;
  remaining: number;
}

export interface AdminRuntimeGate {
  allowHttpError(url: string, status: number, count?: number): void;
}

export const test = base.extend<{ runtimeGate: AdminRuntimeGate }>({
  runtimeGate: [async ({ page }, use) => {
    const allowed: AllowedHttpError[] = [];
    const responses: Array<{ url: string; status: number }> = [];
    const genericResourceErrors: number[] = [];
    const unexpected: string[] = [];

    page.on("console", (message) => {
      if (message.type() !== "error") return;
      const match = message.text().match(/^Failed to load resource:.*status of ([0-9]{3})/);
      if (match) genericResourceErrors.push(Number(match[1]));
      else unexpected.push(`console.error: ${message.text()}`);
    });
    page.on("pageerror", (error) => unexpected.push(`pageerror: ${error.message}`));
    page.on("requestfailed", (request) => {
      const reason = request.failure()?.errorText ?? "request failed";
      if (!/aborted|cancelled|ERR_ABORTED/i.test(reason)) {
        unexpected.push(`requestfailed: ${reason} ${request.url()}`);
      }
    });
    page.on("response", (response) => {
      if (response.status() >= 400) {
        const parsed = new URL(response.url());
        responses.push({ url: `${parsed.pathname}${parsed.search}`, status: response.status() });
      }
    });

    const runtimeGate: AdminRuntimeGate = {
      allowHttpError(url, status, count = 1) {
        if (!url.startsWith("/") || !Number.isInteger(status) || status < 400 || count < 1) {
          throw new TypeError("Expected HTTP errors require an exact relative URL, status, and positive count");
        }
        allowed.push({ url, status, remaining: count });
      },
    };

    await use(runtimeGate);

    const allowedConsoleCounts = new Map<number, number>();
    for (const response of responses) {
      const rule = allowed.find((candidate) =>
        candidate.remaining > 0 && candidate.url === response.url && candidate.status === response.status);
      if (!rule) {
        unexpected.push(`unexpected HTTP ${response.status}: ${response.url}`);
        continue;
      }
      rule.remaining -= 1;
      allowedConsoleCounts.set(response.status, (allowedConsoleCounts.get(response.status) ?? 0) + 1);
    }
    for (const status of genericResourceErrors) {
      const remaining = allowedConsoleCounts.get(status) ?? 0;
      if (remaining > 0) allowedConsoleCounts.set(status, remaining - 1);
      else unexpected.push(`unexpected browser resource console error: HTTP ${status}`);
    }
    for (const rule of allowed) {
      if (rule.remaining > 0) unexpected.push(`expected HTTP ${rule.status} not observed: ${rule.url} ×${rule.remaining}`);
    }
    expect(unexpected, unexpected.join("\n")).toEqual([]);
  }, { auto: true }],
});

export { expect };
export type { Page };

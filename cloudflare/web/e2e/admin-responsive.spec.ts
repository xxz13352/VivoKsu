import { expect, test, type Page } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import { isAbsolute, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  createTask12ApiState,
  installTask12Api,
  task12EventId,
  task12LongEvidence,
  task12RunId,
} from "./admin-api-fixtures";

const widths = [320, 360, 768, 1024, 1440];
const routes = [
  { name: "overview", url: "/?view=overview", ready: ".overview-workspace" },
  { name: "versions", url: "/?view=versions", ready: ".version-list" },
  { name: "users", url: "/?view=users", ready: ".user-list" },
  { name: "sessions", url: "/?view=sessions", ready: ".session-list" },
  { name: "audit", url: "/?view=audit", ready: "[data-audit-action='open-user']" },
  { name: "rom", url: "/?view=rom", ready: ".rom-list" },
  { name: "audit-user", url: "/?view=audit&level=user&userId=7", ready: "[data-audit-action='open-run']" },
  { name: "audit-run", url: `/?view=audit&level=run&runId=${encodeURIComponent(task12RunId)}`, ready: "[data-audit-action='open-event']" },
  { name: "audit-command", url: `/?view=audit&level=command&runId=${encodeURIComponent(task12RunId)}&eventId=${task12EventId}`, ready: "[data-command-field='paths'] .audit-code", localOverflow: true },
  { name: "audit-output", url: `/?view=audit&level=output&runId=${encodeURIComponent(task12RunId)}&eventId=${task12EventId}&stream=stdout`, ready: "[data-output-stream='stdout']", localOverflow: true },
];

test("keeps every primary workspace inside 320-1440 with readable text and 44px targets", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  const screenshotRoot = validatedScreenshotRoot();
  if (screenshotRoot) await mkdir(screenshotRoot, { recursive: true });

  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    state.authenticated = false;
    await page.goto("/?view=overview");
    await expect(page.getByRole("heading", { name: "管理员登录" })).toBeVisible();
    await expectResponsivePage(page);
    if (screenshotRoot) await page.screenshot({ path: resolve(screenshotRoot, `task12-login-${width}.png`), fullPage: true });

    state.authenticated = true;
    for (const route of routes) {
      await page.goto(route.url);
      const ready = page.locator(route.ready);
      await expect(ready).toBeVisible();
      if (route.name === "audit-output") await expect(ready).toContainText(task12LongEvidence);
      await expectResponsivePage(page);
      if (route.localOverflow) {
        const overflow = await ready.evaluate((element) => {
          const rect = element.getBoundingClientRect();
          return {
            local: element.scrollWidth > element.clientWidth,
            inside: rect.left >= 0 && rect.right <= document.documentElement.clientWidth,
          };
        });
        expect(overflow).toEqual({ local: true, inside: true });
      }
      if (screenshotRoot) await page.screenshot({ path: resolve(screenshotRoot, `task12-${route.name}-${width}.png`), fullPage: true });
    }
  }

  expect(state.unmocked).toEqual([]);
});

async function expectResponsivePage(page: Page) {
  const layout = await page.evaluate(() => ({
    htmlOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    bodyOverflow: document.body.scrollWidth - document.body.clientWidth,
    bodyFont: Number.parseFloat(getComputedStyle(document.body).fontSize),
    compactBrandFont: Number.parseFloat(getComputedStyle(document.querySelector(".brand-copy small")).fontSize),
    monoFonts: [...document.querySelectorAll("code, pre, time, .eyebrow")].map((element) => Number.parseFloat(getComputedStyle(element).fontSize)),
  }));
  expect(layout.htmlOverflow).toBe(0);
  expect(layout.bodyOverflow).toBe(0);
  expect(layout.bodyFont).toBeGreaterThanOrEqual(13);
  expect(layout.compactBrandFont).toBeGreaterThanOrEqual(12);
  expect(layout.monoFonts.every((size) => size >= 12)).toBe(true);

  const undersized = await page.locator("button, input, select, textarea, a[href], [tabindex='0']").evaluateAll((elements) => elements
    .filter((element) => {
      const style = getComputedStyle(element);
      return style.display !== "none"
        && style.visibility !== "hidden"
        && !(element as HTMLElement).hidden
        && element.getClientRects().length > 0;
    })
    .map((element) => {
      const rect = element.getBoundingClientRect();
      return { tag: element.tagName, text: element.textContent?.trim(), width: rect.width, height: rect.height };
    })
    .filter((metric) => metric.width < 44 || metric.height < 44));
  expect(undersized, JSON.stringify(undersized)).toEqual([]);
}

test("meets non-text control contrast and honors reduced motion and forced colors", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.goto("/?view=versions");

  const contrast = await page.evaluate(() => {
    const root = getComputedStyle(document.documentElement);
    const control = root.getPropertyValue("--control-line").trim();
    const samples = ["--surface", "--canvas", "--surface-raised"].map((name) => root.getPropertyValue(name).trim());
    const luminance = (color: string) => {
      const match = color.match(/^#([0-9a-f]{6})$/i);
      if (!match) return Number.NaN;
      const components = match[1].match(/../g)!.map((pair) => Number.parseInt(pair, 16) / 255)
        .map((component) => component <= 0.04045 ? component / 12.92 : ((component + 0.055) / 1.055) ** 2.4);
      return 0.2126 * components[0] + 0.7152 * components[1] + 0.0722 * components[2];
    };
    const ratio = (left: string, right: string) => {
      const [a, b] = [luminance(left), luminance(right)];
      return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
    };
    return { control, ratios: samples.map((sample) => ratio(control, sample)) };
  });
  expect(contrast.control).toBe("#5a7085");
  expect(contrast.ratios.every((ratio) => ratio >= 3)).toBe(true);

  await page.emulateMedia({ reducedMotion: "reduce", forcedColors: "active" });
  const firstMenu = page.locator('[data-menu-id="overview"]');
  await firstMenu.focus();
  const media = await firstMenu.evaluate((element) => {
    const root = getComputedStyle(document.documentElement);
    const style = getComputedStyle(element);
    return {
      reduced: matchMedia("(prefers-reduced-motion: reduce)").matches,
      forced: matchMedia("(forced-colors: active)").matches,
      scrollBehavior: root.scrollBehavior,
      transitionMs: Number.parseFloat(style.transitionDuration) * (style.transitionDuration.includes("ms") ? 1 : 1_000),
      animationMs: Number.parseFloat(style.animationDuration) * (style.animationDuration.includes("ms") ? 1 : 1_000),
      outlineStyle: style.outlineStyle,
      outlineWidth: Number.parseFloat(style.outlineWidth),
    };
  });
  expect(media).toMatchObject({ reduced: true, forced: true, scrollBehavior: "auto" });
  expect(media.transitionMs).toBeLessThanOrEqual(0.01);
  expect(media.animationMs).toBeLessThanOrEqual(0.01);
  expect(media.outlineStyle).not.toBe("none");
  expect(media.outlineWidth).toBeGreaterThanOrEqual(2);
});

function validatedScreenshotRoot(): string | null {
  const requested = process.env.NWFLASH_ADMIN_SCREENSHOT_DIR;
  if (!requested) return null;
  if (!isAbsolute(requested)) throw new Error("NWFLASH_ADMIN_SCREENSHOT_DIR must be absolute");
  const workspaceRoot = fileURLToPath(new URL("../../../", import.meta.url));
  const relation = relative(workspaceRoot, requested);
  if (!relation.startsWith("..") || isAbsolute(relation)) {
    throw new Error("NWFLASH_ADMIN_SCREENSHOT_DIR must be outside the workspace");
  }
  return requested;
}

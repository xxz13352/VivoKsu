import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "./admin-test";

import { createTask12ApiState, installTask12Api } from "./admin-api-fixtures";

const menuLabels = ["概览", "版本策略", "用户管理", "在线会话", "操作审计", "ROM 查询"];
const widths = [320, 360, 768, 1024, 1440];

test("implements the locked desktop information architecture and keyboard path", async ({ page }) => {
  const state = createTask12ApiState();
  await installTask12Api(page, state);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?view=overview");

  const sidebar = page.locator(".shell-sidebar");
  const menu = sidebar.getByRole("navigation", { name: "主菜单" });
  const health = sidebar.getByRole("status", { name: "服务健康" });
  const path = page.getByRole("navigation", { name: "当前位置" });
  await expect(sidebar).toBeVisible();
  await expect(menu.getByRole("button")).toHaveText(menuLabels);
  await expect(health).toContainText("会话已验证");
  await expect(path).toContainText("NWFLASH / ADMIN / 概览");
  await expect(page.getByRole("searchbox", { name: "全局搜索" })).toBeVisible();
  await expect(page.getByRole("button", { name: /账户/ })).toBeVisible();

  const desktop = await page.evaluate(() => {
    const sidebar = document.querySelector(".shell-sidebar")!;
    const content = document.querySelector(".shell-content")!;
    const health = document.querySelector(".service-health")!;
    const sidebarRect = sidebar.getBoundingClientRect();
    const contentRect = content.getBoundingClientRect();
    const healthRect = health.getBoundingClientRect();
    return {
      sidebar: { x: sidebarRect.x, width: sidebarRect.width, height: sidebarRect.height },
      sidebarPosition: getComputedStyle(sidebar).position,
      contentX: contentRect.x,
      healthBottom: Math.round(sidebarRect.bottom - healthRect.bottom),
      healthInsideMenu: Boolean(health.closest(".admin-menu")),
    };
  });
  expect(desktop).toEqual({
    sidebar: { x: 0, width: 196, height: 900 },
    sidebarPosition: "fixed",
    contentX: 196,
    healthBottom: 16,
    healthInsideMenu: false,
  });

  const overview = menu.getByRole("button", { name: "概览" });
  await overview.focus();
  await overview.press("ArrowRight");
  await expect(menu.getByRole("button", { name: "版本策略" })).toBeFocused();
  await page.keyboard.press("End");
  await expect(menu.getByRole("button", { name: "ROM 查询" })).toBeFocused();
  await page.keyboard.press("Home");
  await overview.press("ArrowRight");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\?view=versions$/);
  await expect(path).toContainText("NWFLASH / ADMIN / 版本策略");
});

test("keeps the real shell accessible and overflow-free across five widths", async ({ page }) => {
  test.slow();
  const state = createTask12ApiState();
  await installTask12Api(page, state);

  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/?view=overview");

    const sidebar = page.locator(".shell-sidebar");
    const menu = sidebar.getByRole("navigation", { name: "主菜单" });
    await expect(menu.getByRole("button")).toHaveText(menuLabels);
    await expect(sidebar.getByRole("status", { name: "服务健康" })).toBeVisible();
    await expect(page.getByRole("navigation", { name: "当前位置" })).toContainText("概览");
    await expect(page.getByRole("searchbox", { name: "全局搜索" })).toBeVisible();
    await expect(page.getByRole("button", { name: /账户/ })).toBeVisible();

    const layout = await page.evaluate(() => {
      const list = document.querySelector(".admin-menu-list")!;
      return {
        bodyOverflow: document.body.scrollWidth - document.body.clientWidth,
        htmlOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
        sidebarPosition: getComputedStyle(document.querySelector(".shell-sidebar")!).position,
        menuColumns: getComputedStyle(list).gridTemplateColumns.split(/\s+/).filter(Boolean).length,
      };
    });
    expect(layout.bodyOverflow).toBe(0);
    expect(layout.htmlOverflow).toBe(0);
    if (width <= 360) {
      expect(layout.sidebarPosition).toBe("static");
      expect(layout.menuColumns).toBe(2);
      const buttons = await menu.getByRole("button").evaluateAll((elements) => elements.map((element) => {
        const rect = element.getBoundingClientRect();
        return { left: rect.left, right: rect.right, height: rect.height };
      }));
      expect(buttons.every(({ left, right, height }) => left >= 0 && right <= width && height >= 44)).toBe(true);
    } else {
      expect(layout.sidebarPosition).toBe("fixed");
    }

    const axe = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
      .analyze();
    expect(axe.violations, `${width}px\n${JSON.stringify(axe.violations, null, 2)}`).toEqual([]);
  }

  expect(state.unmocked).toEqual([]);
});

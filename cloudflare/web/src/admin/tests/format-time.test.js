import { describe, expect, it } from "vitest";
import { formatBeijingTime, formatBeijingUtcText } from "../format-time.js";

describe("admin time display formatting", () => {
  it("renders epoch milliseconds as Beijing time in yy/M/d HH:mm", () => {
    // 2026-09-14T08:46:32Z → 北京时间 16:46。
    expect(formatBeijingTime(Date.UTC(2026, 8, 14, 8, 46, 32))).toBe("26/9/14 16:46");
    // 月/日/时不补零(2026-02-01T16:05:00Z → 次日 00:05)。
    expect(formatBeijingTime(Date.UTC(2026, 1, 1, 16, 5, 0))).toBe("26/2/2 00:05");
    // 两位年跨世纪边界仍取后两位(1999-12-31T16:00Z → 北京 2000-01-01)。
    expect(formatBeijingTime(Date.UTC(1999, 11, 31, 16, 0, 0))).toBe("00/1/1 00:00");
  });

  it("rejects non-integer, negative, and NaN inputs with the fallback", () => {
    expect(formatBeijingTime(-1)).toBe("未提供");
    expect(formatBeijingTime(Number.NaN)).toBe("未提供");
    expect(formatBeijingTime(1.5)).toBe("未提供");
    expect(formatBeijingTime("not-a-time")).toBe("未提供");
    // null/空串经 Number() 归 0:epoch 0 是合法时刻,按原样渲染而非兜底。
    expect(formatBeijingTime(null)).toBe("70/1/1 08:00");
  });

  it("is deterministic across host timezones (no local-time dependence)", () => {
    // 同一 UTC 瞬间在任何宿主时区都渲染同一字符串:函数只做固定 +8h
    // 偏移后读 UTC 分量,不查宿主时区。用两个不同参考值锁定输出形状。
    const instant = Date.UTC(2026, 8, 14, 0, 0, 0);
    expect(formatBeijingTime(instant)).toBe("26/9/14 08:00");
    expect(formatBeijingTime(instant + 8 * 3600 * 1000)).toBe("26/9/14 16:00");
  });

  it("converts D1 datetime('now') UTC text into Beijing display format", () => {
    expect(formatBeijingUtcText("2026-09-14 08:46:32")).toBe("26/9/14 16:46");
    // D1 常见的仅日期文本(如 api_users.created_at)按 00:00 UTC 解析。
    expect(formatBeijingUtcText("2026-08-28")).toBe("26/8/28 08:00");
    // 带毫秒/时区的 ISO 文本按 UTC 语义解析偏移(拒绝本地时区解释)。
    expect(formatBeijingUtcText("2026-09-14 08:46:32.123")).toBe("26/9/14 16:46");
  });

  it("falls back for blank or unparseable D1 text", () => {
    expect(formatBeijingUtcText("", "—")).toBe("—");
    expect(formatBeijingUtcText(null, "—")).toBe("—");
    expect(formatBeijingUtcText("not a date", "—")).toBe("—");
  });
});

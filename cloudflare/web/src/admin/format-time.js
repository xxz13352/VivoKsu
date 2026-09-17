/**
 * 管理台时间展示:固定按北京时间(UTC+8)格式化为 `yy/M/d HH:mm`
 * (如 26/9/14 16:46)。不取浏览器本地时区——管理员与用户均在国内,
 * 固定偏移保证同一记录在任何运行环境(浏览器/CI/workerd)显示一致,
 * 也避免 unit 测试因宿主时区漂移。
 *
 * 只影响展示层;筛选参数(from/to)仍走服务端 Date.parse 的 ISO/epoch
 * 输入,导出 NDJSON 亦保持机器可读的 ISO 契约,均不受此格式影响。
 */
const BEIJING_OFFSET_MS = 8 * 60 * 60 * 1000;

const padTwo = (value) => String(value).padStart(2, "0");

/**
 * epoch 毫秒 → `yy/M/d HH:mm`(北京时间)。非法/负值返回 fallback(默认
 * "未提供"),与各页此前的兜底文案一致。
 */
export function formatBeijingTime(milliseconds, fallback = "未提供") {
  const value = Number(milliseconds);
  if (!Number.isSafeInteger(value) || value < 0) return fallback;
  const shifted = new Date(value + BEIJING_OFFSET_MS);
  if (Number.isNaN(shifted.getTime())) return fallback;
  const year = String(shifted.getUTCFullYear()).slice(-2);
  const month = shifted.getUTCMonth() + 1;
  const day = shifted.getUTCDate();
  const hour = padTwo(shifted.getUTCHours());
  const minute = padTwo(shifted.getUTCMinutes());
  return `${year}/${month}/${day} ${hour}:${minute}`;
}

/**
 * D1 `datetime('now')` 生成的 UTC 文本(`YYYY-MM-DD HH:MM:SS`)→ 北京时间
 * 展示。缺省值/非法文本返回 fallback。
 */
export function formatBeijingUtcText(utcText, fallback = "未提供") {
  if (typeof utcText !== "string" || utcText.trim() === "") return fallback;
  const normalized = utcText.trim().replace(" ", "T") + "Z";
  const parsed = Date.parse(normalized);
  if (!Number.isSafeInteger(parsed) || parsed < 0) return fallback;
  return formatBeijingTime(parsed, fallback);
}

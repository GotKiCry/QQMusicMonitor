/**
 * 时间格式化与解析工具
 */

/** 毫秒 -> MM:SS 文本 */
export function formatMs(ms) {
    if (!Number.isFinite(ms) || ms < 0) ms = 0;
    const totalSecs = Math.floor(ms / 1000);
    const m = String(Math.floor(totalSecs / 60)).padStart(2, '0');
    const s = String(totalSecs % 60).padStart(2, '0');
    return `${m}:${s}`;
}

/** LRC 时间标签 [mm:ss.xx] -> 秒（f64），失败返回 null */
export function parseLrcTime(timeStr) {
    const parts = timeStr.split(':');
    if (parts.length !== 2) return null;
    const min = Number(parts[0]);
    const sec = Number(parts[1]);
    if (!Number.isFinite(min) || !Number.isFinite(sec)) return null;
    return min * 60 + sec;
}

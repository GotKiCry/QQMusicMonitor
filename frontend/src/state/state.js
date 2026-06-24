/**
 * 全局运行时状态：当前歌曲数据、歌词结构、插值基线、连接状态。
 *
 * 切歌 bug 修复的核心：所有"切歌时必须重置"的状态都集中在这里，
 * 由 resetForSongChange() 统一清空，避免旧歌词残留继续渲染。
 */

/** @type {any | null} 最近一次收到的 SongInfo */
export let songInfo = null;

/** @type {any[]} 歌词行 DOM 元素与时间信息 */
export let lyricLines = [];

/** @type {any[]} 翻译时间轴条目 */
export let transMap = [];

/** 上一次歌词重建的 key（title|artist|qrcLength），用于判断是否需要重建 */
export let lastRenderKey = '';

/** 当前高亮行索引，-1 表示无 */
export let lastActiveIdx = -1;

/** requestAnimationFrame 句柄 */
export let animFrame = 0;

/** 上一帧同步耗时（ms），用于调试显示 */
export let lastSyncMs = 0;

// —— 插值基线（切歌时必须重置，否则进度条会从旧歌位置继续走）——
/** 上一次采样的播放进度（ms） */
export let lastSampleProgress = 0;
/** 上一次采样的本地时间（performance.now()） */
export let lastSampleLocalTime = 0;
/** 上一次原始 current_time_ms，用于去重/跳变检测 */
export let lastRawTimeMs = -1;
/** 是否正在播放 */
export let isPlaying = false;

// —— 连接状态 ——
/** @type {WebSocket | null} */
export let ws = null;
export let reconnectAttempts = 0;
export let reconnectTimer = 0;
export let isIntentionalClose = false;

/* ============================================================
 * 状态修改器：因为 export 的 let 是只读视图，需要通过函数修改
 * ============================================================ */

/** @param {any | null} v */
export function setSongInfo(v) { songInfo = v; }
/** @param {any[]} v */
export function setLyricLines(v) { lyricLines = v; }
/** @param {any[]} v */
export function setTransMap(v) { transMap = v; }
/** @param {string} v */
export function setLastRenderKey(v) { lastRenderKey = v; }
/** @param {number} v */
export function setLastActiveIdx(v) { lastActiveIdx = v; }
/** @param {number} v */
export function setAnimFrame(v) { animFrame = v; }
/** @param {number} v */
export function setLastSyncMs(v) { lastSyncMs = v; }
/** @param {number} v */
export function setLastSampleProgress(v) { lastSampleProgress = v; }
/** @param {number} v */
export function setLastSampleLocalTime(v) { lastSampleLocalTime = v; }
/** @param {number} v */
export function setLastRawTimeMs(v) { lastRawTimeMs = v; }
/** @param {boolean} v */
export function setIsPlaying(v) { isPlaying = v; }
/** @param {WebSocket | null} v */
export function setWs(v) { ws = v; }
/** @param {number} v */
export function setReconnectAttempts(v) { reconnectAttempts = v; }
/** @param {number} v */
export function setReconnectTimer(v) { reconnectTimer = v; }
/** @param {boolean} v */
export function setIsIntentionalClose(v) { isIntentionalClose = v; }

/**
 * 切歌时彻底重置所有与"具体歌曲内容"相关的状态。
 *
 * 修复现象：切歌后歌名/专辑/图片已切，但旧歌词仍继续渲染、
 * 进度条与歌词高亮停在旧歌位置好几秒才同步。
 *
 * 根因：旧代码只重置了部分字段，插值基线（lastSampleProgress 等）
 * 仍停留在旧歌进度，渲染循环据此继续推进旧歌词高亮。
 * 此函数统一清空：歌词数据、高亮索引、渲染 key、插值基线。
 *
 * @param {number} freshProgress - 新歌起始进度（ms），通常为 0
 * @param {number} nowLocal - performance.now() 时间戳
 */
export function resetForSongChange(freshProgress, nowLocal) {
    setLyricLines([]);
    setTransMap([]);
    setLastRenderKey('');
    setLastActiveIdx(-1);

    setLastRawTimeMs(freshProgress);
    setLastSampleProgress(freshProgress);
    setLastSampleLocalTime(nowLocal);
}

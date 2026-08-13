/**
 * 状态栏歌词（嵌入任务栏）入口与渲染逻辑。
 *
 * 独立窗口（lyric-bar.html）加载，复用 Tauri 的 song-info /
 * playback-progress 事件通道；本地 rAF 插值推进时间：
 *  - QRC 逐字歌词：逐字渐变高亮（与主界面一致的扫光效果）
 *  - 无 QRC：整行主题渐变文字（LRC 兜底）
 *  - 歌词宽度超出任务栏可用空间时自动水平往返滚动
 *
 * 时间同步与主窗口同一套策略：server_ts 传输补偿、淡出尾音钉住、
 * play→pause 快照、pause→play 原地 hold、seek 立即跟随、小抖动死区容忍。
 * 行状态区分 pending（前奏）/ active（高亮中）/ done（已唱完，变暗）。
 */

import { listenSongInfo } from '../connection/tauri-bridge.js';
import {
    loadFrontendConfig,
    STORAGE_KEY,
    hydrateFrontendConfig
} from '../config/frontend-config.js';
import { applyThemeToDocument, AUTO_COLOR_KEY } from '../config/theme.js';
import { parseTranslation, findTranslation } from './translation.js';

const els = {
    line: document.getElementById('lyric-bar-line'),
    inner: document.getElementById('lyric-bar-inner'),
    trans: document.getElementById('lyric-bar-trans')
};

/** 行数组：{start, end, text, words?: [{content, start, dur}]} */
let rows = [];
let transMap = [];
let showTrans = true;

/** 插值基线（与主窗口 render-loop 一致：服务端权威值 + 本地单调时钟流逝） */
let baseT = 0;
let baseTs = 0;
let playing = false;
let hasSong = false;

/** 淡出尾音 / 死区跟踪（与主窗口 update-handler 同策略） */
let lastServerPosMs = -1;
let behindCount = 0;

/** 当前渲染状态 */
let currentRowIdx = -1;
let currentRowState = '';   // 'pending' | 'active' | 'done'
let currentRowStart = 0;    // 当前行起始时间（翻译行重新匹配用）
let wordEls = [];           // 当前行的逐字 fg 元素 {el, start, dur, width}
let lastShownTrans = '';
let lastRenderKey = '';     // 静态内容指纹：跳过同内容的重复重建
let scrollOverflow = 0;     // 上次测量的滚动溢出量（防无关 layout 抖动重开动画）

/** 占位模式：'idle' | 'loading' | 'pure' | 'plain' | null */
let fallbackMode = null;
let fallbackText = '';
let loadingStartedAt = 0;

const FALLBACK_TEXT = {
    idle: '等待播放',
    loading: '正在加载歌词...',
    pure: '♪ 纯音乐，请欣赏'
};

/** QRC 行级构建（保留逐字绝对时间） */
function rowsFromQrc(qrcData) {
    return qrcData.map((line) => {
        const words = (line.words || []).map((w) => ({
            content: w.content,
            start: w.start_time_ms,
            dur: w.duration_ms || 200
        }));
        const text = words.map((w) => w.content).join('');
        const durEnd = line.duration_ms > 0 ? line.start_time_ms + line.duration_ms : 0;
        const lastWord = line.words && line.words.length > 0
            ? line.words[line.words.length - 1]
            : null;
        // 词时间为绝对时间（相对歌曲开头），与 XML/QRC-text 解析器输出一致
        const lastWordEnd = lastWord ? lastWord.start_time_ms + lastWord.duration_ms : 0;
        let end = Math.max(durEnd, lastWordEnd);
        if (end <= 0) end = line.start_time_ms + 4000;
        return { start: line.start_time_ms, end, text, words };
    });
}

/** LRC 文本解析兜底（无逐字数据）；支持一行多时间标签（重复副歌） */
function rowsFromLrc(lyrics) {
    const out = [];
    const tagRe = /\[(\d{2}):(\d{2})(?:[.:](\d{2,3}))?\]/g;
    for (const raw of (lyrics || '').split('\n')) {
        const trimmed = raw.trim();
        if (!trimmed) continue;
        const tags = [...trimmed.matchAll(tagRe)];
        if (tags.length === 0) continue;
        const lastTag = tags[tags.length - 1];
        const text = trimmed.slice(lastTag.index + lastTag[0].length).trim();
        if (!text) continue;
        for (const m of tags) {
            const ms = (parseInt(m[1], 10) * 60 + parseInt(m[2], 10)) * 1000
                + (m[3] ? parseInt(m[3].padEnd(3, '0'), 10) : 0);
            out.push({ start: ms, end: 0, text, words: [] });
        }
    }
    out.sort((a, b) => a.start - b.start);
    for (let i = 0; i < out.length; i++) {
        out[i].end = i + 1 < out.length ? out[i + 1].start : out[i].start + 5000;
    }
    return out;
}

/** 当前时间 t（ms）对应的歌词行索引；t 早于首行时返回 -1 */
function rowIdxAt(t) {
    for (let i = rows.length - 1; i >= 0; i--) {
        if (rows[i].start <= t) return i;
    }
    return -1;
}

/**
 * 计算带传输补偿的原始时间（与主窗口一致）：
 * 后端时间戳与本机同源，补偿事件/IPC 传输耗时（上限 250ms，防时钟调整跳跃）。
 */
function computeRawTimeMs(p) {
    const totalMs = p.total_time_ms && p.total_time_ms > 0
        ? p.total_time_ms
        : (p.total_time || 0) * 1000;
    let rawT = p.current_time_ms && p.current_time_ms > 0
        ? p.current_time_ms
        : (totalMs * ((p.progress_percent || 0) / 100));
    if (p.is_playing && Number.isFinite(p.server_ts) && p.server_ts > 0) {
        rawT += Math.min(250, Math.max(0, Date.now() - p.server_ts));
        if (totalMs > 0) rawT = Math.min(rawT, totalMs);
    }
    return rawT;
}

/**
 * 同首歌的进度同步状态机（镜像主窗口 update-handler 的策略）：
 *  - 淡出尾音：is_playing=true 但未补偿位置冻结 → 钉住显示，暂停 snap 时零回退
 *  - play→pause：直接 snap 服务端权威最终位置
 *  - pause→play：服务端落后 ≤500ms 时本地时间基准向未来平移原地 hold
 *  - 播放中后退 >250ms：判定 seek 立即跟随
 *  - 死区（落后 50~250ms）：容忍抖动，连续 5 次采样不收敛才 snap
 */
function syncProgressBaseline({ isPlaying, rawT, serverPosMs }) {
    const prevPlaying = playing;
    playing = isPlaying;
    const localEstimate = prevPlaying && baseTs > 0
        ? baseT + Math.max(0, performance.now() - baseTs)
        : baseT;

    if (playing !== prevPlaying) {
        behindCount = 0;
        if (!playing) {
            // play → pause：后端已完成淡出建模，携带的就是真实停止点
            baseT = rawT;
            baseTs = performance.now();
        } else {
            // pause → play：原地 hold 落后量，等服务端追平后自然同速前进
            const behindMs = baseT - rawT;
            if (behindMs > 0 && behindMs <= 500) {
                baseTs = performance.now() + behindMs;
            } else {
                baseT = rawT;
                baseTs = performance.now();
            }
        }
        setScrollPaused(!playing);
        lastServerPosMs = serverPosMs;
        return;
    }

    if (!playing) {
        // 暂停稳态：服务端位置恒定且权威，直接跟随（覆盖暂停中 seek）
        baseT = rawT;
        baseTs = performance.now();
        behindCount = 0;
    } else if (serverPosMs > 0 && serverPosMs === lastServerPosMs && localEstimate >= serverPosMs) {
        // 淡出尾音：播放标志仍为 true 但位置不再前进（后端在静音确认期钉住）。
        // 钉住显示并把本地时钟冻结到下一次采样之后：采样间隙（~100ms）内也
        // 不再插值蠕升，暂停 snap 时高亮零回退。位置一旦恢复前进即退出本分支。
        baseT = serverPosMs;
        baseTs = performance.now() + 1500;
        behindCount = 0;
    } else if (rawT < localEstimate - 250) {
        // 播放中后退 >250ms 几乎只会是 seek：立即跟随
        baseT = rawT;
        baseTs = performance.now();
        behindCount = 0;
    } else if (rawT >= localEstimate - 50) {
        // 正常：服务端大致等于或略超前本地估计 → snap
        baseT = rawT;
        baseTs = performance.now();
        behindCount = 0;
    } else {
        // 死区（服务端落后 50~250ms）：多为快照/传输抖动，立即 snap 会回跳；
        // 持续 5 个采样未收敛说明是系统性偏差，必须 snap 回服务端强制收敛
        behindCount++;
        if (behindCount >= 5) {
            behindCount = 0;
            baseT = rawT;
            baseTs = performance.now();
        }
    }
    lastServerPosMs = serverPosMs;
}

/** 事件回调：完整 SongInfo → 重建行数据 */
function onSongInfo(data) {
    const hasValidSong = data.title && data.title !== 'No music playing' && data.title !== 'ERROR';

    if (!hasValidSong) {
        hasSong = false;
        playing = false;
        rows = [];
        transMap = [];
        currentRowIdx = -1;
        currentRowState = '';
        currentRowStart = 0;
        wordEls = [];
        lastShownTrans = '';
        lastRenderKey = '';
        renderFallback('idle');
        return;
    }

    const newRows = (data.qrc_data && data.qrc_data.length > 0)
        ? rowsFromQrc(data.qrc_data)
        : rowsFromLrc(data.lyrics);
    const newTransMap = parseTranslation(data.trans);
    const renderKey = `${data.title}|${data.artist}|${data.qrc_data ? data.qrc_data.length : 0}|${(data.lyrics || '').length}|${(data.trans || '').length}`;

    // 进度基线：切歌/首次加载直接 snap 到服务端（含传输补偿）
    const rawT = computeRawTimeMs(data);
    baseT = rawT;
    baseTs = performance.now();
    playing = !!data.is_playing;
    lastServerPosMs = data.current_time_ms > 0 ? data.current_time_ms : -1;
    behindCount = 0;
    setScrollPaused(!playing);
    hasSong = true;

    // 静态内容未变（如仅专辑图升级触发快照）：只校准基线，不重建 DOM，
    // 避免滚动动画/高亮被无意义地重置
    if (renderKey !== lastRenderKey) {
        lastRenderKey = renderKey;
        rows = newRows;
        transMap = newTransMap;
        currentRowIdx = -1;
        currentRowState = '';
        currentRowStart = 0;
        lastShownTrans = '';
        fallbackMode = null;

        if (rows.length === 0) {
            // 歌词未就绪或纯音乐：显示占位，绝不残留上一首歌词
            const firstPlainLine = (data.lyrics || '')
                .split('\n')
                .map((s) => s.replace(/^\s*\[\d{2}:\d{2}[.:]\d{2,3}\]\s*/g, '').trim())
                .find((s) => s && s !== '//');
            if (firstPlainLine) {
                renderFallback('plain', firstPlainLine);
            } else if ((data.lyrics || '').trim()) {
                renderFallback('pure');
            } else {
                loadingStartedAt = performance.now();
                renderFallback('loading');
            }
        }
    }
}

/** 事件回调：进度事件 → 同策略校准插值基线 */
function onProgress(progress) {
    if (!hasSong) return;
    const serverPosMs = progress.current_time_ms > 0 ? progress.current_time_ms : -1;
    syncProgressBaseline({
        isPlaying: !!progress.is_playing,
        rawT: computeRawTimeMs(progress),
        serverPosMs
    });
}

/** 渲染占位内容（等待播放 / 加载中 / 纯音乐 / 无时间戳纯文本） */
function renderFallback(kind, customText = '') {
    fallbackMode = kind;
    const text = customText || FALLBACK_TEXT[kind] || FALLBACK_TEXT.idle;
    const domUnchanged = text === fallbackText;
    fallbackText = text;

    // 状态复位无条件执行：即使占位文案未变（如 A 歌加载→B 歌加载），
    // 也要清掉上一首歌残留的翻译行/滚动类
    wordEls = [];
    currentRowIdx = -1;
    currentRowState = '';
    currentRowStart = 0;
    lastShownTrans = '';
    els.trans.hidden = true;
    els.inner.classList.remove('scrolling', 'pending', 'done');
    els.inner.style.removeProperty('--scroll-dist');
    els.inner.style.removeProperty('--scroll-dur');
    scrollOverflow = 0;

    if (domUnchanged) return;

    els.inner.innerHTML = '';
    const span = document.createElement('span');
    span.className = 'lyric-bar-text bar-content';
    span.textContent = text;
    els.inner.appendChild(span);
    applyScrollIfNeeded(true);
}

/** 重建当前行 DOM（逐字或纯文本），行状态通过 CSS class 施加 */
function renderRow(row) {
    wordEls = [];
    els.inner.innerHTML = '';
    els.inner.classList.remove('pending', 'done');

    const wrap = document.createElement('span');
    wrap.className = 'bar-content';

    if (row.words && row.words.length > 0) {
        // 逐字模式：每个字两层 span，fg 渐变剪裁
        for (const w of row.words) {
            const wordSpan = document.createElement('span');
            wordSpan.className = 'word';
            const bg = document.createElement('span');
            bg.className = 'word-bg';
            bg.textContent = w.content;
            const fg = document.createElement('span');
            fg.className = 'word-fg';
            fg.textContent = w.content;
            wordSpan.appendChild(bg);
            wordSpan.appendChild(fg);
            wrap.appendChild(wordSpan);
            wordEls.push({ el: fg, start: w.start, dur: w.dur, width: 0 });
        }
    } else {
        // 纯文本模式
        const span = document.createElement('span');
        span.className = 'lyric-bar-text';
        span.textContent = row.text;
        wrap.appendChild(span);
    }

    els.inner.appendChild(wrap);
    applyScrollIfNeeded(true);
    setScrollPaused(!playing);
}

/**
 * 施加行状态：
 *  - pending（前奏）：整行压暗
 *  - active：正常，逐字高亮
 *  - done（行结束/间奏）：整行轻微变暗（与主窗口 before/after 语义一致）
 * 状态切换不重建 DOM，滚动动画不被打断。
 */
function applyRowState(state) {
    currentRowState = state;
    els.inner.classList.toggle('pending', state === 'pending');
    els.inner.classList.toggle('done', state === 'done');
    if (state === 'done') {
        for (const w of wordEls) {
            if (w.width !== 100) {
                w.width = 100;
                w.el.style.width = '100%';
            }
        }
    } else if (state === 'pending') {
        for (const w of wordEls) {
            if (w.width !== 0) {
                w.width = 0;
                w.el.style.width = '0%';
            }
        }
    }
}

/** 更新当前行逐字高亮宽度（量化到 0.5% 防抖） */
function updateWordHighlights(t) {
    for (const w of wordEls) {
        const wEnd = w.start + w.dur;
        let target;
        if (t >= wEnd) target = 100;
        else if (t >= w.start) target = ((t - w.start) / w.dur) * 100;
        else target = 0;
        const quant = Math.round(target * 2) / 2;
        if (quant !== w.width) {
            w.width = quant;
            w.el.style.width = `${quant}%`;
        }
    }
}

/**
 * 歌词宽度超出窗口时启用水平往返滚动。
 * forceRestart：换行时强制回到 phase 0（新行从头显示，不落在滚动中段）；
 * 其余情况仅在溢出量变化时重启，过滤 ResizeObserver 的无关 layout 抖动。
 */
function applyScrollIfNeeded(forceRestart = false) {
    const overflow = els.inner.scrollWidth - els.line.clientWidth;
    if (overflow > 8) {
        els.inner.style.setProperty('--scroll-dist', `${overflow}px`);
        els.inner.style.setProperty(
            '--scroll-dur',
            `${Math.min(12, Math.max(4, overflow * 0.03))}s`
        );
        const distChanged = Math.abs(overflow - scrollOverflow) > 2;
        const active = els.inner.classList.contains('scrolling');
        if (forceRestart || distChanged || !active) {
            scrollOverflow = overflow;
            els.inner.classList.remove('scrolling');
            void els.inner.offsetWidth; // Force reflow to restart the animation at phase 0
            els.inner.classList.add('scrolling');
        }
    } else {
        scrollOverflow = 0;
        els.inner.classList.remove('scrolling');
    }
}

/** 暂停时冻结往返滚动动画 */
function setScrollPaused(paused) {
    els.inner.style.animationPlayState = paused ? 'paused' : 'running';
}

function setTransText(text) {
    if (text === lastShownTrans) return;
    lastShownTrans = text;
    if (text) {
        els.trans.textContent = text;
        els.trans.hidden = false;
    } else {
        els.trans.hidden = true;
    }
}

function tick() {
    requestAnimationFrame(tick);
    if (!hasSong) return;

    if (fallbackMode) {
        // 加载兜底：歌词长时间未到达（如网络失败）时改为纯音乐提示
        if (fallbackMode === 'loading' && performance.now() - loadingStartedAt > 15000) {
            renderFallback('pure');
        }
        return;
    }
    if (rows.length === 0) return;

    const t = playing && baseTs > 0
        ? baseT + Math.max(0, performance.now() - baseTs)
        : baseT;

    // 前奏（t 早于首行）：显示首行 pending；行结束（间奏）：当前行 done
    let idx = rowIdxAt(t);
    let state = 'active';
    if (idx < 0) {
        idx = 0;
        state = 'pending';
    } else if (t >= rows[idx].end) {
        state = 'done';
    }

    if (idx !== currentRowIdx) {
        currentRowIdx = idx;
        currentRowStart = rows[idx].start;
        renderRow(rows[idx]);
        if (showTrans) setTransText(findTranslation(transMap, currentRowStart));
        applyRowState(state);
    } else if (state !== currentRowState) {
        applyRowState(state);
    }

    if (wordEls.length > 0 && state === 'active') updateWordHighlights(t);
}

/** 重读偏好配置：主题 + 翻译开关 + 字号字重 + 所在侧对齐（跟随主窗口设置） */
function applyPreferences() {
    const cfg = loadFrontendConfig();
    applyThemeToDocument(cfg);
    showTrans = cfg.showTranslation;

    // 歌词文字随所在侧对齐：左侧贴着开始按钮、右侧贴着通知区，
    // 消除「文字在 520px 窗口内居中」造成的视觉漂浮感
    const side = cfg.lyricBarSide === 'left' ? 'left' : 'right';
    document.body.classList.toggle('side-left', side === 'left');
    document.body.classList.toggle('side-right', side === 'right');

    // 任务栏高度有限：字号按主窗口偏好 0.6 倍映射并 clamp 到 12~20px
    const barFontSize = Math.min(20, Math.max(12, Math.round((cfg.fontSize || 26) * 0.6)));
    document.documentElement.style.setProperty('--bar-font-size', `${barFontSize}px`);
    document.documentElement.style.setProperty(
        '--bar-font-weight',
        String(Math.min(800, Math.max(300, cfg.fontWeight || 700)))
    );

    // 翻译开关切换立即生效（不等下一行切换）
    if (!showTrans) {
        els.trans.hidden = true;
    } else if (!fallbackMode && currentRowIdx >= 0) {
        setTransText(findTranslation(transMap, currentRowStart));
    }
}

/** 查询任务栏明暗主题（DWM 沉浸式模式），浅色时切换深色文字配色 */
async function refreshTaskbarLightMode() {
    if (!window.__TAURI__) return;
    try {
        const light = await window.__TAURI__.core.invoke('get_taskbar_light_theme');
        document.body.classList.toggle('light', !!light);
    } catch {
        /* 查询失败保持默认深色配色 */
    }
}

/** 页面入口 */
export function initLyricBar() {
    applyPreferences();
    // 水合后端持久化的偏好（跨 origin 权威值）：dev server 端口漂移或
    // dev/exe 的 localStorage origin 不同时，仍能恢复主题/字号/边距等设置
    if (window.__TAURI__) {
        hydrateFrontendConfig().then((cfg) => {
            if (cfg) applyPreferences();
        });
    }
    refreshTaskbarLightMode();
    // 系统主题可能随时切换（自动深色/浅色），低频轮询保持配色同步
    setInterval(refreshTaskbarLightMode, 10000);

    // 主窗口切换主题/字号时实时跟随：
    // 1. Tauri 事件广播（settings 面板发出）
    // 2. localStorage 共享的 storage 事件兜底（同源多窗口同步）
    if (window.__TAURI__) {
        window.__TAURI__.event
            .listen('theme-changed', () => applyPreferences())
            .catch(() => {});
    }
    window.addEventListener('storage', (e) => {
        // 偏好变化或专辑自动取色变化（主窗口提取后广播）都重新应用
        if (e.key === null || e.key === STORAGE_KEY || e.key === AUTO_COLOR_KEY) applyPreferences();
    });

    // 任务栏可用宽度变化（左右位置切换/分辨率/DPI 变化）时自动重测滚动
    if (typeof ResizeObserver !== 'undefined') {
        new ResizeObserver(() => applyScrollIfNeeded(false)).observe(els.line);
    }

    listenSongInfo(onSongInfo, onProgress, () => {});
    requestAnimationFrame(tick);
}

initLyricBar();

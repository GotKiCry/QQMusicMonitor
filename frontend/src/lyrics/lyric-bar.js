/**
 * 状态栏歌词（嵌入任务栏）入口与渲染逻辑。
 *
 * 独立窗口（lyric-bar.html）加载，复用 Tauri 的 song-info /
 * playback-progress 事件通道；本地 rAF 按单调时钟推进时间：
 *  - QRC 逐字歌词：逐字渐变高亮（与主界面一致的扫光效果）
 *  - 无 QRC：整行主题渐变文字（LRC 兜底）
 *  - 歌词宽度超出任务栏可用空间时自动水平往返滚动
 *
 * 时间同步：进度帧交给共享单调播放时钟（state/playback-clock.js）做限速纠偏——
 * 显示位置只按 1× 前进、永不回退/跳变；seek/切歌等大偏差才直接对齐；
 * 暂停冻结显示；WASAPI 门控误报暂停（位置仍在走）时仍按 1× 前进。
 * 行状态区分 pending（前奏）/ active（高亮中，行结束后驻留满格）。
 */

import { listenSongInfo } from '../connection/tauri-bridge.js';
import {
    loadFrontendConfig,
    STORAGE_KEY,
    hydrateFrontendConfig
} from '../config/frontend-config.js';
import { applyThemeToDocument, AUTO_COLOR_KEY, AUTO_PALETTE_KEY, barTextColors } from '../config/theme.js';
import { parseTranslation, findTranslation } from './translation.js';
import { rowsFromQrc, rowsFromLrc, rowIdxAt } from './lines.js';
import { createPlaybackClock, computeRawTimeMs } from '../state/playback-clock.js';

const els = {
    line: document.getElementById('lyric-bar-line'),
    inner: document.getElementById('lyric-bar-inner'),
    trans: document.getElementById('lyric-bar-trans')
};

/** 行数组：{start, end, text, words?: [{content, start, dur}]} */
let rows = [];
let transMap = [];
let showTrans = true;

/** 共享单调播放时钟：进度帧只做 ±20% 限速纠偏，显示位置单调不回退、不跳变 */
const clock = createPlaybackClock();
/** 当前歌曲指纹（title|artist）：仅在真正切歌时重置时钟基线 */
let lastSongKey = '';
let hasSong = false;

/** 当前渲染状态 */
let currentRowIdx = -1;
let currentRowState = '';   // 'pending' | 'active' | 'done'
let currentRowStart = 0;    // 当前行起始时间（翻译行重新匹配用）
let wordEls = [];           // 当前行的逐字 fg 元素 {el, start, dur, width}
let lastShownTrans = '';
let lastRenderKey = '';     // 静态内容指纹：跳过同内容的重复重建
let scrollOverflow = 0;     // 当前行相对窗口的溢出量（px），>0 才需要滚动
let lastScrollX = '';       // 上次写入的 translateX（去抖：位置没变不重复写 style）

/** 占位模式：'idle' | 'loading' | 'pure' | 'plain' | null */
let fallbackMode = null;
let fallbackText = '';
let loadingStartedAt = 0;

/** 当前任务栏明暗：auto 模式由 DWM 检测，手动配置直接指定；失败保持上次值 */
let taskbarLight = false;

/** 最近一次有效歌曲帧的时刻（无效 title 帧抑制用） */
let lastValidAt = 0;

/** 本窗口是否尚未收到过任何 song-info 帧（首帧判定：切歌防护只对非首帧生效） */
let isFirstSongFrame = true;

/** 实际显示的内容指纹（'row:idx' / 'fb:kind:text'）：避免每帧重复重建 DOM */
let displayedContent = '';

const FALLBACK_TEXT = {
    idle: '等待播放',
    loading: '正在加载歌词…',
    pure: '♪ 纯音乐，请欣赏'
};

/** 事件回调：完整 SongInfo → 重建行数据 */
function onSongInfo(data) {
    const firstFrame = isFirstSongFrame;
    isFirstSongFrame = false;
    const hasValidSong = data.title && data.title !== 'No music playing' && data.title !== 'ERROR';

    if (!hasValidSong) {
        // 切歌瞬间 SMTC 会话切换会推几帧无效 title（No music playing / ERROR）：
        // 150ms（≈1~2 个后端采样周期）内出现过有效帧则忽略本次（滤振荡）；
        // 超过 150ms 仍无效才确认切占位——抑制窗口太大会让旧歌歌词
        // 滞留到新歌就绪（切歌第一时间没缓过来），主界面是立即响应的
        if (performance.now() - lastValidAt < 150) return;
        hasSong = false;
        // 时钟不在此处重置：等下一帧有效 song-info（切歌/恢复）时由 clock 判定对齐
        rows = [];
        transMap = [];
        currentRowIdx = -1;
        currentRowState = '';
        currentRowStart = 0;
        wordEls = [];
        lastShownTrans = '';
        lastRenderKey = '';
        setFallback('idle');
        return;
    }
    lastValidAt = performance.now();

    const newRows = (data.qrc_data && data.qrc_data.length > 0)
        ? rowsFromQrc(data.qrc_data)
        : rowsFromLrc(data.lyrics);
    const newTransMap = parseTranslation(data.trans);
    const renderKey = `${data.title}|${data.artist}|${data.qrc_data ? data.qrc_data.length : 0}|${(data.lyrics || '').length}|${(data.trans || '').length}`;

    // 真正切歌（首帧 / title|artist 指纹变化）才套「旧歌 timeline 防护」并重置
    // 时钟：非首帧全量帧带 >5s 大进度/大时长几乎只出现在 SMTC 切歌瞬间——
    // 新歌刚开头不可能处于 >5s 位置。同歌全量帧（封面升级/歌词-翻译晚到）
    // 只交给时钟限速纠偏，绝不清零——旧实现对每个非首帧全量帧都套归零，
    // 播放中途收到快照会把基线打回 0（歌词闪回首行）。
    const songKey = `${data.title}|${data.artist}`;
    const songChanged = firstFrame || songKey !== lastSongKey;
    let rawT = computeRawTimeMs(data);
    if (songChanged && !firstFrame) {
        if (data.total_time_ms > 5000) {
            data.total_time_ms = 0;
            data.total_time = 0;
        }
        rawT = rawT > 5000 ? 0 : rawT;
    }
    const totalMs = data.total_time_ms > 0
        ? data.total_time_ms
        : (data.total_time || 0) * 1000;
    if (songChanged) {
        lastSongKey = songKey;
        clock.reset({ songKey, rawMs: rawT, isPlaying: !!data.is_playing });
    } else {
        clock.update({
            songKey: lastSongKey,
            rawMs: rawT,
            isPlaying: !!data.is_playing,
            totalMs
        });
    }
    hasSong = true;

    // 静态内容未变（如仅专辑图升级触发快照）：只交给时钟校准，不重建 DOM，
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
        // 内容指纹跨歌重置：displayedContent 只记 'row:idx'，旧歌与新歌都从
        // row:0 开始 → 指纹相等 → tick 跳过首行 DOM 重建 → 旧歌第一句驻留
        // （切歌「首句残留」bug；已缓存歌无 loading 占位缓冲，必现）。
        wordEls = [];
        displayedContent = '';

        if (rows.length === 0) {
            // 歌词未就绪或纯音乐：设置占位状态（DOM 由 tick 在指纹变化时渲染），
            // 绝不残留上一首歌词
            const firstPlainLine = (data.lyrics || '')
                .split('\n')
                .map((s) => s.replace(/^\s*\[\d{2}:\d{2}[.:]\d{2,3}\]\s*/g, '').trim())
                .find((s) => s && s !== '//');
            if (firstPlainLine) {
                setFallback('plain', firstPlainLine);
            } else if ((data.lyrics || '').trim()) {
                setFallback('pure');
            } else {
                setFallback('loading');
            }
        }
    }
}

/** 事件回调：进度帧 → 共享单调时钟限速纠偏（不直接 snap 到服务端） */
function onProgress(progress) {
    if (!hasSong) return;
    const totalMs = progress.total_time_ms > 0
        ? progress.total_time_ms
        : (progress.total_time || 0) * 1000;
    clock.update({
        songKey: lastSongKey,
        rawMs: computeRawTimeMs(progress),
        isPlaying: !!progress.is_playing,
        totalMs
    });
}

/** 设置占位状态（等待播放 / 加载中 / 纯音乐 / 无时间戳纯文本）；DOM 由 tick 在指纹变化时渲染 */
function setFallback(kind, customText = '') {
    fallbackMode = kind;
    fallbackText = customText || FALLBACK_TEXT[kind] || FALLBACK_TEXT.idle;
    if (kind === 'loading') loadingStartedAt = performance.now();
}

/** 渲染占位 DOM（仅在内容指纹变化时执行一次） */
function applyFallbackDom() {
    // 状态复位：清掉上一首歌残留的翻译行/滚动偏移/行状态
    wordEls = [];
    currentRowIdx = -1;
    currentRowState = '';
    currentRowStart = 0;
    lastShownTrans = '';
    els.trans.hidden = true;
    els.inner.classList.remove('pending', 'done');
    els.inner.style.transform = '';
    scrollOverflow = 0;

    els.inner.innerHTML = '';
    const span = document.createElement('span');
    span.className = 'lyric-bar-text bar-content';
    span.textContent = fallbackText;
    els.inner.appendChild(span);
    measureScroll();
}

/** 重建当前行 DOM（逐字或纯文本），行状态通过 CSS class 施加 */
function renderRow(row) {
    // 换行过渡：旧行克隆到独立的绝对定位层做上滑退出（克隆体连同 inner 的
    // 横向滚动偏移一起保留），新行从下方滑入——垂直滚动式换行。
    if (els.inner.firstElementChild) {
        const layer = document.createElement('div');
        layer.className = 'row-leaving';
        const ghost = els.inner.cloneNode(true);
        ghost.removeAttribute('id');
        // 克隆体只做退出动画：清掉进入/淡入动画，避免重复播放
        ghost.querySelectorAll('.bar-content, .lyric-bar-trans').forEach((el) => {
            el.style.animation = 'none';
            el.classList.remove('row-entering');
        });
        layer.appendChild(ghost);
        els.line.appendChild(layer);
        setTimeout(() => layer.remove(), 360);
    }

    wordEls = [];
    els.inner.innerHTML = '';
    els.inner.classList.remove('pending', 'done');

    const wrap = document.createElement('span');
    wrap.className = 'bar-content row-entering';

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
            fg.style.clipPath = 'inset(0 100% 0 0)';
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
    // 换行回到行首：先清掉上一行的滚动偏移再重新测量
    lastScrollX = '';
    els.inner.style.transform = '';
    measureScroll();
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
                w.el.style.clipPath = 'inset(0 0 0 0)';
            }
        }
    } else if (state === 'pending') {
        for (const w of wordEls) {
            if (w.width !== 0) {
                w.width = 0;
                w.el.style.clipPath = 'inset(0 100% 0 0)';
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
            // clip-path 裁剪（inset 右侧切入），比 width 写入不触发布局
            w.el.style.clipPath = `inset(0 ${(100 - quant).toFixed(1)}% 0 0)`;
        }
    }
}

/**
 * 测量当前行相对窗口的溢出量（px）：决定是否需要水平滚动。
 * 换行、窗口宽度变化（后端按任务栏可用空间重定位）后都要重测。
 */
function measureScroll() {
    // 用 .bar-content 测量：inner 受 max-width: 100% 限制，内容恒定从
    // 窗口左缘向右溢出，row 元素自身的 scrollWidth 才是内容真实宽度
    const rowEl = els.inner.firstElementChild;
    const contentW = rowEl ? rowEl.scrollWidth : els.inner.scrollWidth;
    const overflow = contentW - els.line.clientWidth;
    scrollOverflow = overflow > 8 ? overflow : 0;
    if (scrollOverflow === 0) {
        lastScrollX = '';
        els.inner.style.transform = '';
    }
}

/**
 * 按当前行进度推进水平滚动：滚动位置直接跟随播放进度，行结束时正好
 * 滚到行尾——固定时长的 CSS 动画在短行上会「追不上进度」。
 *  - 行内前 10% 停留：先完整显示行首
 *  - 10%→90%：匀速滚到行尾（贴通知区）
 *  - 90%→行结束：停在行尾
 * 暂停时 clock 位置冻结，滚动随之冻结；seek 时直接跟到新位置。
 */
function updateScroll(t, row) {
    if (scrollOverflow <= 0 || !row) return;
    const span = Math.max(1, (row.end || row.start + 4000) - row.start);
    const p = Math.min(1, Math.max(0, (t - row.start) / span));
    const q = Math.min(1, Math.max(0, (p - 0.1) / 0.8));
    const x = (-scrollOverflow * q).toFixed(1);
    if (x === lastScrollX) return;
    lastScrollX = x;
    els.inner.style.transform = 'translateX(' + x + 'px)';
}

function setTransText(text) {
    if (text === lastShownTrans) return;
    lastShownTrans = text;
    if (text) {
        els.trans.textContent = text;
        els.trans.hidden = false;
        // 翻译行跟随歌词行做同向的「从下滑入」，换行时整体是一体的
        els.trans.classList.remove('row-entering');
        void els.trans.offsetWidth;
        els.trans.classList.add('row-entering');
    } else {
        els.trans.hidden = true;
    }
}

function tick() {
    requestAnimationFrame(tick);

    // —— 目标内容推导（行 / 占位）——
    // t 直接取单调时钟的当前显示位置：只按 1× 推进 + 限速纠偏，永不回退，
    // 目标行因此单调前进，不需要任何换行防抖，指纹变化即重建。
    let target = '';
    const t = clock.position();
    if (fallbackMode) {
        // 加载兜底：歌词长时间未到达（如网络失败）时改为纯音乐提示
        if (fallbackMode === 'loading' && performance.now() - loadingStartedAt > 15000) {
            setFallback('pure');
        }
        target = 'fb:' + fallbackMode + ':' + fallbackText;
    } else if (hasSong && rows.length > 0) {
        // 前奏（t 早于首行）：目标为首行（pending）；
        // 行结束（间奏/暂停恢复）：维持本行满格高亮直到下一行开始（驻留）
        let idx = rowIdxAt(rows, t);
        if (idx < 0) idx = 0;
        target = 'row:' + idx;
    }

    // —— 内容切换：目标单调，指纹变化立即重建 DOM ——
    if (target && target !== displayedContent) {
        displayedContent = target;
        if (target.startsWith('row:')) {
            const idx = parseInt(target.slice(4), 10);
            currentRowIdx = idx;
            currentRowStart = rows[idx].start;
            renderRow(rows[idx]);
            if (showTrans) setTransText(findTranslation(transMap, currentRowStart));
            applyRowState(rowStateAt(t, idx));
        } else {
            applyFallbackDom();
        }
    }

    // —— 行状态跟随与逐字高亮（显示行确定后每帧执行）——
    if (!fallbackMode && displayedContent.startsWith('row:')) {
        const idx = parseInt(displayedContent.slice(4), 10);
        if (idx >= 0 && idx < rows.length) {
            const st = rowStateAt(t, idx);
            if (st !== currentRowState) applyRowState(st);
            if (wordEls.length > 0 && st === 'active') updateWordHighlights(t);
            // 超长行的水平滚动跟随行内进度（行结束前滚到行尾）
            updateScroll(t, rows[idx]);
        }
    }
}

/** 行状态：前奏（t 早于行开始）为 pending；否则 active（行结束后驻留满格高亮） */
function rowStateAt(t, idx) {
    return t < rows[idx].start ? 'pending' : 'active';
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
    // 字重上限 600（用户 2026-10：去掉描边后 Regular 太细、锯齿感重）：
    // 雅黑只有 400/700 字面，600 按 CSS 匹配规则落到 Bold；描边/光晕已删，
    // Bold 的笔画更实，锯齿感明显减轻（若后续觉得糊可回 500 或改极细描边）
    document.documentElement.style.setProperty(
        '--bar-font-weight',
        String(Math.min(600, Math.max(300, cfg.fontWeight || 700)))
    );

    // 任务栏明暗：手动配置直接生效（无需求 DWM，可靠）
    if (cfg.taskbarTheme === 'light') taskbarLight = true;
    else if (cfg.taskbarTheme === 'dark') taskbarLight = false;
    applyTaskbarAppearance();

    // 翻译开关切换立即生效（不等下一行切换）
    if (!showTrans) {
        els.trans.hidden = true;
    } else if (!fallbackMode && currentRowIdx >= 0) {
        setTransText(findTranslation(transMap, currentRowStart));
    }
}

/**
 * 按任务栏明暗把主题落成歌词文字变量。
 * 文字基准色取 --theme-ink（auto 调色板输出，主窗口歌词高亮同一个 ink，
 * 保证"两边同色"）；预设主题没有 --theme-ink 时回退 --theme-primary。
 * 深色任务栏对 ink 再保色相提亮到 4.5:1，浅色任务栏作向黑适配。
 */
function applyTaskbarAppearance() {
    const cs = getComputedStyle(document.body);
    const primary = (cs.getPropertyValue('--theme-primary') || '').trim() || '#14b8a6';
    const ink = (cs.getPropertyValue('--theme-ink') || '').trim() || primary;
    const gradient = (cs.getPropertyValue('--theme-gradient') || '').trim();
    const { fg, gradA, gradB, dim } = barTextColors(gradient, taskbarLight, ink);
    document.documentElement.style.setProperty('--bar-fg-color', fg);
    // 未唱底字：高亮色的同色系暗色（不再用白色，避免高亮字半透明边缘透白）
    document.documentElement.style.setProperty('--bar-bg-color', dim);
    document.documentElement.style.setProperty(
        '--bar-text-gradient',
        `linear-gradient(135deg, ${gradA}, ${gradB})`
    );
    document.body.classList.toggle('light', taskbarLight);
}

/**
 * 查询任务栏明暗主题（DWM 沉浸式模式），浅色时切换深色文字配色。
 * 手动配置（taskbarTheme: dark/light）直接生效，不查询系统；
 * auto 模式查询失败时保持上次成功状态，不猜测——避免浅色任务栏上
 * 白字+白描边彻底不可见（轮询会持续重试直至成功）。
 */
async function refreshTaskbarLightMode() {
    const cfg = loadFrontendConfig();
    if (cfg.taskbarTheme === 'dark') {
        taskbarLight = false;
        applyTaskbarAppearance();
        return;
    }
    if (cfg.taskbarTheme === 'light') {
        taskbarLight = true;
        applyTaskbarAppearance();
        return;
    }
    if (!window.__TAURI__) {
        applyTaskbarAppearance();
        return;
    }
    try {
        const light = await window.__TAURI__.core.invoke('get_taskbar_light_theme');
        if (typeof light === 'boolean') taskbarLight = light;
    } catch {
        /* 查询失败：保持上次成功状态，下轮轮询重试 */
    }
    applyTaskbarAppearance();
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
    setInterval(refreshTaskbarLightMode, 2000);

    // 主窗口切换主题/字号时实时跟随：
    // 1. Tauri 事件广播（settings 面板发出）
    // 2. localStorage 共享的 storage 事件兜底（同源多窗口同步）
    if (window.__TAURI__) {
        window.__TAURI__.event
            .listen('theme-changed', () => applyPreferences())
            .catch(() => {});
    }
    window.addEventListener('storage', (e) => {
        // 偏好变化或专辑自动调色板变化（主窗口提取后广播）都重新应用
        if (
            e.key === null ||
            e.key === STORAGE_KEY ||
            e.key === AUTO_COLOR_KEY ||
            e.key === AUTO_PALETTE_KEY
        ) {
            applyPreferences();
        }
    });

    // 任务栏可用宽度变化（左右位置切换/分辨率/DPI 变化）时自动重测滚动
    if (typeof ResizeObserver !== 'undefined') {
        new ResizeObserver(() => measureScroll()).observe(els.line);
    }

    listenSongInfo(onSongInfo, onProgress, () => {});
    requestAnimationFrame(tick);
}

initLyricBar();

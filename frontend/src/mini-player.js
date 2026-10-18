/**
 * 迷你播放器（左键点击状态栏歌词弹出的小窗口）。
 *
 * 内容：专辑封面全幅背景、歌曲信息、平滑进度条、播放控制
 * （上一曲 / 播放暂停 / 下一曲）。
 *
 * 复用 Tauri 的 song-info / playback-progress 事件通道与共享单调播放时钟
 * （state/playback-clock.js）：进度条按本地单调时钟 1× 推进、限速纠偏，
 * 与主窗口 / 状态栏歌词条的时间策略一致；失焦后由后端自动隐藏（flyout）。
 */

import { listenSongInfo, mediaControl } from './connection/tauri-bridge.js';
import {
    loadFrontendConfig,
    STORAGE_KEY,
    hydrateFrontendConfig
} from './config/frontend-config.js';
import { applyThemeToDocument, AUTO_COLOR_KEY, AUTO_PALETTE_KEY } from './config/theme.js';
import { createPlaybackClock, computeRawTimeMs } from './state/playback-clock.js';
import { $, setTextIfChanged, preloadAlbumArt, resetAlbumArt } from './utils/dom.js';
import { formatMs } from './utils/format.js';

const els = {
    card: $('mp-card'),
    art: $('mp-art'),
    title: $('mp-title'),
    artist: $('mp-artist'),
    album: $('mp-album'),
    fill: $('mp-fill'),
    timeCur: $('mp-time-cur'),
    timeTotal: $('mp-time-total'),
    btnPrev: $('mp-prev'),
    btnPlay: $('mp-play'),
    btnNext: $('mp-next'),
    iconPlay: $('mp-icon-play'),
    iconPause: $('mp-icon-pause')
};

/** preloadAlbumArt/resetAlbumArt 约定 els.albumArt 为封面容器 */
const artEls = { albumArt: els.art };

/** 共享单调播放时钟：显示位置单调不回退、不跳变 */
const clock = createPlaybackClock();
/** 当前歌曲指纹（title|artist）：仅在真正切歌时重置时钟基线 */
let lastSongKey = '';
let hasSong = false;
/** 本窗口是否尚未收到过任何 song-info 帧（首帧不套切歌归零启发式） */
let isFirstSongFrame = true;
/** 最近一次有效歌曲帧的时刻（无效 title 帧抑制用） */
let lastValidAt = 0;
/** 最近一次后端帧给出的总时长（ms），进度条分母 */
let totalMsState = 0;

/** 事件回调：完整 SongInfo → 更新元信息、封面与时钟 */
function onSongInfo(data) {
    const firstFrame = isFirstSongFrame;
    isFirstSongFrame = false;
    const hasValidSong = data.title && data.title !== 'No music playing' && data.title !== 'ERROR';

    if (!hasValidSong) {
        // 切歌瞬间 SMTC 会话切换会推几帧无效 title：150ms 内出现过有效帧
        // 则忽略（滤振荡），超过才确认切占位（与状态栏歌词条一致）
        if (performance.now() - lastValidAt < 150) return;
        hasSong = false;
        totalMsState = 0;
        setTextIfChanged(els.title, '等待播放');
        setTextIfChanged(els.artist, 'QQMusic Monitor');
        setTextIfChanged(els.album, '');
        resetAlbumArt(artEls);
        syncArtVisuals();
        setPlayingState(false);
        return;
    }
    lastValidAt = performance.now();

    const songKey = `${data.title}|${data.artist}`;
    const songChanged = firstFrame || songKey !== lastSongKey;
    let rawT = computeRawTimeMs(data);

    // 真正切歌才套「旧歌 timeline 防护」（新歌不可能处于 >5s 位置）并重置
    // 时钟；同歌全量帧（封面升级等）只交给时钟限速纠偏，绝不清零。
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
    totalMsState = totalMs;
    if (songChanged) {
        lastSongKey = songKey;
        clock.reset({ songKey, rawMs: rawT, isPlaying: !!data.is_playing });
    } else {
        clock.update({ songKey: lastSongKey, rawMs: rawT, isPlaying: !!data.is_playing, totalMs });
    }
    hasSong = true;
    setPlayingState(!!data.is_playing);

    // —— 元信息与封面 ——
    setTextIfChanged(els.title, data.title || '等待播放');
    setTextIfChanged(els.artist, data.artist || '未知歌手');
    setTextIfChanged(els.album, data.album || '');
    if (data.album_pic_url) {
        preloadAlbumArt(artEls, data.album_pic_url, data.album_pic_hd_url);
    } else if (songChanged) {
        resetAlbumArt(artEls);
    }
    syncArtVisuals();
}

/** 事件回调：进度帧 → 共享单调时钟限速纠偏 */
function onProgress(progress) {
    if (!hasSong) return;
    const totalMs = progress.total_time_ms > 0
        ? progress.total_time_ms
        : (progress.total_time || 0) * 1000;
    totalMsState = totalMs;
    clock.update({
        songKey: lastSongKey,
        rawMs: computeRawTimeMs(progress),
        isPlaying: !!progress.is_playing,
        totalMs
    });
    setPlayingState(!!progress.is_playing);
}

/** 播放/暂停图标切换 */
function setPlayingState(playing) {
    els.iconPlay.classList.toggle('hidden', playing);
    els.iconPause.classList.toggle('hidden', !playing);
}

/** 封面状态同步：has-art 切换 scrim 模式（MutationObserver 驱动） */
function syncArtVisuals() {
    els.card.classList.toggle('has-art', !!els.art.style.backgroundImage);
}

/** 进度条与时间标签（rAF 每帧；写入前做变化检测避免无谓 reflow） */
let lastFillPct = -1;
function renderProgress(t) {
    const pct = totalMsState > 0 ? Math.min(100, (t / totalMsState) * 100) : 0;
    const quant = Math.round(pct * 20) / 20; // 量化到 0.05% 防抖
    if (quant !== lastFillPct) {
        lastFillPct = quant;
        els.fill.style.width = `${quant}%`;
        els.card.classList.toggle('has-progress', quant > 0);
    }
    setTextIfChanged(els.timeCur, formatMs(totalMsState > 0 ? Math.min(t, totalMsState) : t));
    setTextIfChanged(els.timeTotal, formatMs(totalMsState));
}

function tick() {
    requestAnimationFrame(tick);
    renderProgress(clock.position());
}

/** 重读偏好配置：主题（跟随主窗口设置） */
function applyPreferences() {
    applyThemeToDocument(loadFrontendConfig());
}

/** 播放控制按钮 → 既有媒体控制命令 */
function bindControls() {
    els.btnPrev.addEventListener('click', () => mediaControl('previous'));
    els.btnPlay.addEventListener('click', () => mediaControl('play_pause'));
    els.btnNext.addEventListener('click', () => mediaControl('next'));
}

/** 退场动画时长（ms）：与 CSS mp-slide-out 一致，播完回执后端真正隐藏窗口 */
const EXIT_ANIM_MS = 190;
let hideTimer = null;

/** 播退场动画并回执后端隐藏（hide-request 事件与留白区点击共用） */
function playExitAndHide() {
    if (hideTimer || els.card.classList.contains('mp-exit')) return; // 已在退场中
    els.card.classList.remove('mp-enter');
    void els.card.offsetWidth;
    els.card.classList.add('mp-exit');
    hideTimer = setTimeout(() => {
        hideTimer = null;
        window.__TAURI__.core.invoke('hide_mini_player').catch(() => {});
    }, EXIT_ANIM_MS);
}

/**
 * 显隐动画编排：窗口创建一次后 hide/show 复用，CSS 动画不能靠加载触发——
 * 后端 show 时发 mini-player-show（载荷 from_below 指明滑动方向：卡片永远从
 * 歌词条一侧滑入/滑出），隐藏前发 mini-player-hide-request（播退场后回执；
 * 后端另有 500ms 兜底强制收起）。
 */
function bindVisibilityAnimations() {
    if (!window.__TAURI__) return;
    const { listen } = window.__TAURI__.event;
    listen('mini-player-show', (event) => {
        // 卡片在歌词条上方（常规底部任务栏）→ 从下方滑入；在下方 → 从上方滑入
        const fromBelow = !event.payload || event.payload.from_below !== false;
        els.card.style.setProperty('--mp-from', fromBelow ? '26px' : '-26px');
        // 退场动画中被重新打开：取消待回执，直接切回入场
        if (hideTimer) {
            clearTimeout(hideTimer);
            hideTimer = null;
        }
        // 两个类都要移除：mp-enter 已在类表上时重复 add 不会重启动画
        els.card.classList.remove('mp-exit', 'mp-enter');
        void els.card.offsetWidth; // 强制 reflow 以重触发入场动画
        els.card.classList.add('mp-enter');
    }).catch(() => {});
    listen('mini-player-hide-request', () => playExitAndHide()).catch(() => {});
    // 窗口不透明：透明留白区会拦截点击但不触发失焦——补「点留白 = 点外部」
    document.addEventListener('click', (e) => {
        if (!e.target.closest('.mp-card')) playExitAndHide();
    });
}

/** 页面入口 */
function initMiniPlayer() {
    applyPreferences();
    bindControls();
    bindVisibilityAnimations();
    syncArtVisuals();
    // 在线封面异步加载完成才写入 backgroundImage：监听 style 属性即时同步
    new MutationObserver(syncArtVisuals).observe(els.art, {
        attributes: true,
        attributeFilter: ['style']
    });
    // 水合后端持久化的偏好（跨 origin 权威值）
    if (window.__TAURI__) {
        hydrateFrontendConfig().then((cfg) => {
            if (cfg) applyPreferences();
        });
        window.__TAURI__.event
            .listen('theme-changed', () => applyPreferences())
            .catch(() => {});
    }
    window.addEventListener('storage', (e) => {
        if (
            e.key === null ||
            e.key === STORAGE_KEY ||
            e.key === AUTO_COLOR_KEY ||
            e.key === AUTO_PALETTE_KEY
        ) {
            applyPreferences();
        }
    });

    listenSongInfo(onSongInfo, onProgress, () => {});
    requestAnimationFrame(tick);
}

initMiniPlayer();

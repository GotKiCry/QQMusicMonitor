/**
 * QQMusic Monitor - 前端入口
 *
 * 模块职责划分：
 *  - config/       前后端配置的加载、持久化、同步
 *  - connection/   WebSocket 与 Tauri event 桥接
 *  - state/        全局运行时状态 + SongInfo 更新处理 + 切歌重置
 *  - lyrics/       翻译解析与歌词 DOM 构建
 *  - render/       60fps 渲染循环（插值、逐字扫光、居中滚动）
 *  - settings/     设置抽屉交互
 *  - utils/        时间格式化、DOM 小工具
 */

import { $, bindElements } from './utils/dom.js';
import { loadFrontendConfig, hydrateFrontendConfig } from './config/frontend-config.js';
import { fetchBackendConfig, getDefaultBackendConfig } from './config/backend-config.js';
import { connect, disconnect, updateConnectionUI, sendCommand } from './connection/ws.js';
import { listenSongInfo, isTauri, mediaControl, windowControl, isWindowMaximized, onWindowResized, startWindowDragging } from './connection/tauri-bridge.js';
import {
    animFrame, songInfo, setAnimFrame, setLastSyncMs,
    setIsIntentionalClose
} from './state/state.js';
import { handleSongInfoUpdate } from './state/update-handler.js';
import { startRenderLoop } from './render/render-loop.js';
import { wireUpSettings, syncConfigToUI } from './settings/settings-ui.js';

/* ============================================================
 * DOM 绑定
 * ============================================================ */

const els = bindElements({
    // 播放区
    songTitle: 'song-title',
    songArtist: 'song-artist',
    songAlbum: 'song-album',
    progressFill: 'progress-fill',
    timeCurrent: 'time-current',
    timeTotal: 'time-total',
    albumArt: 'album-art',
    // 歌词区
    lyricsContainer: 'lyrics-container',
    lyricsViewport: 'lyrics-viewport',
    // 头部
    statusDot: 'status-dot',
    statusText: 'status-text',
    btnSettings: 'btn-settings',
    topHeader: 'top-header',
    // 窗口控制
    windowControls: 'window-controls',
    btnWinMin: 'btn-win-min',
    btnWinMax: 'btn-win-max',
    btnWinClose: 'btn-win-close',
    iconWinState: 'icon-win-state',
    // 设置抽屉
    settingsOverlay: 'settings-overlay',
    btnSettingsClose: 'btn-settings-close',
    // 后端设置
    cfgOffset: 'cfg-offset',
    valOffset: 'val-offset',
    btnOffsetDec: 'btn-offset-dec',
    btnOffsetInc: 'btn-offset-inc',
    cfgInterval: 'cfg-interval',
    valInterval: 'val-interval',
    cfgPort: 'cfg-port',
    btnApplyPort: 'btn-apply-port',
    // 前端设置
    cfgFontSize: 'cfg-font-size',
    valFontSize: 'val-font-size',
    cfgFontWeight: 'cfg-font-weight',
    valFontWeight: 'val-font-weight',
    toggleTrans: 'toggle-trans',
    toggleLyricBar: 'toggle-lyric-bar',
    toggleDebug: 'toggle-debug',
    btnLyricSideLeft: 'btn-lyric-side-left',
    btnLyricSideRight: 'btn-lyric-side-right',
    btnTaskbarThemeAuto: 'btn-taskbar-theme-auto',
    btnTaskbarThemeDark: 'btn-taskbar-theme-dark',
    btnTaskbarThemeLight: 'btn-taskbar-theme-light',
    cfgLyricBarGap: 'cfg-lyric-bar-gap',
    valLyricBarGap: 'val-lyric-bar-gap',
    themeDotCustom: 'theme-dot-custom',
    customColor: 'custom-color',
    // 调试
    debugPanel: 'debug-panel',
    debugInfo: 'debug-info',
    rawData: 'raw-data',
    // 媒体控制按钮
    btnPrev: 'btn-prev',
    btnPlayPause: 'btn-play-pause',
    btnNext: 'btn-next',
    iconPlay: 'icon-play',
    iconPause: 'icon-pause',
    mediaControls: 'media-controls'
});

/* ============================================================
 * 媒体控制按钮
 * ============================================================ */

function triggerMediaControl(action) {
    if (isTauri()) {
        mediaControl(action);
    } else {
        sendCommand(action);
    }
}

els.btnPrev.addEventListener('click', () => triggerMediaControl('previous'));
els.btnNext.addEventListener('click', () => triggerMediaControl('next'));
els.btnPlayPause.addEventListener('click', () => triggerMediaControl('play_pause'));

/* ============================================================
 * 窗口控制按钮（无边框窗口：最小化 / 最大化 / 关闭）
 * ============================================================ */

/** 单一 SVG 图标：按最大化状态替换内部形状（不同时存在两套图标） */
const WIN_ICON_MAX =
    '<rect x="5" y="5" width="14" height="14" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.8"/>';
const WIN_ICON_RESTORE =
    '<path d="M8 8V5.5A1.5 1.5 0 0 1 9.5 4h9A1.5 1.5 0 0 1 20 5.5v9a1.5 1.5 0 0 1-1.5 1.5H16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/>' +
    '<rect x="4" y="8" width="12" height="12" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.8"/>';

/** 根据最大化状态切换图标 */
function updateMaximizeIcon(maximized) {
    els.iconWinState.innerHTML = maximized ? WIN_ICON_RESTORE : WIN_ICON_MAX;
    els.btnWinMax.title = maximized ? '还原' : '最大化';
}

function wireUpWindowControls() {
    if (!isTauri()) {
        // 浏览器调试环境：隐藏窗口控制按钮组
        els.windowControls.style.display = 'none';
        return;
    }

    els.btnWinMin.addEventListener('click', () => windowControl('minimize'));
    els.btnWinMax.addEventListener('click', () => windowControl('maximize'));
    els.btnWinClose.addEventListener('click', () => windowControl('close'));

    isWindowMaximized().then(updateMaximizeIcon);
    onWindowResized(updateMaximizeIcon);

    // 自定义标题栏拖动：顶栏空白区域可拖动窗口，按钮保持可点击；双击切换最大化
    els.topHeader.addEventListener('mousedown', (event) => {
        if (event.buttons !== 1) return;
        if (event.target.closest('button, input, select, a, label')) return;
        if (event.detail === 2) {
            windowControl('maximize');
        } else {
            startWindowDragging();
        }
    });
}

/**
 * 根据播放状态切换播放/暂停图标
 * @param {boolean} playing
 */
function updatePlayPauseIcon(playing) {
    if (playing) {
        els.iconPlay.classList.add('hidden');
        els.iconPause.classList.remove('hidden');
    } else {
        els.iconPlay.classList.remove('hidden');
        els.iconPause.classList.add('hidden');
    }
}

/* ============================================================
 * 配置加载
 * ============================================================ */

let frontConfig = loadFrontendConfig();
let backConfig = getDefaultBackendConfig();

/** 调试面板用的同步耗时引用（按引用传递给 update handler） */
const lastSyncMsRef = { val: 0 };

/* ============================================================
 * 消息处理统一入口
 * ============================================================ */

function onMessage(data) {
    const t0 = performance.now();
    handleSongInfoUpdate(els, data, frontConfig, backConfig, lastSyncMsRef);
    lastSyncMsRef.val = performance.now() - t0;
    setLastSyncMs(lastSyncMsRef.val);
    if (typeof data.is_playing === 'boolean') {
        updatePlayPauseIcon(data.is_playing);
    }
}

function onPlaybackProgress(progress) {
    if (!songInfo) return;
    onMessage({ ...songInfo, ...progress });
}

/* ============================================================
 * 渲染循环启动
 * ============================================================ */

function startRendering() {
    if (!animFrame) {
        startRenderLoop(
            els.timeCurrent, els.timeTotal, els.progressFill,
            els.lyricsContainer, els.lyricsViewport
        );
    }
}

/* ============================================================
 * 前端偏好变化回调：翻译开关切换时重显现有翻译行
 * ============================================================ */

function onFrontConfigChange() {
    const transDivs = els.lyricsViewport.querySelectorAll('.lyric-trans');
    transDivs.forEach((div) => {
        /** @type {HTMLElement} */ (div).style.display = frontConfig.showTranslation ? 'block' : 'none';
    });
}

/* ============================================================
 * 启动流程
 * ============================================================ */

// 1. 初始 UI 同步
syncConfigToUI(els, frontConfig, backConfig);

// 2. 绑定设置面板交互
wireUpSettings({
    els,
    configs: { front: frontConfig, back: backConfig },
    onFrontConfigChange
});

// 2.5 窗口控制（无边框窗口）
wireUpWindowControls();

// 3. 加载后端配置并启动数据源
if (isTauri()) {
    // Tauri 环境：优先用 event 监听（主路径）
    fetchBackendConfig().then((cfg) => {
        Object.assign(backConfig, cfg);
        syncConfigToUI(els, frontConfig, backConfig);
    });
    listenSongInfo(onMessage, onPlaybackProgress, startRendering);
    updateConnectionUI('connected', els.statusDot, els.statusText);
} else {
    // 浏览器/独立调试环境：用 WebSocket 连接后端
    fetchBackendConfig().then((cfg) => {
        Object.assign(backConfig, cfg);
        syncConfigToUI(els, frontConfig, backConfig);
        connect({
            port: backConfig.port,
            elStatusDot: els.statusDot,
            elStatusText: els.statusText,
            onMessage,
            onConnected: startRendering,
            onDisconnected: () => {
                if (animFrame) { cancelAnimationFrame(animFrame); setAnimFrame(0); }
            }
        });
    });
}

// 4. 卸载时清理 WebSocket
window.addEventListener('beforeunload', () => {
    setIsIntentionalClose(true);
    disconnect();
});

/**
 * 恢复状态栏歌词窗口（按偏好：开关/位置/边距）。
 * 位置与边距随开关一次下发：三次并发 invoke 时后端创建窗口与
 * set_lyric_bar_side / _gap 的先后顺序不确定，首次定位会读到旧值
 * （默认右侧、0 边距），表现为启动恢复后歌词条位置与设置不符。
 */
function restoreLyricBar(cfg) {
    if (!isTauri() || !cfg.lyricBar) return;
    window.__TAURI__.core.invoke('set_lyric_bar_enabled', {
        enabled: true,
        side: cfg.lyricBarSide,
        gap: cfg.lyricBarGap || 0
    }).catch((e) => {
        console.error('Failed to restore lyric bar:', e);
    });
}

// 5. 水合后端持久化的前端偏好（跨 origin 权威值），随后恢复歌词条。
// dev server 端口漂移 / dev 与 exe 的 origin 不同会导致 localStorage 读不到，
// 以后端文件为准；浏览器调试环境直接按 localStorage 恢复。
if (isTauri()) {
    hydrateFrontendConfig().then((cfg) => {
        if (cfg) {
            Object.assign(frontConfig, cfg);
            syncConfigToUI(els, frontConfig, backConfig);
        }
        restoreLyricBar(frontConfig);
    });
}

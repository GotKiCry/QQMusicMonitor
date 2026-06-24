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
import { loadFrontendConfig } from './config/frontend-config.js';
import { fetchBackendConfig, getDefaultBackendConfig } from './config/backend-config.js';
import { connect, disconnect, updateConnectionUI } from './connection/ws.js';
import { listenSongInfo, notifyBackgroundState, isTauri } from './connection/tauri-bridge.js';
import {
    animFrame, setAnimFrame, setLastSyncMs,
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
    albumBlurBg: 'album-blur-bg',
    // 歌词区
    lyricsContainer: 'lyrics-container',
    lyricsViewport: 'lyrics-viewport',
    // 头部
    statusDot: 'status-dot',
    statusText: 'status-text',
    btnSettings: 'btn-settings',
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
    chkOutTxt: 'chk-out-txt',
    chkOutJson: 'chk-out-json',
    chkOutLyric: 'chk-out-lyric',
    // 前端设置
    cfgFontSize: 'cfg-font-size',
    valFontSize: 'val-font-size',
    cfgFontWeight: 'cfg-font-weight',
    valFontWeight: 'val-font-weight',
    toggleTrans: 'toggle-trans',
    toggleDebug: 'toggle-debug',
    // 调试
    debugPanel: 'debug-panel',
    debugInfo: 'debug-info',
    rawData: 'raw-data'
});

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
}

/* ============================================================
 * 渲染循环启动
 * ============================================================ */

function startRendering() {
    if (!animFrame) {
        startRenderLoop(
            els.timeCurrent, els.timeTotal, els.progressFill,
            els.lyricsContainer, els.lyricsViewport,
            { offsetMs: backConfig.offsetMs }
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
    onFrontConfigChange,
    // 偏移变化时重新应用插值（通过重新处理当前 songInfo 实现）
    reapplyOnSongInfo: () => {
        import('./state/state.js').then(({ songInfo }) => {
            if (songInfo) onMessage(songInfo);
        });
    }
});

// 3. 加载后端配置并启动数据源
if (isTauri()) {
    // Tauri 环境：优先用 event 监听（主路径）
    fetchBackendConfig().then((cfg) => {
        Object.assign(backConfig, cfg);
        syncConfigToUI(els, frontConfig, backConfig);
    });
    listenSongInfo(onMessage, startRendering);
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

// 4. 可见性监听：后台时通知后端降频
document.addEventListener('visibilitychange', () => {
    const isHidden = document.visibilityState === 'hidden';
    notifyBackgroundState(isHidden);
});

// 5. 卸载时清理 WebSocket
window.addEventListener('beforeunload', () => {
    setIsIntentionalClose(true);
    disconnect();
});

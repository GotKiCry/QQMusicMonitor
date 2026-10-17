/**
 * Tauri 桥接：通过 Tauri event / invoke 与 Rust 后端通信。
 *
 * - listen('song-info') 订阅后端广播的 SongInfo（零回环开销，Tauri 主路径）
 */

/**
 * @param {(data: any) => void} onSongInfo
 * @param {(data: any) => void} onPlaybackProgress
 * @param {() => void} onConnected - 首次监听建立后启动渲染循环
 */
export async function listenSongInfo(onSongInfo, onPlaybackProgress, onConnected) {
    if (!window.__TAURI__) return false;
    const { listen } = window.__TAURI__.event;
    await Promise.all([
        listen('song-info', (event) => {
            if (event.payload) onSongInfo(event.payload);
        }),
        listen('playback-progress', (event) => {
            if (event.payload) onPlaybackProgress(event.payload);
        })
    ]);

    try {
        const current = await window.__TAURI__.core.invoke('get_current_song_info');
        if (current) onSongInfo(current);
    } catch (error) {
        console.error('Failed to get initial song state:', error);
    }

    onConnected();
    return true;
}

/** 是否运行在 Tauri 环境中 */
export function isTauri() {
    return !!window.__TAURI__;
}

/**
 * 通过 Tauri invoke 发送媒体控制命令。
 * @param {'play_pause' | 'next' | 'previous'} action
 */
export function mediaControl(action) {
    if (!window.__TAURI__) return;
    const cmd = action === 'play_pause' ? 'media_toggle_play_pause'
               : action === 'next' ? 'media_next'
               : 'media_previous';
    window.__TAURI__.core.invoke(cmd);
}

/**
 * 获取 Tauri 当前窗口对象（懒加载）。
 * @returns {Promise<any> | null} 非 Tauri 环境返回 null
 */
export function getAppWindow() {
    if (!window.__TAURI__) return null;
    return window.__TAURI__.window.getCurrentWindow();
}

/**
 * 窗口控制命令。
 * @param {'minimize' | 'maximize' | 'close'} action
 *
 * minimize / close 均为后台模式：隐藏窗口（从任务栏移除），保留系统托盘
 * 小图标，单击托盘图标或托盘菜单"显示"可恢复窗口。真正退出程序走托盘
 * 菜单"退出"或状态栏歌词条右键菜单"退出"（2026-10 用户要求：关闭按钮
 * 回后台，不再直接结束进程）。
 */
export function windowControl(action) {
    const appWindow = getAppWindow();
    if (!appWindow) return;
    if (action === 'minimize' || action === 'close') {
        // 先通知后端进入后台省电模式（停 GUI 推送；API 开启时不影响），再隐藏窗口
        window.__TAURI__.core.invoke('window_hidden').catch(() => {});
        appWindow.hide();
    }
    else if (action === 'maximize') appWindow.toggleMaximize();
}

/**
 * 查询窗口是否最大化。
 * @returns {Promise<boolean>}
 */
export async function isWindowMaximized() {
    const appWindow = getAppWindow();
    if (!appWindow) return false;
    try {
        return await appWindow.isMaximized();
    } catch {
        return false;
    }
}

/**
 * 订阅窗口尺寸变化（用于最大化/还原图标切换）。
 * @param {(maximized: boolean) => void} callback
 */
export async function onWindowResized(callback) {
    const appWindow = getAppWindow();
    if (!appWindow) return;
    try {
        await appWindow.onResized(() => {
            appWindow.isMaximized().then(callback).catch(() => {});
        });
    } catch {
        /* 权限不足时忽略 */
    }
}

/**
 * 开始拖动窗口（自定义标题栏）。
 * @returns {boolean} 是否已发起拖动
 */
export function startWindowDragging() {
    const appWindow = getAppWindow();
    if (!appWindow) return false;
    appWindow.startDragging();
    return true;
}

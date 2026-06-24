/**
 * Tauri 桥接：通过 Tauri event / invoke 与 Rust 后端通信。
 *
 * - listen('song-info') 订阅后端广播的 SongInfo（零回环开销，Tauri 主路径）
 * - invoke('set_background_state', ...) 通知后端窗口可见性以降频轮询
 */

/**
 * @param {(data: any) => void} onSongInfo
 * @param {() => void} onConnected - 首次监听建立后启动渲染循环
 */
export function listenSongInfo(onSongInfo, onConnected) {
    if (!window.__TAURI__) return false;
    const { listen } = window.__TAURI__.event;
    listen('song-info', (event) => {
        if (event.payload) onSongInfo(event.payload);
    });
    onConnected();
    return true;
}

/**
 * 通知后端窗口是否进入后台，以便降低 SMTC 轮询频率。
 * @param {boolean} isBackground
 */
export function notifyBackgroundState(isBackground) {
    if (!window.__TAURI__) return;
    window.__TAURI__.core.invoke('set_background_state', { isBackground });
}

/** 是否运行在 Tauri 环境中 */
export function isTauri() {
    return !!window.__TAURI__;
}

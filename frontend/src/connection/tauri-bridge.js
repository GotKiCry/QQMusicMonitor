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

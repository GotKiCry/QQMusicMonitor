/**
 * 后端（Rust 侧）运行时配置：端口、偏移、轮询间隔、文件输出开关。
 * 通过 Tauri invoke 与后端同步，并落盘到 config.toml。
 */

/**
 * @typedef {Object} BackendConfig
 * @property {number} port
 * @property {number} offsetMs
 * @property {number} intervalMs
 * @property {boolean} outputTxt
 * @property {boolean} outputJson
 * @property {boolean} outputLyric
 */

/** @returns {BackendConfig} */
export function getDefaultBackendConfig() {
    return {
        port: 3000,
        offsetMs: 200,
        intervalMs: 50,
        outputTxt: false,
        outputJson: true,
        outputLyric: true
    };
}

/**
 * 从 Tauri 后端读取配置；非 Tauri 环境返回默认值。
 * @returns {Promise<BackendConfig>}
 */
export async function fetchBackendConfig() {
    if (window.__TAURI__) {
        try {
            const cfg = await window.__TAURI__.core.invoke('get_app_config');
            return {
                port: cfg.server_port,
                offsetMs: cfg.smtc_offset_ms,
                intervalMs: cfg.update_interval_ms,
                outputTxt: cfg.output_txt,
                outputJson: cfg.output_json,
                outputLyric: cfg.output_lyric
            };
        } catch (e) {
            console.error('Failed to get app config:', e);
        }
    }
    return getDefaultBackendConfig();
}

/**
 * 保存后端配置到 config.toml（仅 Tauri 环境）。
 * @param {BackendConfig} cfg
 */
export async function saveBackendConfig(cfg) {
    if (!window.__TAURI__) return;
    try {
        await window.__TAURI__.core.invoke('save_app_config', {
            newCfg: {
                server_port: cfg.port,
                smtc_offset_ms: cfg.offsetMs,
                update_interval_ms: cfg.intervalMs,
                output_txt: cfg.outputTxt,
                output_json: cfg.outputJson,
                output_lyric: cfg.outputLyric
            }
        });
    } catch (e) {
        console.error('Failed to save config:', e);
    }
}

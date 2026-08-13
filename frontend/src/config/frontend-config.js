/**
 * 前端（浏览器本地）偏好配置：外观、字号、翻译开关等。
 *
 * 持久化双通道：
 *  - localStorage：同 origin 缓存（浏览器调试环境仅有这一层）
 *  - 后端文件 %APPDATA%\qqmusic-monitor\frontend-prefs.json：权威存储
 *    （Tauri 环境）。dev 模式下前端 origin 是 http://127.0.0.1:1430，
 *    端口被占用时 CLI 会换随机端口；直接运行 exe 时是 tauri://localhost——
 *    localStorage 按 origin 隔离，端口/协议一变配置就"丢失"，
 *    因此以后端文件为准，localStorage 仅作缓存与兜底。
 */

export const STORAGE_KEY = 'qqmusic-monitor-front-prefs';

/**
 * @typedef {Object} FrontendConfig
 * @property {number} fontSize
 * @property {number} fontWeight
 * @property {boolean} showTranslation
 * @property {string} theme
 * @property {string|null} customColor
 * @property {boolean} lyricBar
 * @property {'left'|'right'} lyricBarSide
 * @property {number} lyricBarGap
 * @property {boolean} debug
 */

/** @returns {FrontendConfig} */
export function getDefaultFrontendConfig() {
    return {
        fontSize: 26,
        fontWeight: 700,
        showTranslation: true,
        theme: 'aurora-cyan',
        customColor: null,
        lyricBar: false,
        lyricBarSide: 'right',
        lyricBarGap: 0,
        debug: false
    };
}

/** 把任意对象规范化为 FrontendConfig（缺字段补默认值） */
function normalizePrefs(p) {
    const base = getDefaultFrontendConfig();
    const isHex = (v) => typeof v === 'string' && /^#[0-9a-fA-F]{6}$/.test(v);
    return {
        fontSize: Number.isInteger(p.fontSize) ? p.fontSize : base.fontSize,
        fontWeight: Number.isInteger(p.fontWeight) ? p.fontWeight : base.fontWeight,
        showTranslation: typeof p.showTranslation === 'boolean' ? p.showTranslation : base.showTranslation,
        theme: typeof p.theme === 'string' ? p.theme : base.theme,
        customColor: isHex(p.customColor) ? p.customColor : null,
        lyricBar: typeof p.lyricBar === 'boolean' ? p.lyricBar : base.lyricBar,
        lyricBarSide: p.lyricBarSide === 'left' ? 'left' : 'right',
        lyricBarGap: Number.isInteger(p.lyricBarGap)
            ? Math.min(500, Math.max(0, p.lyricBarGap))
            : base.lyricBarGap,
        debug: typeof p.debug === 'boolean' ? p.debug : base.debug
    };
}

/** @returns {FrontendConfig} */
export function loadFrontendConfig() {
    try {
        const raw = localStorage.getItem(STORAGE_KEY);
        if (!raw) return getDefaultFrontendConfig();
        return normalizePrefs(JSON.parse(raw));
    } catch {
        return getDefaultFrontendConfig();
    }
}

/** 仅写 localStorage（不触发后端保存，供 hydrate 回填缓存用） */
function saveLocalOnly(cfg) {
    try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(cfg));
    } catch {}
}

/** @param {FrontendConfig} cfg */
export function saveFrontendConfig(cfg) {
    saveLocalOnly(cfg);
    // Tauri 环境：同步写后端文件（跨 origin 权威存储）
    if (window.__TAURI__) {
        window.__TAURI__.core
            .invoke('save_frontend_prefs', { json: JSON.stringify(cfg) })
            .catch((e) => console.error('Failed to save frontend prefs:', e));
    }
}

/**
 * Tauri 环境：从后端文件水合前端偏好（跨 origin 权威值）。
 * 文件存在时回填 localStorage 缓存并返回规范化配置；无文件返回 null。
 * @returns {Promise<FrontendConfig|null>}
 */
export async function hydrateFrontendConfig() {
    if (!window.__TAURI__) return null;
    try {
        const json = await window.__TAURI__.core.invoke('get_frontend_prefs');
        if (!json) return null;
        const cfg = normalizePrefs(JSON.parse(json));
        saveLocalOnly(cfg);
        return cfg;
    } catch (e) {
        console.error('Failed to hydrate frontend prefs:', e);
        return null;
    }
}

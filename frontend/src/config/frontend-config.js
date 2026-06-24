/**
 * 前端（浏览器本地）偏好配置：外观、字号、翻译开关等。
 * 持久化到 localStorage，不经过后端。
 */

const STORAGE_KEY = 'qqmusic-monitor-front-prefs';

/**
 * @typedef {Object} FrontendConfig
 * @property {number} fontSize
 * @property {number} fontWeight
 * @property {boolean} showTranslation
 * @property {string} theme
 * @property {boolean} debug
 */

/** @returns {FrontendConfig} */
export function getDefaultFrontendConfig() {
    return {
        fontSize: 26,
        fontWeight: 700,
        showTranslation: true,
        theme: 'aurora-cyan',
        debug: false
    };
}

/** @returns {FrontendConfig} */
export function loadFrontendConfig() {
    const base = getDefaultFrontendConfig();
    try {
        const raw = localStorage.getItem(STORAGE_KEY);
        if (!raw) return base;
        const p = JSON.parse(raw);
        return {
            fontSize: Number.isInteger(p.fontSize) ? p.fontSize : base.fontSize,
            fontWeight: Number.isInteger(p.fontWeight) ? p.fontWeight : base.fontWeight,
            showTranslation: typeof p.showTranslation === 'boolean' ? p.showTranslation : base.showTranslation,
            theme: typeof p.theme === 'string' ? p.theme : base.theme,
            debug: typeof p.debug === 'boolean' ? p.debug : base.debug
        };
    } catch {
        return base;
    }
}

/** @param {FrontendConfig} cfg */
export function saveFrontendConfig(cfg) {
    try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(cfg));
    } catch {}
}

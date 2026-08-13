/**
 * 主题应用共享模块：设置抽屉与状态栏歌词页面共用。
 * 预设主题只设置 body[data-theme]，变量由 tokens.css 定义；
 * 自定义主题（custom）与自动取色（auto，专辑封面主色）通过
 * 内联 CSS 变量覆盖默认 token。
 */

/** 自动取色结果的跨窗口共享 key（主窗口提取后经 localStorage 广播给歌词条） */
export const AUTO_COLOR_KEY = 'qqmusic-monitor-auto-color';

/** 读取当前自动主题色（无则 null） */
export function loadAutoColor() {
    try {
        return localStorage.getItem(AUTO_COLOR_KEY);
    } catch {
        return null;
    }
}

/** 取色盘参考色（与 tokens.css 中各主题主色一致） */
export function presetThemeColor(theme) {
    return {
        'aurora-purple': '#a855f7',
        'aurora-emerald': '#10b981',
        'aurora-sunset': '#f97316'
    }[theme] || '#14b8a6';
}

/**
 * 把自定义色写入 CSS 变量（覆盖默认 token），null 则清除覆盖。
 * 顺带把「自动主题」圆点（设置面板的 theme-dot-auto）中心圆点重写成当前
 * --theme-primary，避免它和落日橙 dot-sunset(#f97316) 撞色，也让用户看到
 * 当前自动主题会取到的真实色（专辑取色结果 / 默认主题色）。
 * @param {string|null} hex
 */
export function paintCustomTheme(hex) {
    const vars = [
        '--theme-primary',
        '--theme-primary-glow',
        '--theme-gradient',
        '--theme-gradient-strong',
        '--theme-bar-glow',
        '--border-focus'
    ];
    if (!hex) {
        vars.forEach((v) => document.body.style.removeProperty(v));
    } else {
        const [r, g, b] = hexToRgb(hex);
        const glow = `rgba(${r}, ${g}, ${b}, 0.35)`;
        const barGlow = `rgba(${r}, ${g}, ${b}, 0.18)`;
        const focus = `rgba(${r}, ${g}, ${b}, 0.4)`;
        const light = mix(hex, '#ffffff', 0.25);
        const dark = mix(hex, '#000000', 0.35);
        const strongDark = mix(hex, '#000000', 0.55);
        document.body.style.setProperty('--theme-primary', hex);
        document.body.style.setProperty('--theme-primary-glow', glow);
        document.body.style.setProperty('--theme-gradient', `linear-gradient(135deg, ${light}, ${dark})`);
        document.body.style.setProperty('--theme-gradient-strong', `linear-gradient(135deg, ${dark}, ${strongDark})`);
        document.body.style.setProperty('--theme-bar-glow', barGlow);
        document.body.style.setProperty('--border-focus', focus);
    }
    // 重写 SVG 让圆点中心跟随当前 --theme-primary（CSS 中不可用变量，故每次重建）
    paintAutoDot();
}

/**
 * 把自动主题圆点 (.theme-dot-auto) 的中心圆点颜色改为当前 --theme-primary。
 *  - 设置面板里此按钮的中心圆点 = 当前主题主色（cyan/purple/emerald/sunset/auto 取色）
 *  - 主题切换 / 专辑取色更新都会触发 paintCustomTheme → paintAutoDot
 *  - 歌词条窗口无此按钮，安全 no-op
 *  - SVG fill 不接受 CSS 变量（data URI 内），必须整段重写
 */
export function paintAutoDot() {
    const btn = document.querySelector('.theme-dot-auto');
    if (!btn) return;
    const raw = getComputedStyle(document.body).getPropertyValue('--theme-primary');
    const color = (raw || '').trim() || '#14b8a6';
    const svg = `<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 20'><circle cx='10' cy='10' r='8.5' fill='none' stroke='#ffffff' stroke-width='3'/><circle cx='10' cy='10' r='2.8' fill='${color}'/></svg>`;
    btn.style.backgroundImage = `url("data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}")`;
}

/**
 * 按前端偏好配置应用主题到文档。
 * @param {{theme: string, customColor: (string|null)}} front
 * @param {string} [fallbackHex] - custom 但无颜色时的兜底
 */
export function applyThemeToDocument(front, fallbackHex) {
    document.body.setAttribute('data-theme', front.theme);
    if (front.theme === 'custom') {
        paintCustomTheme(front.customColor || fallbackHex || '#14b8a6');
    } else if (front.theme === 'auto') {
        // 自动取色：有主窗口发布的专辑色就应用，否则回落默认 token
        paintCustomTheme(loadAutoColor());
    } else {
        paintCustomTheme(null);
    }
}

/**
 * 专辑取色：从封面图提取主色。
 *  - data:/blob: 直接解码（canvas 安全）
 *  - http(s)：crossOrigin=anonymous（CDN 需允许 CORS），canvas 被 taint 时返回 null
 * 算法：缩到 48px 采样，跳过透明与近灰像素求均值（防止灰底拉低饱和度），
 * 再把均值以灰轴为中心放大饱和度 35%。
 * @param {string} src
 * @returns {Promise<string|null>} '#rrggbb'
 */
export function extractAlbumColor(src) {
    return new Promise((resolve) => {
        if (!src) {
            resolve(null);
            return;
        }
        const img = new Image();
        const timer = setTimeout(() => finish(null), 5000);
        const finish = (hex) => {
            clearTimeout(timer);
            resolve(hex);
        };
        img.onload = () => {
            try {
                const size = 48;
                const canvas = document.createElement('canvas');
                canvas.width = size;
                canvas.height = size;
                const ctx = canvas.getContext('2d', { willReadFrequently: true });
                ctx.drawImage(img, 0, 0, size, size);
                const d = ctx.getImageData(0, 0, size, size).data;

                let r = 0;
                let g = 0;
                let b = 0;
                let n = 0;
                for (let i = 0; i < d.length; i += 4) {
                    if (d[i + 3] < 128) continue; // 跳过透明像素
                    const pr = d[i];
                    const pg = d[i + 1];
                    const pb = d[i + 2];
                    const mx = Math.max(pr, pg, pb);
                    const mn = Math.min(pr, pg, pb);
                    if (mx - mn < 24) continue; // 跳过近灰像素（背景/白字黑字）
                    r += pr;
                    g += pg;
                    b += pb;
                    n++;
                }
                // 彩色像素太少（黑白封面）：退回全图不透明像素均值
                if (n < size * size * 0.05) {
                    r = 0;
                    g = 0;
                    b = 0;
                    n = 0;
                    for (let i = 0; i < d.length; i += 4) {
                        if (d[i + 3] < 128) continue;
                        r += d[i];
                        g += d[i + 1];
                        b += d[i + 2];
                        n++;
                    }
                }
                if (n === 0) {
                    finish(null);
                    return;
                }
                r /= n;
                g /= n;
                b /= n;
                // 饱和度放大：以灰轴为中心向外推 35%
                const gray = (r + g + b) / 3;
                const boost = (v) =>
                    Math.max(0, Math.min(255, Math.round(gray + (v - gray) * 1.35)));
                const hex = ((1 << 24) + (boost(r) << 16) + (boost(g) << 8) + boost(b))
                    .toString(16)
                    .slice(1);
                finish(`#${hex}`);
            } catch {
                finish(null); // canvas 被 taint（跨域图无 CORS 头）
            }
        };
        img.onerror = () => finish(null);
        if (/^https?:/i.test(src)) img.crossOrigin = 'anonymous';
        img.src = src;
    });
}

/** #rrggbb → [r, g, b] */
function hexToRgb(hex) {
    const n = parseInt(hex.slice(1), 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** 把 hex 向目标色按比例混合（t=1 全为目标色） */
function mix(hex, target, t) {
    const [r, g, b] = hexToRgb(hex);
    const [tr, tg, tb] = hexToRgb(target);
    const m = (a, ta) => Math.round(a + (ta - a) * t);
    const toHex = (v) => v.toString(16).padStart(2, '0');
    return `#${toHex(m(r, tr))}${toHex(m(g, tg))}${toHex(m(b, tb))}`;
}
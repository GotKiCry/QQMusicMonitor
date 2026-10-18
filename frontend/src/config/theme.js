/**
 * 主题应用共享模块：设置抽屉与状态栏歌词页面共用。
 * 预设主题只设置 body[data-theme]，变量由 tokens.css 定义；
 * 自定义主题（custom）与自动取色（auto，专辑封面主色）通过内联 CSS
 * 变量覆盖默认 token。
 *
 * 自动取色（2026-10 升级，用户确认）：
 *  - 64px 采样 + 5-bit 直方图中位切分（MMCQ，零依赖，不引 ColorThief/Vibrant）
 *  - OKLCH 双档选择：鲜艳档（品牌色）+ 低饱和冷色背景档（蓝紫绿大片）
 *  - 无彩色/取色失败 → 中性银回退（不再返回 null、不再继承上一首颜色）
 *  - 输出「角色调色板」而非单个 hex：
 *      primary   品牌/装饰（进度条、hover、光晕）
 *      ink       文字色（主窗口歌词高亮 + 状态栏文字共用同一个 ink，
 *                对 #2c2c2c 深色背景 ≥4.5:1；取色与背景无关，角色映射与背景有关）
 *      secondary 渐变另一端（色相距离 ≥35° 的次优簇，无则 primary 提亮）
 *      onPrimary 填充色上的文字（按 primary 亮度选黑/白）
 *      neutral   是否走了中性回退
 */

/** 自动取色结果（主色 hex）的跨窗口共享 key：兼容旧版读取 */
export const AUTO_COLOR_KEY = 'qqmusic-monitor-auto-color';

/** 自动取色调色板（JSON）的跨窗口共享 key（主窗口提取后经 localStorage 广播） */
export const AUTO_PALETTE_KEY = 'qqmusic-monitor-auto-palette';

/** 读取当前自动主题色（无则 null） */
export function loadAutoColor() {
    try {
        return localStorage.getItem(AUTO_COLOR_KEY);
    } catch {
        return null;
    }
}

/**
 * 读取当前自动主题调色板：优先新版 JSON，其次兼容旧版单 hex。
 * @returns {{primary:string,ink:string,secondary:string,onPrimary:string,neutral:boolean}|null}
 */
export function loadAutoPalette() {
    try {
        const raw = localStorage.getItem(AUTO_PALETTE_KEY);
        if (raw) {
            const p = JSON.parse(raw);
            if (p && typeof p.primary === 'string' && /^#[0-9a-fA-F]{6}$/.test(p.primary)) {
                return normalizePalette(p);
            }
        }
    } catch {
        /* 损坏的 JSON：走旧版 hex 兜底 */
    }
    const hex = loadAutoColor();
    return hex ? paletteFromHex(hex) : null;
}

/** 取色盘参考色（与 tokens.css 中各主题主色一致） */
export function presetThemeColor(theme) {
    return {
        'aurora-purple': '#a855f7',
        'aurora-emerald': '#10b981',
        'aurora-sunset': '#f97316'
    }[theme] || '#14b8a6';
}

/** 深色目标背景（主窗口玻璃底/状态栏深色任务栏）的相对亮度：ink 的对比度基准 */
const INK_TARGET_LUM = 0.0296; // #2c2c2c
/**
 * 文字 ink 的最低对比度。2026-10 用户两轮反馈"浅色的都改为深色的"、
 * "颜色不统一"：原 4.5:1 把深红品牌色提亮成浅珊瑚 #e2796e，与深红装饰色
 * （进度条/封面框）脱节。降到 2.8:1 后 ink 为深砖红（#8a2a24 → #b8473d），
 * 与 primary 视觉同色系；对主窗口实测背景仍有约 4.1:1（大字达标）。
 * 同时主窗口装饰元素统一改用 ink（见 components/settings CSS），
 * 让"文字高亮 / 艺术家名 / 进度条 / 封面框 / 状态栏歌词"全部同一个色。
 */
const INK_MIN_RATIO = 2.8;

/**
 * 把主题写进 CSS 变量（覆盖默认 token），null 则清除覆盖。
 * 入参兼容三种形态：
 *  - null：清除覆盖（回落到 tokens.css 的预设主题变量）
 *  - string hex：旧调用点（设置面板/预设主题预览）→ 内部补全 ink/secondary
 *  - palette 对象：extractAlbumPalette 的输出
 * @param {string|null|Object} input
 */
export function paintCustomTheme(input) {
    const vars = [
        '--theme-primary',
        '--theme-ink',
        '--theme-secondary',
        '--theme-on-primary',
        '--theme-primary-glow',
        '--theme-gradient',
        '--theme-gradient-strong',
        '--theme-text-gradient',
        '--theme-bar-glow',
        '--border-focus'
    ];
    if (!input) {
        vars.forEach((v) => document.body.style.removeProperty(v));
        paintAutoDot();
        return;
    }
    const p = typeof input === 'string' ? paletteFromHex(input) : normalizePalette(input);
    if (!p) {
        vars.forEach((v) => document.body.style.removeProperty(v));
        paintAutoDot();
        return;
    }
    const primary = p.primary;
    const ink = p.ink || ensureOklchContrast(primary, INK_TARGET_LUM, INK_MIN_RATIO);
    const secondary = p.secondary || mix(primary, '#ffffff', 0.25);
    const [r, g, b] = hexToRgb(primary);
    const [ir, ig, ib] = hexToRgb(ink);
    const light = mix(primary, '#ffffff', 0.25);
    const dark = mix(primary, '#000000', 0.35);
    const strongDark = mix(primary, '#000000', 0.55);
    // 文字渐变：两端都取 ink（保证纯文本行也能读），亮端微提亮
    const inkLight = mix(ink, '#ffffff', 0.18);
    const inkDark = mix(ink, '#000000', 0.35);
    document.body.style.setProperty('--theme-primary', primary);
    document.body.style.setProperty('--theme-ink', ink);
    document.body.style.setProperty('--theme-secondary', secondary);
    document.body.style.setProperty('--theme-on-primary', p.onPrimary || (relLum(primary) > 0.45 ? '#0a0e17' : '#ffffff'));
    document.body.style.setProperty('--theme-primary-glow', 'rgba(' + r + ', ' + g + ', ' + b + ', 0.35)');
    document.body.style.setProperty('--theme-gradient', 'linear-gradient(135deg, ' + light + ', ' + dark + ')');
    document.body.style.setProperty('--theme-gradient-strong', 'linear-gradient(135deg, ' + dark + ', ' + strongDark + ')');
    document.body.style.setProperty('--theme-text-gradient', 'linear-gradient(135deg, ' + inkLight + ', ' + inkDark + ')');
    document.body.style.setProperty('--theme-bar-glow', 'rgba(' + ir + ', ' + ig + ', ' + ib + ', 0.18)');
    document.body.style.setProperty('--border-focus', 'rgba(' + ir + ', ' + ig + ', ' + ib + ', 0.4)');
    paintAutoDot();
}

/**
 * 自动主题圆点 (.theme-dot-auto) 已改为纯 CSS 实现：白描边 + 背景直接消费
 * --theme-primary（见 settings.css），主题切换/封面取色实时跟随。
 * 保留空实现兼容旧调用点（paintCustomTheme 等）。
 */
export function paintAutoDot() {
    // no-op：CSS 变量方案接管（20px 小尺寸 SVG data URI 渲染不可控）
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
        // 自动取色：优先用新版调色板；取色尚未发布时保持当前色不动
        // （清除覆盖会让各窗口闪回默认青绿色）
        const palette = loadAutoPalette();
        if (palette) paintCustomTheme(palette);
    } else {
        paintCustomTheme(null);
    }
}

/* ============================================================
 * 颜色工具（sRGB / HSL / OKLCH / 对比度）
 * ============================================================ */

/** #rrggbb → [r, g, b] */
function hexToRgb(hex) {
    const n = parseInt(hex.slice(1), 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/** [r, g, b](0-255) → #rrggbb */
function rgbToHex(rgb) {
    const to = (v) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, '0');
    return '#' + to(rgb[0]) + to(rgb[1]) + to(rgb[2]);
}

/** 把 hex 向目标色按比例混合（t=1 全为目标色） */
function mix(hex, target, t) {
    const [r, g, b] = hexToRgb(hex);
    const [tr, tg, tb] = hexToRgb(target);
    const m = (a, ta) => Math.round(a + (ta - a) * t);
    const toHex = (v) => v.toString(16).padStart(2, '0');
    return '#' + toHex(m(r, tr)) + toHex(m(g, tg)) + toHex(m(b, tb));
}

/** sRGB 分量(0-1) → 线性光 */
function srgbToLinear(v) {
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
}

/** 线性光 → sRGB 分量(0-1) */
function linearToSrgb(v) {
    return v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
}

/**
 * sRGB [r,g,b](0-255) → OKLCH {L: 0-1, C: 0-~0.3, h: 0-360}。
 * OKLCH 比 HSL 更接近感知：可在保持色相/彩度的同时只调亮度，
 * 避免 RGB 混白把饱和色灰化。
 */
export function rgbToOklch(rgb) {
    const [r, g, b] = rgb.map((v) => srgbToLinear(v / 255));
    const l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    const m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    const s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    const l_ = Math.cbrt(l);
    const m_ = Math.cbrt(m);
    const s_ = Math.cbrt(s);
    const L = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    const a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    const bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;
    const C = Math.sqrt(a * a + bb * bb);
    let h = (Math.atan2(bb, a) * 180) / Math.PI;
    if (h < 0) h += 360;
    return { L, C, h };
}

/**
 * OKLCH → sRGB [r,g,b](0-255)，自动做色域裁剪：
 * 若超出 sRGB 色域，则二分降低彩度 C（保持 L/h），避免通道裁剪产生色偏。
 */
export function oklchToRgb(L, C, h) {
    const rad = (h * Math.PI) / 180;
    const convert = (c) => {
        const aa = c * Math.cos(rad);
        const bb = c * Math.sin(rad);
        const ll_ = L + 0.3963377774 * aa + 0.2158037573 * bb;
        const mm_ = L - 0.1055613458 * aa - 0.0638541728 * bb;
        const ss_ = L - 0.0894841775 * aa - 1.2914855480 * bb;
        const ll = ll_ * ll_ * ll_;
        const mm = mm_ * mm_ * mm_;
        const ss = ss_ * ss_ * ss_;
        const r = 4.0767416621 * ll - 3.3077115913 * mm + 0.2309699292 * ss;
        const g = -1.2684380046 * ll + 2.6097574011 * mm - 0.3413193965 * ss;
        const bl = -0.0041960863 * ll - 0.7034186147 * mm + 1.7076147010 * ss;
        return [r, g, bl];
    };
    const inGamut = (rgb) => rgb.every((v) => v >= -0.0005 && v <= 1.0005);
    let rgb = convert(C);
    if (!inGamut(rgb)) {
        let lo = 0;
        let hi = C;
        for (let i = 0; i < 24; i++) {
            const mid = (lo + hi) / 2;
            if (inGamut(convert(mid))) lo = mid;
            else hi = mid;
        }
        rgb = convert(lo);
    }
    return rgb.map((v) => Math.round(linearToSrgb(Math.max(0, Math.min(1, v))) * 255));
}

/** OKLCH → #rrggbb */
function oklchToHex(L, C, h) {
    return rgbToHex(oklchToRgb(L, C, h));
}

/** hex → 相对亮度（WCAG 定义） */
export function relLum(hex) {
    const [r, g, b] = hexToRgb(hex);
    const lin = (v) => srgbToLinear(v / 255);
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/**
 * 保色相/彩度调整明度，直到对深色背景 bgLum 的对比度 ≥ minRatio（OKLCH 二分）。
 * 用于生成 ink：暗色品牌色不会被混成灰粉，只变亮（色相不变）。
 * @param {string} hex
 * @param {number} bgLum - 目标背景相对亮度
 * @param {number} minRatio
 * @returns {string}
 */
export function ensureOklchContrast(hex, bgLum, minRatio) {
    const needLum = minRatio * (bgLum + 0.05) - 0.05;
    if (relLum(hex) >= needLum) return hex;
    const { L, C, h } = rgbToOklch(hexToRgb(hex));
    let lo = L;
    let hi = 1;
    for (let i = 0; i < 32; i++) {
        const mid = (lo + hi) / 2;
        if (relLum(oklchToHex(mid, C, h)) >= needLum) hi = mid;
        else lo = mid;
    }
    return oklchToHex(hi, C, h);
}

/** hex → HSL 分量（h: 0-360, s: 0-1, l: 0-1） */
function hexToHsl(hex) {
    const [r, g, b] = hexToRgb(hex).map((v) => v / 255);
    const max = Math.max(r, g, b);
    const min = Math.min(r, g, b);
    const l = (max + min) / 2;
    if (max === min) return [0, 0, l];
    const d = max - min;
    const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    let h;
    if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
    else if (max === g) h = ((b - r) / d + 2) / 6;
    else h = ((r - g) / d + 4) / 6;
    return [h * 360, s, l];
}

/** HSL 分量 → hex */
function hslToHex(h, s, l) {
    const c = (1 - Math.abs(2 * l - 1)) * s;
    const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
    const m = l - c / 2;
    let rgb;
    if (h < 60) rgb = [c, x, 0];
    else if (h < 120) rgb = [x, c, 0];
    else if (h < 180) rgb = [0, c, x];
    else if (h < 240) rgb = [0, x, c];
    else if (h < 300) rgb = [x, 0, c];
    else rgb = [c, 0, x];
    const to = (v) => Math.round((v + m) * 255).toString(16).padStart(2, '0');
    return '#' + to(rgb[0]) + to(rgb[1]) + to(rgb[2]);
}

/* ============================================================
 * MMCQ 调色板量化（5-bit 直方图 + 中位切分）
 * 纯函数、零依赖：ColorThief 的许可证元数据与源码头不一致（BSD-2 vs
 * CC-BY 2.5），node-vibrant 体积 59KB 且带 lodash/Jimp 依赖，均不引入。
 * ============================================================ */

/**
 * 中位切分量化：返回按像素数降序的调色板。
 * @param {Uint8ClampedArray} rgba - ImageData.data
 * @param {number} [colorCount=14]
 * @returns {{rgb:[number,number,number], count:number}[]}
 */
export function quantizeMMCQ(rgba, colorCount = 14) {
    const SIGBITS = 5;
    const RSHIFT = 8 - SIGBITS;
    const HISTO_SIZE = 1 << (3 * SIGBITS);
    const hist = new Float64Array(HISTO_SIZE);
    const sumR = new Float64Array(HISTO_SIZE);
    const sumG = new Float64Array(HISTO_SIZE);
    const sumB = new Float64Array(HISTO_SIZE);
    let total = 0;
    for (let i = 0; i < rgba.length; i += 4) {
        if (rgba[i + 3] < 128) continue;
        const r = rgba[i];
        const g = rgba[i + 1];
        const b = rgba[i + 2];
        const idx = ((r >> RSHIFT) << (2 * SIGBITS)) | ((g >> RSHIFT) << SIGBITS) | (b >> RSHIFT);
        hist[idx] += 1;
        sumR[idx] += r;
        sumG[idx] += g;
        sumB[idx] += b;
        total++;
    }
    if (!total) return [];
    const slots = [];
    for (let i = 0; i < HISTO_SIZE; i++) {
        if (hist[i] > 0) {
            slots.push({
                count: hist[i],
                r: sumR[i] / hist[i],
                g: sumG[i] / hist[i],
                b: sumB[i] / hist[i]
            });
        }
    }
    const boxOf = (list) => {
        let minR = 255, maxR = 0, minG = 255, maxG = 0, minB = 255, maxB = 0, count = 0;
        for (const s of list) {
            if (s.r < minR) minR = s.r;
            if (s.r > maxR) maxR = s.r;
            if (s.g < minG) minG = s.g;
            if (s.g > maxG) maxG = s.g;
            if (s.b < minB) minB = s.b;
            if (s.b > maxB) maxB = s.b;
            count += s.count;
        }
        return { slots: list, count, minR, maxR, minG, maxG, minB, maxB };
    };
    let boxes = [boxOf(slots)];
    while (boxes.length < colorCount) {
        let target = -1;
        for (let i = 0; i < boxes.length; i++) {
            const bx = boxes[i];
            if (bx.slots.length > 1 && (target === -1 || bx.count > boxes[target].count)) target = i;
        }
        if (target === -1) break;
        const box = boxes[target];
        const spanR = box.maxR - box.minR;
        const spanG = box.maxG - box.minG;
        const spanB = box.maxB - box.minB;
        const axis = spanR >= spanG && spanR >= spanB ? 'r' : spanG >= spanB ? 'g' : 'b';
        const sorted = box.slots.slice().sort((p, q) => p[axis] - q[axis]);
        const half = box.count / 2;
        let acc = 0;
        let cut = 0;
        for (let i = 0; i < sorted.length; i++) {
            acc += sorted[i].count;
            if (acc >= half) {
                cut = i + 1;
                break;
            }
        }
        if (cut <= 0 || cut >= sorted.length) cut = Math.max(1, Math.floor(sorted.length / 2));
        boxes.splice(target, 1, boxOf(sorted.slice(0, cut)), boxOf(sorted.slice(cut)));
    }
    return boxes
        .map((bx) => {
            let r = 0, g = 0, b = 0;
            for (const s of bx.slots) {
                r += s.r * s.count;
                g += s.g * s.count;
                b += s.b * s.count;
            }
            return { rgb: [r / bx.count, g / bx.count, b / bx.count], count: bx.count };
        })
        .sort((p, q) => q.count - p.count);
}

/**
 * 从调色板里挑出主题角色（Tier1 鲜艳档 / Tier2 冷色背景档 / 中性回退）。
 * 参数来自 32 张真实封面 + 边界样本的离线实验（见 .agents/memory/lyric-bar.md）。
 * @param {{rgb:[number,number,number], count:number}[]} swatches - quantizeMMCQ 输出
 * @returns {Object} palette（含 primary/ink/secondary/onPrimary/neutral/tier/...）
 */
export function selectThemeRoles(swatches) {
    const total = swatches.reduce((acc, s) => acc + s.count, 0) || 1;
    const items = [];
    for (const s of swatches) {
        const [r, g, b] = s.rgb;
        const mx = Math.max(r, g, b);
        const mn = Math.min(r, g, b);
        const oklch = rgbToOklch(s.rgb);
        // 近白/近黑只保留做背景判断，不作为候选
        if (mn > 238 || mx < 12 || oklch.L < 0.12 || oklch.L > 0.90) continue;
        items.push({ rgb: s.rgb, hex: rgbToHex(s.rgb), count: s.count, pop: s.count / total, L: oklch.L, C: oklch.C, h: oklch.h });
    }
    const tier1 = [];
    for (const d of items) {
        if (d.C >= 0.09 && d.pop >= 0.005 && (d.pop >= 0.02 || d.C >= 0.12)) {
            // 肤色惩罚：暖色相 + 低彩度 + 中高亮度（人像脸部）
            const skin = d.h >= 5 && d.h <= 65 && d.C < 0.11 && d.L > 0.35 ? 0.25 : 1.0;
            // 暖中性惩罚：黄绿低彩度大面积（木纹/米色底）
            const warm = d.h >= 55 && d.h <= 95 && d.C < 0.10 && d.pop > 0.04 ? 0.35 : 1.0;
            const band = Math.exp(-Math.pow((d.L - 0.57) / 0.30, 2));
            const score = Math.pow(d.pop, 0.45) * Math.pow(d.C, 0.75) * band * skin * warm;
            tier1.push({ ...d, score, skin });
        }
    }
    tier1.sort((p, q) => q.score - p.score);
    let best = null;
    let tier = 0;
    if (tier1.length && !(tier1[0].skin < 1 && tier1[0].C < 0.11)) {
        best = tier1[0];
        tier = 1;
    } else {
        const tier2 = [];
        for (const d of items) {
            if (d.C < 0.04 || d.pop < 0.04 || d.L > 0.78) continue;
            if (d.h < 150 || d.h > 330) continue;
            const band = Math.exp(-Math.pow((d.L - 0.45) / 0.35, 2));
            const score = Math.pow(d.pop, 0.4) * Math.pow(d.C, 0.5) * band;
            tier2.push({ ...d, score });
        }
        tier2.sort((p, q) => q.score - p.score);
        if (tier2.length) {
            best = tier2[0];
            tier = 2;
        }
    }
    if (!best) {
        // Tier3 暖中性/香槟档：大面积低饱和暖色（米黄/香槟/奶油）。
        // 有些封面整体是"黑白/低饱和暖调"（如《清明雨上》：黑发人像 +
        // 暖米色唱片/背景，全部 swatch C 只有 0.02~0.04），Tier1/Tier2
        // 都进不去，但取中性银会丢掉封面的暖调。QQ 音乐对这类封面的做法
        // 就是取暖香槟色（浅色背景 + 金色强调）。
        // 限制：h 50-110 避开肤色（20-50）、C>=0.03 避开真正的黑白、
        // L>=0.62 取亮部（米色背景而非深褐头发）、pop>=5% 要求大面积。
        const tier3 = [];
        for (const d of items) {
            if (d.h < 50 || d.h > 110) continue;
            if (d.C < 0.03 || d.C > 0.10) continue;
            if (d.L < 0.62) continue;
            if (d.pop < 0.05) continue;
            const band = Math.exp(-Math.pow((d.L - 0.74) / 0.22, 2));
            const score = Math.pow(d.pop, 0.4) * Math.pow(d.C, 0.3) * band;
            tier3.push({ ...d, score });
        }
        tier3.sort((p, q) => q.score - p.score);
        if (tier3.length) {
            best = tier3[0];
            tier = 3;
        }
    }
    if (!best) return neutralPalette('no-chromatic-swatch', items);
    let primary;
    if (tier === 1) {
        // UI 归一化：保持色相，亮度/彩度收进适合深色 UI 的区间。
        // 2026-10 用户要求"浅色的都改为深色的"：亮度上限 0.78 → 0.68，
        // 避免亮色品牌色（浅黄/粉/亮青）在深色界面上显得飘。
        const L2 = Math.min(0.68, Math.max(0.42, best.L));
        const C2 = Math.min(0.24, Math.max(0.06, best.C));
        primary = Math.abs(L2 - best.L) < 1e-6 && Math.abs(C2 - best.C) < 1e-6
            ? best.hex
            : oklchToHex(L2, C2, best.h);
    } else if (tier === 2) {
        const L2 = Math.max(0.52, best.L);
        const C2 = Math.min(0.12, Math.max(0.07, best.C));
        primary = oklchToHex(L2, C2, best.h);
    } else {
        // Tier3：暖中性封面 → 明确的「深金黄」（不再输出浅香槟灰）。
        // 用户 2026-10 反馈：浅香槟 #b5a68c 在深色 UI 上像"脏米色"，
        // 要求"深黄色但亮一点"：彩度提到 0.14~0.17（金黄），亮度保持
        // 0.70~0.76（醒目），色相收窄到 70~100°（纯黄，避开橙/柠檬）。
        const L2 = Math.min(0.76, Math.max(0.70, best.L));
        const C2 = Math.min(0.17, Math.max(0.14, best.C * 3.5));
        const h2 = Math.min(100, Math.max(70, best.h));
        primary = oklchToHex(L2, C2, h2);
    }
    // 文字色：品牌色转 ink（对深色目标 ≥3.5:1，色相不变）
    const ink = ensureOklchContrast(primary, INK_TARGET_LUM, INK_MIN_RATIO);
    // 渐变另一端：调色板中色相距离 ≥35° 的次优簇；没有则 primary 提亮 18%
    let secondary = null;
    for (const d of items) {
        if (d.hex === best.hex || d.C < 0.03 || d.pop < 0.01) continue;
        let dh = Math.abs(d.h - best.h);
        if (dh > 180) dh = 360 - dh;
        if (dh >= 35) {
            secondary = d.hex;
            break;
        }
    }
    if (!secondary) secondary = mix(primary, '#ffffff', 0.18);
    return {
        primary,
        ink,
        secondary,
        onPrimary: relLum(primary) > 0.45 ? '#0a0e17' : '#ffffff',
        neutral: false,
        tier,
        reason: 'ok',
        swatches: items.slice(0, 6).map((d) => ({ hex: d.hex, pop: d.pop, L: d.L, C: d.C, h: d.h }))
    };
}

/** 中性调色板（黑白/低饱和封面、取色失败时的回退） */
function neutralPalette(reason, swatches) {
    return {
        primary: '#94a3b8',
        ink: '#94a3b8',
        secondary: '#64748b',
        onPrimary: '#0a0e17',
        neutral: true,
        tier: 0,
        reason,
        swatches: Array.isArray(swatches) ? swatches.slice(0, 6).map((d) => ({ hex: d.hex, pop: d.pop })) : []
    };
}

/** 旧版单 hex → 调色板（兼容设置面板/预设主题预览的调用点） */
export function paletteFromHex(hex) {
    if (typeof hex !== 'string' || !/^#[0-9a-fA-F]{6}$/.test(hex)) return null;
    return {
        primary: hex,
        ink: ensureOklchContrast(hex, INK_TARGET_LUM, INK_MIN_RATIO),
        secondary: mix(hex, '#ffffff', 0.2),
        onPrimary: relLum(hex) > 0.45 ? '#0a0e17' : '#ffffff',
        neutral: false,
        tier: 1,
        reason: 'hex',
        swatches: []
    };
}

/** 调色板对象规范化（容错：缺字段补默认值） */
function normalizePalette(p) {
    if (!p || typeof p.primary !== 'string' || !/^#[0-9a-fA-F]{6}$/.test(p.primary)) return null;
    const paper = paletteFromHex(p.primary);
    return {
        ...paper,
        ...p,
        ink: /^#[0-9a-fA-F]{6}$/.test(p.ink || '') ? p.ink : paper.ink,
        secondary: /^#[0-9a-fA-F]{6}$/.test(p.secondary || '') ? p.secondary : paper.secondary
    };
}

/* ============================================================
 * 专辑取色（64px 采样 + MMCQ + OKLCH 双档选择 + 中性回退）
 * ============================================================ */

/** 调色板缓存：同一封面 URL 只解码/量化一次（并发请求复用同一个 Promise） */
const PALETTE_CACHE_MAX = 30;
const paletteCache = new Map();

/**
 * 从封面图提取主题调色板。
 *  - data:/blob: 直接解码（canvas 安全）
 *  - http(s)：crossOrigin=anonymous（CDN 需允许 CORS）；canvas 被 taint 时回退中性
 *  - 任何失败/超时都返回中性调色板（不再返回 null，避免"主题停在上一首"）
 * @param {string} src
 * @returns {Promise<Object>} palette
 */
export function extractAlbumPalette(src) {
    if (!src) return Promise.resolve(neutralPalette('no-src'));
    const cached = paletteCache.get(src);
    if (cached) return cached;
    const task = new Promise((resolve) => {
        const img = new Image();
        const timer = setTimeout(() => finish(neutralPalette('timeout')), 5000);
        const finish = (palette) => {
            clearTimeout(timer);
            resolve(palette);
        };
        img.onload = () => {
            try {
                const size = 64;
                const canvas = document.createElement('canvas');
                canvas.width = size;
                canvas.height = size;
                const ctx = canvas.getContext('2d', { willReadFrequently: true });
                ctx.drawImage(img, 0, 0, size, size);
                const data = ctx.getImageData(0, 0, size, size).data;
                finish(selectThemeRoles(quantizeMMCQ(data, 14)));
            } catch {
                finish(neutralPalette('canvas-tainted'));
            }
        };
        img.onerror = () => finish(neutralPalette('image-error'));
        if (/^https?:/i.test(src)) img.crossOrigin = 'anonymous';
        img.src = src;
    });
    paletteCache.set(src, task);
    if (paletteCache.size > PALETTE_CACHE_MAX) {
        // 淘汰最早插入的一条（Map 保持插入顺序）
        const oldest = paletteCache.keys().next().value;
        paletteCache.delete(oldest);
    }
    return task;
}

/**
 * 旧接口兼容：返回主色 hex（失败/无彩色返回中性银，不再返回 null）。
 * @param {string} src
 * @returns {Promise<string>}
 */
export async function extractAlbumColor(src) {
    const palette = await extractAlbumPalette(src);
    return palette ? palette.primary : null;
}

/* ============================================================
 * 状态栏歌词专用对比度安全配色
 * ============================================================ */

/**
 * 标准深/浅任务栏背景近似相对亮度（WCAG）。
 * dark 取 #202020（Windows 标准不透明深色任务栏，L=0.0144）：实测透明
 * 任务栏在深色壁纸下 L≈0.0037、任务栏空白处 0.005~0.0067，都暗于原假设
 * 的 #2c2c2c（0.03），原值会把暗色主题色过度提亮；用 #202020 作基准
 * 既能覆盖不透明深色任务栏，也贴近深色壁纸场景。
 * light 取 #f0f0f0（标准浅色任务栏）。
 */
const BAR_BG_LUM = { dark: 0.0144, light: 0.871 };
/** 状态栏歌词文字与浅色任务栏背景的最低对比度（WCAG AA 正文） */
const BAR_MIN_RATIO_LIGHT = 4.5;
/**
 * 深色任务栏的对比度目标（与主窗口 ink 统一）：ink 本身已对深色目标按
 * 2.8:1 生成；实测任务栏背景 L≈0.004，实际对比 4:1 以上。
 * 保持与主窗口同色优先（用户"颜色不统一"反馈）。
 */
const BAR_MIN_RATIO_DARK = 2.8;

/** 把 hex 向白/黑逐步混合，直到与 bgLum 对比度 ≥ minRatio（浅色分支用） */
function ensureBarContrast(hex, bgLum, minRatio, towardWhite) {
    let c = hex;
    for (let i = 0; i < 80; i++) {
        const l = relLum(c);
        const ratio = (Math.max(l, bgLum) + 0.05) / (Math.min(l, bgLum) + 0.05);
        if (ratio >= minRatio) return c;
        c = towardWhite ? mix(c, '#ffffff', 0.06) : mix(c, '#000000', 0.06);
    }
    return towardWhite ? '#ffffff' : '#000000';
}

/**
 * 保色相提亮：只调 HSL 亮度（色相/饱和度不变），二分找到达到
 * bgLum 上最低对比度的最小亮度。与 RGB 混白不同，不会把饱和色混成灰粉色。
 */
function ensureBarContrastHue(hex, bgLum, minRatio) {
    const need = minRatio * (bgLum + 0.05) - 0.05;
    if (relLum(hex) >= need) return hex;
    const [h, s, l] = hexToHsl(hex);
    let lo = l;
    let hi = 1;
    for (let i = 0; i < 32; i++) {
        const mid = (lo + hi) / 2;
        if (relLum(hslToHex(h, s, mid)) >= need) hi = mid;
        else lo = mid;
    }
    return hslToHex(h, s, hi);
}

/** 从渐变色 CSS 里提取两端色（仅匹配项目内使用的 hex；失败返回 null） */
function parseGradientEnds(gradientCss) {
    if (!gradientCss || !gradientCss.includes('-gradient(')) return null;
    const colors = gradientCss.match(/#[0-9a-fA-F]{6}/g);
    return colors && colors.length >= 2 ? [colors[0], colors[1]] : null;
}

/**
 * 状态栏歌词文字配色：
 *  - fg：逐字高亮单色 = 主题 ink（主窗口歌词高亮同一个 ink，保证"两边同色"）
 *  - gradA / gradB：纯文本行渐变两端，同样做对比度兜底
 *  - 深色任务栏：对 BAR_BG_LUM.dark 保色相提亮到 4.5:1（WCAG AA 正文）
 *  - 浅色任务栏：向黑混合到 4.5:1（"变深"适配，不是增亮）
 * @param {string} gradientCss - 当前 --theme-gradient 的 CSS 值
 * @param {boolean} taskbarLight - 任务栏是否浅色
 * @param {string} fallbackHex - 文字基准色（传 --theme-ink，缺省回退主色）
 * @returns {{fg: string, gradA: string, gradB: string}}
 */
export function barTextColors(gradientCss, taskbarLight, fallbackHex = '#14b8a6') {
    const [start, end] = parseGradientEnds(gradientCss) || [];
    const a = start || mix(fallbackHex, '#ffffff', 0.25);
    const b = end || mix(fallbackHex, '#000000', 0.35);

    if (taskbarLight) {
        // 浅色任务栏：主题亮色不可读，仅做向黑适配（非增亮）
        const fg = ensureBarContrast(mix(fallbackHex, '#000000', 0.3), BAR_BG_LUM.light, BAR_MIN_RATIO_LIGHT, false);
        return {
            fg,
            gradA: ensureBarContrast(b, BAR_BG_LUM.light, BAR_MIN_RATIO_LIGHT, false),
            gradB: ensureBarContrast(mix(b, '#000000', 0.55), BAR_BG_LUM.light, BAR_MIN_RATIO_LIGHT, false),
            // 未唱底字：与高亮同色系的深色（浅底上用更深的版本）
            dim: mix(fg, '#000000', 0.15)
        };
    }
    // 深色任务栏：保色相提亮到最低对比度（与主窗口 ink 统一，只调亮度）
    const bgLum = BAR_BG_LUM.dark;
    const fg = ensureBarContrastHue(fallbackHex, bgLum, BAR_MIN_RATIO_DARK);
    return {
        fg,
        gradA: ensureBarContrastHue(a, bgLum, BAR_MIN_RATIO_DARK),
        gradB: ensureBarContrastHue(b, bgLum, BAR_MIN_RATIO_DARK),
        // 未唱底字：高亮色的暗化版（同色系）。
        // 背景原因：高亮字用 background-clip:text 渲染，抗锯齿边缘是半透明的，
        // 底字若是白色就会从边缘透出（"红色没盖住白色"）；改成同色系暗色后，
        // 边缘透出的是暗红，视觉上是"红色盖住暗红"，不再泛白。
        // 混黑 0.55（用户 2026-10 反馈）：0.35 时已唱/未唱亮度只差 ~2.5×，
        // 卡拉OK扫光对比不分明；0.55 拉开到 ~5×，未唱仍隐约可辨。
        dim: mix(fg, '#000000', 0.55)
    };
}

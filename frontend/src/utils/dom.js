/**
 * DOM 查询与样式操作的小工具，避免重复样板代码。
 */

/** document.getElementById 的简写 */
export const $ = (id) => document.getElementById(id);

/**
 * 批量绑定多个 DOM 元素。
 * @param {Record<string, string>} ids - 逻辑名 -> 元素 id
 * @returns {Record<string, HTMLElement>}
 */
export function bindElements(ids) {
    const els = {};
    for (const [name, id] of Object.entries(ids)) {
        const el = document.getElementById(id);
        if (!el) {
            console.warn(`[dom] element #${id} not found`);
        }
        els[name] = el;
    }
    return els;
}

/** 仅当文本变化时才写入 textContent，避免无谓 reflow */
export function setTextIfChanged(el, text) {
    if (el && el.textContent !== text) el.textContent = text;
}

/** 安全设置 style.backgroundImage，自动包裹 url() */
export function setBgImage(el, url) {
    if (!el) return;
    el.style.backgroundImage = url ? `url("${url}")` : '';
}

/**
 * 专辑封面预加载状态：记录当前最新目标 URL。
 * 在飞行中的预加载（旧图）完成后发现目标已被更新/作废，则丢弃结果不回写，
 * 避免"新图先到、旧图后回"的闪烁与错图。
 */
let pendingArtUrl = null;
let artLoadTimer = null;
/** 最近一次请求的主图（用于防止高清升级后又被低清主图降级） */
let currentPrimary = '';
/** 最近一次请求主图对应的高清地址 */
let currentHd = null;
/** 当前实际显示的目标 */
let displayedUrl = '';

/** 在线封面最长等待时间（ms）：超时未加载完成也先应用，浏览器就绪后自会绘制 */
const ONLINE_ART_MAX_WAIT_MS = 3000;

function applyArtUrl(els, url) {
    const bg = `url("${url}")`;
    setBgImage(els.albumArt, url);
    if (els.albumBlurBg.style.backgroundImage !== bg) {
        setBgImage(els.albumBlurBg, url);
    }
    els.albumBlurBg.classList.add('active');
}

/**
 * 渐进式专辑封面加载。
 *
 *  - data:/blob:（SMTC 缩略图、本地 QQMusicPicture data URI）立即应用，无延迟
 *  - http(s) 在线图：先用 Image() 预加载，onload 后再应用——期间保留当前
 *    已显示的封面，消除"切到在线 URL 后背景空白/闪烁"的感知延迟
 *  - hdUrl（高清升级地址）：主图为低清本地图（150x150）时携带 800x800
 *    在线地址，后台预加载完成后平滑替换为高清图
 *  - 加载失败（404/断网）：保留当前封面，不显示破图
 *  - 超时兜底：仍直接应用，避免极端网络下永远停在低清图
 *
 * @param {Object} els
 * @param {string} url - 主图（data URI 或在线 URL）
 * @param {string} [hdUrl] - 高清升级地址（可为空）
 */
export function preloadAlbumArt(els, url, hdUrl) {
    if (!url) return;
    const target = url;
    // 已显示该主图 / 该请求已处理（可能已升级到高清）→ 跳过，防止升级后降级
    if (displayedUrl === target || (currentPrimary === target && currentHd === (hdUrl || null))) {
        return;
    }
    currentPrimary = target;
    currentHd = hdUrl || null;

    pendingArtUrl = target;
    const apply = () => {
        if (pendingArtUrl !== target) return; // 已有更新目标，过期请求不回写
        if (artLoadTimer) { clearTimeout(artLoadTimer); artLoadTimer = null; }
        displayedUrl = target;
        applyArtUrl(els, target);
        maybeUpgradeHd();
    };

    // 高清升级：低清主图显示后，预加载 800x800 在线图并平滑替换
    const maybeUpgradeHd = () => {
        if (!hdUrl || hdUrl === target || els.albumArt.dataset.upgradedHd === hdUrl) return;
        els.albumArt.dataset.upgradedHd = hdUrl;
        const hdImg = new Image();
        hdImg.onload = () => {
            if (pendingArtUrl !== target) return; // 期间已换歌/换图，放弃
            displayedUrl = hdUrl;
            applyArtUrl(els, hdUrl);
        };
        // 高清失败：保留低清主图即可，不处理
        hdImg.src = hdUrl;
    };

    if (url.startsWith('data:') || url.startsWith('blob:')) {
        apply();
        return;
    }

    const img = new Image();
    img.onload = apply;
    img.onerror = () => {
        // 加载失败：保留当前已显示封面。网络抖动/缓存偶发失败时
        // 2s 后重试一次，避免"加载不出"长时间空白
        if (pendingArtUrl !== target) return;
        if (els.albumArt.dataset.retried === target) return;
        els.albumArt.dataset.retried = target;
        setTimeout(() => {
            if (pendingArtUrl !== target) return;
            const retry = new Image();
            retry.onload = apply;
            retry.onerror = () => { if (pendingArtUrl === target) pendingArtUrl = null; };
            retry.src = target;
        }, 2000);
    };
    img.src = url;
    artLoadTimer = setTimeout(apply, ONLINE_ART_MAX_WAIT_MS);
}

/** 清空专辑封面（切歌 / 无播放 / 错误态），并作废所有进行中的预加载 */
export function resetAlbumArt(els) {
    pendingArtUrl = null;
    currentPrimary = '';
    currentHd = null;
    displayedUrl = '';
    if (els.albumArt) {
        delete els.albumArt.dataset.upgradedHd;
        delete els.albumArt.dataset.retried;
    }
    if (artLoadTimer) { clearTimeout(artLoadTimer); artLoadTimer = null; }
    setBgImage(els.albumArt, '');
    setBgImage(els.albumBlurBg, '');
    els.albumBlurBg.classList.remove('active');
}

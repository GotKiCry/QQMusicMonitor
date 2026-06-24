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

/**
 * 歌词区 DOM 构建：把后端的 qrc_data 渲染成可逐字扫光的歌词结构。
 *
 * 每行结构：
 *   <div class="lyric-line before">
 *     <span class="word">
 *       <span class="word-bg">字</span>      // 下层灰色底字
 *       <span class="word-fg" style="width:0%">字</span>  // 上层渐变剪裁高亮
 *     </span> ...
 *     <div class="lyric-trans">翻译</div>     // 可选
 *   </div>
 *
 * 渲染循环通过修改 .word-fg 的 width(0~100%) 实现逐字扫光。
 */

import { findTranslation } from './translation.js';

/**
 * @typedef {Object} WordEntry
 * @property {HTMLElement} el
 * @property {HTMLElement} fgEl
 * @property {number} start    - 绝对起始时间(ms)
 * @property {number} dur      - 持续时间(ms)
 * @property {number} width    - 当前剪裁宽度(0~100)，缓存避免重复写 DOM
 */

/**
 * @typedef {Object} LineEntry
 * @property {HTMLElement} el
 * @property {WordEntry[]} words
 * @property {number} start
 * @property {number} end
 * @property {'before'|'active'|'after'} state
 */

/**
 * 在歌词视口内构建全部歌词行。
 * @param {HTMLElement} viewport - #lyrics-viewport
 * @param {any} data - SongInfo
 * @param {Array<{time: number, text: string}>} transMap
 * @param {boolean} showTranslation
 * @returns {LineEntry[]}
 */
export function buildLyricsArea(viewport, data, transMap, showTranslation) {
    viewport.innerHTML = '';

    // 无逐字歌词：显示占位
    if (!data.qrc_data || data.qrc_data.length === 0) {
        const div = document.createElement('div');
        div.className = 'lyric-empty';
        div.textContent = data.lyrics ? data.lyrics.split('\n')[0] : '无逐字歌词';
        viewport.appendChild(div);
        return [];
    }

    /** @type {LineEntry[]} */
    const lines = [];
    const total = data.qrc_data.length;

    for (let idx = 0; idx < total; idx++) {
        const line = data.qrc_data[idx];
        const lineDiv = document.createElement('div');
        lineDiv.className = 'lyric-line before';

        // 首尾留白以实现居中滚动
        if (idx === 0) lineDiv.style.marginTop = '180px';
        if (idx === total - 1) lineDiv.style.marginBottom = '180px';

        /** @type {WordEntry[]} */
        const words = [];
        for (const w of line.words) {
            const span = document.createElement('span');
            span.className = 'word';

            const bgSpan = document.createElement('span');
            bgSpan.className = 'word-bg';
            bgSpan.textContent = w.content;

            const fgSpan = document.createElement('span');
            fgSpan.className = 'word-fg';
            fgSpan.textContent = w.content;
            fgSpan.style.width = '0%';

            span.appendChild(bgSpan);
            span.appendChild(fgSpan);
            lineDiv.appendChild(span);

            words.push({
                el: span,
                fgEl: fgSpan,
                start: w.start_time_ms,
                dur: w.duration_ms || 200,
                width: 0
            });
        }

        // 行有效结束时间：取行时长终点与最后一字终点的较大值
        const durEnd = line.duration_ms > 0 ? line.start_time_ms + line.duration_ms : 0;
        const lastWordEnd = line.words.length > 0
            ? line.words[line.words.length - 1].start_time_ms + line.words[line.words.length - 1].duration_ms
            : 0;
        let lineEnd = Math.max(durEnd, lastWordEnd);
        if (lineEnd <= 0) lineEnd = line.start_time_ms + 4000;

        // 翻译行
        const trans = findTranslation(transMap, line.start_time_ms);
        if (trans) {
            const transDiv = document.createElement('div');
            transDiv.className = 'lyric-trans';
            transDiv.textContent = trans;
            if (!showTranslation) transDiv.style.display = 'none';
            lineDiv.appendChild(transDiv);
        }

        viewport.appendChild(lineDiv);
        lines.push({
            el: lineDiv,
            words,
            start: line.start_time_ms,
            end: lineEnd,
            state: 'before'
        });
    }

    return lines;
}

/**
 * 切歌瞬间在视口内显示加载占位，避免旧歌词残影。
 * @param {HTMLElement} viewport
 */
export function showLoadingPlaceholder(viewport) {
    viewport.innerHTML = '<div class="lyric-loading">正在加载歌词...</div>';
}

/**
 * 切歌瞬间清空视口（无加载提示，用于明确无歌词场景）。
 * @param {HTMLElement} viewport
 */
export function clearLyricsViewport(viewport) {
    viewport.innerHTML = '';
}

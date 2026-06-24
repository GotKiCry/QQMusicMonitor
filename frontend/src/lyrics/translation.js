/**
 * 翻译歌词解析与查找。
 *
 * 翻译文本是带 LRC 时间标签的纯文本，如：
 *   [00:12.30]翻译内容
 * 解析为 { time(ms), text }[] 供歌词行按时间近似匹配。
 */

import { parseLrcTime } from '../utils/format.js';

/**
 * @param {string} text - 原始翻译文本
 * @returns {Array<{time: number, text: string}>}
 */
export function parseTranslation(text) {
    if (!text) return [];
    const entries = [];
    const re = /\[(\d{2}):(\d{2})(?:[.:](\d{2,3}))?\](.*)/;
    for (const line of text.split('\n')) {
        const trimmed = line.trim();
        const m = trimmed.match(re);
        if (!m) continue;
        const ms = (parseInt(m[1], 10) * 60 + parseInt(m[2], 10)) * 1000
            + (m[3] ? parseInt(m[3].padEnd(3, '0'), 10) : 0);
        const transText = m[4].trim();
        if (transText && transText !== '//') {
            entries.push({ time: ms, text: transText });
        }
    }
    entries.sort((a, b) => a.time - b.time);
    return entries;
}

/**
 * 在翻译表中查找与给定行起始时间最接近的翻译（容差 200ms）。
 * @param {Array<{time: number, text: string}>} transMap
 * @param {number} startTimeMs
 * @returns {string}
 */
export function findTranslation(transMap, startTimeMs) {
    if (!transMap || transMap.length === 0) return '';
    let bestText = '';
    let minDiff = Infinity;
    for (const entry of transMap) {
        const diff = Math.abs(entry.time - startTimeMs);
        if (diff <= 200 && diff < minDiff) {
            minDiff = diff;
            bestText = entry.text;
        }
    }
    return bestText;
}

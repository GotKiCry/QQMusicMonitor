/**
 * 歌词行解析共享模块（状态栏歌词条与迷你播放器共用）。
 *
 * 统一产出行数组：{start, end, text, words?: [{content, start, dur}]}
 *  - QRC 逐字歌词：保留逐字绝对时间（相对歌曲开头）
 *  - LRC 纯文本：按行级时间标签展开（支持一行多标签的重复副歌）
 */

/** QRC 行级构建（保留逐字绝对时间） */
export function rowsFromQrc(qrcData) {
    return qrcData.map((line) => {
        const words = (line.words || []).map((w) => ({
            content: w.content,
            start: w.start_time_ms,
            dur: w.duration_ms || 200
        }));
        const text = words.map((w) => w.content).join('');
        const durEnd = line.duration_ms > 0 ? line.start_time_ms + line.duration_ms : 0;
        const lastWord = line.words && line.words.length > 0
            ? line.words[line.words.length - 1]
            : null;
        // 词时间为绝对时间（相对歌曲开头），与 XML/QRC-text 解析器输出一致
        const lastWordEnd = lastWord ? lastWord.start_time_ms + lastWord.duration_ms : 0;
        let end = Math.max(durEnd, lastWordEnd);
        if (end <= 0) end = line.start_time_ms + 4000;
        return { start: line.start_time_ms, end, text, words };
    });
}

/** LRC 文本解析兜底（无逐字数据）；支持一行多时间标签（重复副歌） */
export function rowsFromLrc(lyrics) {
    const out = [];
    const tagRe = /\[(\d{2}):(\d{2})(?:[.:](\d{2,3}))?\]/g;
    for (const raw of (lyrics || '').split('\n')) {
        const trimmed = raw.trim();
        if (!trimmed) continue;
        const tags = [...trimmed.matchAll(tagRe)];
        if (tags.length === 0) continue;
        const lastTag = tags[tags.length - 1];
        const text = trimmed.slice(lastTag.index + lastTag[0].length).trim();
        if (!text) continue;
        for (const m of tags) {
            const ms = (parseInt(m[1], 10) * 60 + parseInt(m[2], 10)) * 1000
                + (m[3] ? parseInt(m[3].padEnd(3, '0'), 10) : 0);
            out.push({ start: ms, end: 0, text, words: [] });
        }
    }
    out.sort((a, b) => a.start - b.start);
    for (let i = 0; i < out.length; i++) {
        out[i].end = i + 1 < out.length ? out[i + 1].start : out[i].start + 5000;
    }
    return out;
}

/** 当前时间 t（ms）对应的歌词行索引；t 早于首行时返回 -1 */
export function rowIdxAt(rows, t) {
    for (let i = rows.length - 1; i >= 0; i--) {
        if (rows[i].start <= t) return i;
    }
    return -1;
}

/**
 * 60fps requestAnimationFrame 渲染循环。
 *
 * 职责：
 *  1. 基于插值基线（lastSampleProgress + 本地时钟流逝）推算当前播放时间
 *  2. 更新底部时间文本与进度条宽度
 *  3. 刷新各歌词行状态（before/active/after）与逐字剪裁宽度
 *  4. 活跃行变化时通过 CSS transform translateY 平滑居中滚动
 *
 * 切歌时 state.resetForSongChange() 会清空 lyricLines 并重置插值基线，
 * 因此本循环不会再用旧歌进度推进旧歌词高亮。
 */

import {
    songInfo, lyricLines, lastActiveIdx, isPlaying,
    lastSampleProgress, lastSampleLocalTime,
    setLastActiveIdx, setAnimFrame
} from '../state/state.js';
import { formatMs } from '../utils/format.js';
import { setTextIfChanged } from '../utils/dom.js';

/** @typedef {{offsetMs: number}} RenderConfig */

/**
 * @param {HTMLElement} elTimeCurrent
 * @param {HTMLElement} elTimeTotal
 * @param {HTMLElement} elProgressFill
 * @param {HTMLElement} elLyricsContainer - 用于计算居中高度
 * @param {HTMLElement} elLyricsViewport - 被 translateY 的容器
 * @param {RenderConfig} cfg
 */
export function startRenderLoop(elTimeCurrent, elTimeTotal, elProgressFill, elLyricsContainer, elLyricsViewport, cfg) {
    function loop() {
        setAnimFrame(requestAnimationFrame(loop));
        if (!songInfo) return;

        // 前进推算插值时间（含用户配置的音频延迟补偿）
        let t;
        if (isPlaying && lastSampleLocalTime > 0) {
            const elapsed = performance.now() - lastSampleLocalTime;
            t = lastSampleProgress + elapsed + cfg.offsetMs;
        } else {
            t = lastSampleProgress + cfg.offsetMs;
        }
        t = Math.max(0, t);

        // 底部时间与进度条
        const totalMs = (songInfo.total_time_ms && songInfo.total_time_ms > 0)
            ? songInfo.total_time_ms
            : songInfo.total_time * 1000;
        setTextIfChanged(elTimeCurrent, formatMs(t));
        setTextIfChanged(elTimeTotal, formatMs(totalMs));
        if (totalMs > 0) {
            const pct = Math.min(100, Math.max(0, (t / totalMs) * 100));
            elProgressFill.style.width = `${pct}%`;
        }

        if (lyricLines.length === 0) return;

        let activeIdx = -1;

        for (let i = 0; i < lyricLines.length; i++) {
            const line = lyricLines[i];

            let lineState;
            if (t < line.start) lineState = 'before';
            else if (t >= line.end) lineState = 'after';
            else { lineState = 'active'; activeIdx = i; }

            if (line.state !== lineState) {
                line.state = lineState;
                line.el.className = `lyric-line ${lineState}`;
            }

            if (lineState === 'before') {
                for (const w of line.words) {
                    if (w.width !== 0) { w.width = 0; w.fgEl.style.width = '0%'; }
                }
            } else if (lineState === 'after') {
                for (const w of line.words) {
                    if (w.width !== 100) { w.width = 100; w.fgEl.style.width = '100%'; }
                }
            } else {
                // 活跃行：逐字剪裁宽度
                for (const w of line.words) {
                    const wEnd = w.start + w.dur;
                    let targetWidth;
                    if (t >= wEnd) targetWidth = 100;
                    else if (t >= w.start) targetWidth = ((t - w.start) / w.dur) * 100;
                    else targetWidth = 0;

                    const quantWidth = Math.round(targetWidth * 10) / 10;
                    if (quantWidth !== w.width) {
                        w.width = quantWidth;
                        w.fgEl.style.width = `${quantWidth}%`;
                    }
                }
            }
        }

        // 活跃行变化时平滑居中
        if (activeIdx !== lastActiveIdx && activeIdx >= 0) {
            setLastActiveIdx(activeIdx);
            const activeLineEl = lyricLines[activeIdx].el;
            const viewportCenter = elLyricsContainer.clientHeight / 2;
            const targetScrollY = viewportCenter - activeLineEl.offsetTop - (activeLineEl.clientHeight / 2);
            elLyricsViewport.style.transform = `translateY(${targetScrollY}px)`;
        }
    }

    if (!requestAnimationFrame) return;
    loop();
}

/** 停止渲染循环 */
export function stopRenderLoop() {
    // 由调用方在断连时使用；animFrame 通过 setAnimFrame 更新
    // 这里 import 后 cancel 需要当前句柄，因此提供一个 helper
    import('../state/state.js').then(({ animFrame }) => {
        if (animFrame) cancelAnimationFrame(animFrame);
        setAnimFrame(0);
    });
}

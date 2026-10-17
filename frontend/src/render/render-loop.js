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
    songInfo, lyricLines, lastActiveIdx, clockRunning,
    lastSampleProgress, lastSampleLocalTime,
    setLastActiveIdx, setAnimFrame
} from '../state/state.js';
import { formatMs } from '../utils/format.js';
import { setTextIfChanged } from '../utils/dom.js';

/**
 * @param {HTMLElement} elTimeCurrent
 * @param {HTMLElement} elTimeTotal
 * @param {HTMLElement} elProgressFill
 * @param {HTMLElement} elLyricsContainer - 用于计算居中高度
 * @param {HTMLElement} elLyricsViewport - 被 translateY 的容器
 */
export function startRenderLoop(elTimeCurrent, elTimeTotal, elProgressFill, elLyricsContainer, elLyricsViewport) {
    function loop() {
        setAnimFrame(requestAnimationFrame(loop));
        if (!songInfo) return;

        // 后端采样已经完成 SMTC 漂移修正和用户偏移补偿；
        // 显示位置由 state 中的单调播放时钟驱动：clockRunning 为 true 时
        // 用本地单调时钟补齐两次采样之间的流逝时间，暂停/冻结时停在基线。
        let t;
        if (clockRunning && lastSampleLocalTime > 0) {
            const elapsed = Math.max(0, performance.now() - lastSampleLocalTime);
            t = lastSampleProgress + elapsed;
        } else {
            t = lastSampleProgress;
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

            // 行高亮驻留：读完当前行后到下一行开始前（句间空隙/间奏/暂停恢复），
            // 维持本行满格高亮，不熄灯变暗——高亮只在下一行开始时转移
            const nextStart = i + 1 < lyricLines.length ? lyricLines[i + 1].start : line.end;
            let lineState;
            if (t < line.start) lineState = 'before';
            else if (t >= nextStart) lineState = 'after';
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

        // 活跃行变化时平滑居中。
        // 空档兜底：当前时间不落在任何一行内（行间空隙/前奏/尾声）时
        // activeIdx 为 -1，若只按活跃行居中，启动/切歌后首次落在空档
        // 会一直停留在歌词列表顶部，留下大片空白。此时以「最近停留行」
        // 为居中锚点：前奏取第一行，空档取上一行（与正常播放中空档的
        // 停留位置一致），保证视图停在正确的空档位置。
        let anchorIdx = activeIdx;
        if (anchorIdx < 0 && lyricLines.length > 0) {
            if (t < lyricLines[0].start) {
                anchorIdx = 0;
            } else {
                for (let i = lyricLines.length - 1; i >= 0; i--) {
                    if (lyricLines[i].start <= t) { anchorIdx = i; break; }
                }
            }
        }

        if (anchorIdx !== lastActiveIdx && anchorIdx >= 0) {
            setLastActiveIdx(anchorIdx);
            const anchorLineEl = lyricLines[anchorIdx].el;
            const viewportCenter = elLyricsContainer.clientHeight / 2;
            const targetScrollY = viewportCenter - anchorLineEl.offsetTop - (anchorLineEl.clientHeight / 2);
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

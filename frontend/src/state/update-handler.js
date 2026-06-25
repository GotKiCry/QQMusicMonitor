/**
 * SongInfo 数据更新处理器：把后端推送的 SongInfo 应用到状态与 DOM。
 *
 * 这里是切歌 bug 修复的另一关键点：
 *  - 切歌瞬间无条件清空旧歌词 DOM 并显示加载占位
 *  - 调用 state.resetForSongChange() 重置插值基线，防止旧进度继续推进
 *  - 新歌第一帧若后端 timeline 滞后带了大进度（>5s），强制归零
 */

import {
    songInfo, isPlaying, lastSampleProgress, lastSampleLocalTime, lastRawTimeMs,
    lastRenderKey,
    setSongInfo, setIsPlaying, setLastSampleProgress, setLastSampleLocalTime, setLastRawTimeMs,
    setTransMap, setLastRenderKey, setLastActiveIdx, setLyricLines,
    resetForSongChange
} from './state.js';
import { parseTranslation } from '../lyrics/translation.js';
import { buildLyricsArea, showLoadingPlaceholder } from '../lyrics/builder.js';
import { setBgImage } from '../utils/dom.js';

/** 切歌后已收到几次非切歌更新（用于延迟歌词构建直到进度稳定） */
let postChangeCount = 0;

/**
 * @param {Object} els - DOM 元素集合
 * @param {any} data - 后端推送的 SongInfo
 * @param {Object} cfg - 前端偏好（用于 showTranslation）
 * @param {Object} backCfg - 后端配置引用（用于调试显示）
 * @param {number} lastSyncMsRef - 上次同步耗时（{val: number}，需按引用更新）
 */
export function handleSongInfoUpdate(els, data, cfg, backCfg, lastSyncMsRef) {
    const isSongChanged = !songInfo
        || songInfo.title !== data.title
        || songInfo.artist !== data.artist;

    // 解析后端时间字段
    const totalMs = data.total_time_ms && data.total_time_ms > 0
        ? data.total_time_ms
        : data.total_time * 1000;
    let rawTimeMs = data.current_time_ms && data.current_time_ms > 0
        ? data.current_time_ms
        : (totalMs * (data.progress_percent / 100));

    const oldIsPlaying = songInfo ? songInfo.is_playing : false;
    setIsPlaying(data.is_playing);
    setSongInfo(data);

    if (isSongChanged) {
        // —— 切歌：彻底清空旧歌词与插值基线 ——
        // 1) 立即清空歌词视口，显示加载占位，杜绝旧歌词残影继续渲染
        showLoadingPlaceholder(els.lyricsViewport);

        // 2) 清空歌词相关状态
        setLyricLines([]);
        setTransMap([]);
        setLastRenderKey('');
        setLastActiveIdx(-1);
        postChangeCount = 0;

        // 3) 清空旧专辑封面，避免上一首封面残留
        setBgImage(els.albumArt, '');
        setBgImage(els.albumBlurBg, '');
        els.albumBlurBg.classList.remove('active');

        // 4) 重置歌词视口滚动位置：防止旧歌 translateY 残留导致新歌词闪现错位
        els.lyricsViewport.style.transform = '';

        // 5) 立即重置进度条与时间显示，避免旧歌进度残留
        els.progressFill.style.width = '0%';
        els.timeCurrent.textContent = '00:00';
        els.timeTotal.textContent = '00:00';

        // 6) 后端 SMTC 切歌瞬间可能带着旧歌的 timeline（Position/EndTime），
        //    新歌刚开头不可能处于 >5s 位置，强制归零进度与时长，
        //    防止前端进度条/插值基线从旧歌位置继续走。
        if (data.total_time_ms > 5000) {
            data.total_time_ms = 0;
            data.total_time = 0;
        }
        const freshProgress = rawTimeMs > 5000 ? 0 : rawTimeMs;

        // 6) 重置插值基线：从这里开始按新歌进度推进
        resetForSongChange(freshProgress, performance.now());
        rawTimeMs = freshProgress;
    } else {
        // —— 同首歌的进度同步 ——
        const localEstimate = isPlaying && lastSampleLocalTime > 0
            ? lastSampleProgress + (performance.now() - lastSampleLocalTime)
            : lastSampleProgress;

        if (isPlaying !== oldIsPlaying) {
            // 播放/暂停切换：取较大值避免恢复时回退
            setLastRawTimeMs(rawTimeMs);
            setLastSampleProgress(Math.max(rawTimeMs, localEstimate));
            setLastSampleLocalTime(performance.now());
        } else if (Math.abs(rawTimeMs - localEstimate) > 1000) {
            // 大跳变（seek 或切歌后 timeline 更新）
            setLastRawTimeMs(rawTimeMs);
            setLastSampleProgress(rawTimeMs);
            setLastSampleLocalTime(performance.now());
        } else if (rawTimeMs >= localEstimate - 50) {
            // 正常：服务端大致等于或略超前本地估计，snap 到服务端
            setLastRawTimeMs(rawTimeMs);
            setLastSampleProgress(rawTimeMs);
            setLastSampleLocalTime(performance.now());
        }
        // else: 服务端落后本地时钟（罕见竞态），忽略此采样
    }

    // —— 左面板歌曲信息 ——
    els.songTitle.textContent = data.title || '等待播放';
    els.songArtist.textContent = data.artist || 'QQ音乐监听器';
    els.songAlbum.textContent = data.album || 'SMTC 模式';

    // —— 专辑封面 ——
    if (data.album_pic_url) {
        setBgImage(els.albumArt, data.album_pic_url);
        const newBg = `url("${data.album_pic_url}")`;
        if (els.albumBlurBg.style.backgroundImage !== newBg) {
            setBgImage(els.albumBlurBg, data.album_pic_url);
        }
        els.albumBlurBg.classList.add('active');
    } else if (!data.title || data.title === 'No music playing' || data.title === 'ERROR') {
        setBgImage(els.albumArt, '');
        setBgImage(els.albumBlurBg, '');
        els.albumBlurBg.classList.remove('active');
    }

    // —— 播放状态动画 ——
    if (data.is_playing) {
        els.albumArt.classList.add('playing');
    } else {
        els.albumArt.classList.remove('playing');
    }

    // —— 重建歌词 ——
    // 切歌后后端会将 total_time_ms/current_time_ms 归零，直到 SMTC 报告
    // 新歌的 timeline 才恢复非零值。只有 totalMs > 0 才表示 timeline 已就绪，
    // 此时 rawTimeMs 也是真实的播放进度，用 t 构建歌词不会闪回。
    // postChangeCount >= 20（约1秒）作为兜底，防止异常情况下永远不构建。
    const qrcLength = data.qrc_data ? data.qrc_data.length : 0;
    const renderKey = `${data.title}|${data.artist}|${qrcLength}`;
    const needRebuild = renderKey !== lastRenderKey;
    if (!isSongChanged) {
        postChangeCount++;
        if (qrcLength > 0 && needRebuild && (totalMs > 0 || postChangeCount >= 20)) {
            setLastRenderKey(renderKey);
            const newTransMap = parseTranslation(data.trans);
            setTransMap(newTransMap);
            const newLines = buildLyricsArea(els.lyricsViewport, data, newTransMap, cfg.showTranslation);
            setLyricLines(newLines);
            setLastActiveIdx(-1);
        }
    }

    // —— 调试面板 ——
    if (cfg.debug) {
        els.rawData.textContent = JSON.stringify(data, null, 2);
        els.debugInfo.textContent = `Offset: ${backCfg.offsetMs}ms | Poll: ${backCfg.intervalMs}ms | Sync: ${lastSyncMsRef.val.toFixed(1)}ms | QRC Lines: ${qrcLength}`;
    }
}

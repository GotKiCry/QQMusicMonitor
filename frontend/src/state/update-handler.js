/**
 * SongInfo 数据更新处理器：把后端推送的 SongInfo 应用到状态与 DOM。
 *
 * 这里是切歌 bug 修复的另一关键点：
 *  - 切歌瞬间无条件清空旧歌词 DOM 并显示加载占位
 *  - 调用 state.resetForSongChange() 重置插值基线，防止旧进度继续推进
 *  - 新歌第一帧若后端 timeline 滞后带了大进度（>5s），强制归零
 *
 * 时间推进统一交给共享的单调播放时钟（playback-clock.js）：服务端位置
 * 只用于限速纠偏，显示位置永不回退、不跳变，与状态栏歌词条共用同一实现。
 */

import {
    songInfo, lastRenderKey,
    setSongInfo, setIsPlaying, setLastSampleProgress, setLastSampleLocalTime,
    setClockRunning, setTransMap, setLastRenderKey, setLastActiveIdx, setLyricLines,
    resetForSongChange
} from './state.js';
import { createPlaybackClock } from './playback-clock.js';
import { parseTranslation } from '../lyrics/translation.js';
import { buildLyricsArea, showLoadingPlaceholder } from '../lyrics/builder.js';
import { preloadAlbumArt, resetAlbumArt } from '../utils/dom.js';
import { extractAlbumPalette, paintCustomTheme, AUTO_COLOR_KEY, AUTO_PALETTE_KEY } from '../config/theme.js';

/** 切歌后已收到几次非切歌更新（用于延迟歌词构建直到进度稳定） */
let postChangeCount = 0;

/** 主窗口单调播放时钟：把服务端位置流变成不回退、不跳变的显示位置 */
const playbackClock = createPlaybackClock();

/** 歌曲标识（切歌重置判定维度：歌名+歌手，与后端 SongInfo 一致） */
const songKeyOf = (data) => `${data.title}|${data.artist}`;

/** 自动主题取色请求序号：只允许最新一次请求的结果落地 */
let autoColorSeq = 0;

/** 最近一次发布的调色板签名：同一封面重复帧（update 每 100ms 一帧）不重复写库/广播 */
let lastAutoPaletteSig = '';

/**
 * @param {Object} els - DOM 元素集合
 * @param {any} data - 后端推送的 SongInfo
 * @param {Object} cfg - 前端偏好（用于 showTranslation）
 * @param {Object} backCfg - 后端配置引用（用于调试显示）
 * @param {number} lastSyncMsRef - 上次同步耗时（{val: number}，需按引用更新）
 */
export function handleSongInfoUpdate(els, data, cfg, backCfg, lastSyncMsRef) {
    // 区分「应用启动首帧」与「真正切歌」：启动时后端不存在
    // 「新歌名 + 旧 timeline」的错配窗口，首帧进度（如启动即
    // 暂停在歌曲中段）是权威值，不应套用下方的 >5s 归零启发式。
    const isFirstLoad = !songInfo;
    const isSongChanged = isFirstLoad
        || songInfo.title !== data.title
        || songInfo.artist !== data.artist;

    // 解析后端时间字段
    const totalMs = data.total_time_ms && data.total_time_ms > 0
        ? data.total_time_ms
        : data.total_time * 1000;
    let rawTimeMs = data.current_time_ms && data.current_time_ms > 0
        ? data.current_time_ms
        : (totalMs * (data.progress_percent / 100));

    // 后端时间戳与浏览器位于同一台机器，可用来补偿事件/WS 传输耗时。
    // 限制到 250ms，避免系统时钟调整或休眠恢复造成异常跳跃。
    if (data.is_playing && Number.isFinite(data.server_ts) && data.server_ts > 0) {
        const transportDelayMs = Math.min(250, Math.max(0, Date.now() - data.server_ts));
        rawTimeMs += transportDelayMs;
        if (totalMs > 0) rawTimeMs = Math.min(rawTimeMs, totalMs);
    }

    const songKey = songKeyOf(data);
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

        // 3) 清空旧专辑封面，避免上一首封面残留（同时作废进行中的预加载）
        resetAlbumArt(els);

        // 4) 重置歌词视口滚动位置：防止旧歌 translateY 残留导致新歌词闪现错位
        els.lyricsViewport.style.transform = '';

        // 5) 立即重置进度条与时间显示，避免旧歌进度残留
        els.progressFill.style.width = '0%';
        els.timeCurrent.textContent = '00:00';
        els.timeTotal.textContent = '00:00';

        // 6) 后端 SMTC 切歌瞬间可能带着旧歌的 timeline（Position/EndTime），
        //    新歌刚开头不可能处于 >5s 位置，强制归零进度与时长，
        //    防止前端进度条/插值基线从旧歌位置继续走。
        //    启动首帧无此错配窗口，直接信任后端位置——否则「启动即
        //    暂停在歌曲中段」时进度会先闪 0 再在下一帧跳回。
        let freshProgress = rawTimeMs;
        if (!isFirstLoad) {
            if (data.total_time_ms > 5000) {
                data.total_time_ms = 0;
                data.total_time = 0;
            }
            freshProgress = rawTimeMs > 5000 ? 0 : rawTimeMs;
        }

        // 7) 重置插值基线：以新歌进度重置单调播放时钟（首帧/切歌直接对齐服务端）
        resetForSongChange(freshProgress, performance.now());
        playbackClock.reset({ songKey, rawMs: freshProgress, isPlaying: data.is_playing });
        rawTimeMs = freshProgress;
    } else {
        // —— 同首歌的进度采样：交给单调播放时钟做限速纠偏 ——
        // 播放中按 1× 推进，服务端重锚阶跃只做「单次 ≤ elapsed×20%」的纠偏，
        // 显示位置永不回退、不跳变；|偏差|>700ms 判 seek 直接对齐；
        // 暂停且服务端位置停滞时冻结显示；is_playing=false 但位置仍在前进
        // （WASAPI 门控静音段）与 timeline 归零窗口都继续按 1× 走。
        playbackClock.update({
            songKey,
            rawMs: rawTimeMs,
            isPlaying: data.is_playing,
            totalMs
        });
    }

    // —— 时钟基线同步到 state ——
    // 渲染循环仍按「基线位置 + 本地单调流逝」公式插值；
    // running 决定是否推进（暂停/冻结时停在基线）。
    const b = playbackClock.baseline();
    setLastSampleProgress(b.progress);
    setLastSampleLocalTime(b.localTime);
    setClockRunning(b.running);
    setIsPlaying(data.is_playing);

    // —— 左面板歌曲信息 ——
    els.songTitle.textContent = data.title || '等待播放';
    els.songArtist.textContent = data.artist || 'QQ音乐监听器';
    els.songAlbum.textContent = data.album || 'SMTC 模式';

    // —— 专辑封面（渐进式加载）——
    // 本地/SMTC data URI 立即应用；在线 URL 预加载完成后再应用，期间保留
    // 当前封面，消除 CDN 加载延迟造成的空白/闪烁。
    // album_pic_hd_url：低清本地图(150x150)时携带 800x800 高清地址，
    // 主图先即时显示，高清预加载完成后平滑升级。
    if (data.album_pic_url) {
        preloadAlbumArt(els, data.album_pic_url, data.album_pic_hd_url);
    } else if (!data.title || data.title === 'No music playing' || data.title === 'ERROR') {
        resetAlbumArt(els);
    }

    // —— 自动主题（专辑取色）——
    // 主窗口提取调色板并发布到 localStorage（歌词条窗口经 storage 事件跟随），
    // 序号保证切歌竞态下只有最新结果落地。
    // 黑白/低饱和封面、取色失败 → 中性银回退（不再继承上一首颜色，
    // 避免"主题色不是当前歌"的观感）；同一封面 URL 有缓存，重复帧不重复解码。
    //
    // 注意：即使当前主题不是 auto 也提取调色板——设置面板的 AUTO 按钮需要
    // 始终显示「当前歌曲」的取色（切歌实时刷新）；只有 theme=auto 才应用为
    // 当前主题。无封面帧保持当前主题（切歌瞬间 SMTC 常滞后几帧才补封面）。
    {
        const seq = ++autoColorSeq;
        const artSrc = data.album_pic_hd_url || data.album_pic_url;
        if (artSrc) {
            extractAlbumPalette(artSrc).then((palette) => {
                if (seq !== autoColorSeq) return;
                // 同一封面：取色结果不变，不重复写库 / 广播（每 100ms 一帧）
                const sig = `${palette.primary}|${palette.ink}`;
                if (sig === lastAutoPaletteSig) return;
                lastAutoPaletteSig = sig;
                try {
                    localStorage.setItem(AUTO_COLOR_KEY, palette.primary);
                    localStorage.setItem(AUTO_PALETTE_KEY, JSON.stringify(palette));
                } catch {}
                if (cfg.theme === 'auto') paintCustomTheme(palette);
                // 通知设置面板刷新 AUTO 按钮的色点/hex（切歌实时跟随）
                window.dispatchEvent(new Event('auto-color-updated'));
            });
        }
    }

    // —— 播放状态动画 ——
    if (data.is_playing) {
        els.albumArt.classList.add('playing');
    } else {
        els.albumArt.classList.remove('playing');
    }

    // —— 重建歌词 ——
    // buildLyricsArea 只按 qrc_data 构建纯 DOM，不依赖时间轴；
    // 滚动/活跃行定位完全由渲染循环基于插值基线驱动，timeline 就绪后
    // 会自动纠正。因此歌词数据一到达即可构建，无需等待 totalMs>0——
    // 否则每次切歌都会先闪过"正在加载歌词"占位（后端切歌后会把
    // timeline 归零直到 SMTC 报新歌数据）。
    // LRC 兜底路径同理：歌词文本到达即可构建；数据尚未到达时保留
    // postChangeCount 兜底（防止把"加载中"误判为"纯音乐"）。
    const qrcLength = data.qrc_data ? data.qrc_data.length : 0;
    const renderKey = `${data.title}|${data.artist}|${qrcLength}`;
    const needRebuild = renderKey !== lastRenderKey;
    if (!isSongChanged) {
        postChangeCount++;
        if (qrcLength > 0 && needRebuild) {
            setLastRenderKey(renderKey);
            const newTransMap = parseTranslation(data.trans);
            setTransMap(newTransMap);
            const newLines = buildLyricsArea(els.lyricsViewport, data, newTransMap, cfg.showTranslation);
            setLyricLines(newLines);
            setLastActiveIdx(-1);
        } else if (qrcLength === 0 && needRebuild && (data.lyrics || '').trim()) {
            setLastRenderKey(renderKey);
            const newLines = buildLyricsArea(els.lyricsViewport, data, [], cfg.showTranslation);
            setLyricLines(newLines);
            setLastActiveIdx(-1);
        } else if (qrcLength === 0 && postChangeCount === 20) {
            setLastRenderKey(renderKey);
            const newLines = buildLyricsArea(els.lyricsViewport, data, [], cfg.showTranslation);
            setLyricLines(newLines);
        }
    }

    // —— 调试面板 ——
    if (cfg.debug) {
        els.rawData.textContent = JSON.stringify(data, null, 2);
        els.debugInfo.textContent = `Offset: ${backCfg.offsetMs}ms | Poll: ${backCfg.intervalMs}ms | Sync: ${lastSyncMsRef.val.toFixed(1)}ms | QRC Lines: ${qrcLength}`;
    }
}

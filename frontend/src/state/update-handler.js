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
    lastRenderKey, lastServerPosMs,
    setSongInfo, setIsPlaying, setLastSampleProgress, setLastSampleLocalTime, setLastRawTimeMs,
    setTransMap, setLastRenderKey, setLastActiveIdx, setLyricLines,
    resetForSongChange, setLastServerPosMs
} from './state.js';
import { parseTranslation } from '../lyrics/translation.js';
import { buildLyricsArea, showLoadingPlaceholder } from '../lyrics/builder.js';
import { preloadAlbumArt, resetAlbumArt } from '../utils/dom.js';
import { extractAlbumColor, paintCustomTheme, AUTO_COLOR_KEY } from '../config/theme.js';

/** 切歌后已收到几次非切歌更新（用于延迟歌词构建直到进度稳定） */
let postChangeCount = 0;

/** 服务端采样持续落后本地估计的采样数（死区收敛用） */
let serverBehindCount = 0;

/** 死区内允许的最大连续落后采样数：超出则判定为系统性偏差，snap 收敛 */
const SERVER_BEHIND_SNAP_SAMPLES = 5;

/** 自动主题取色请求序号：只允许最新一次请求的结果落地 */
let autoColorSeq = 0;

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

    const oldIsPlaying = songInfo ? songInfo.is_playing : false;
    setIsPlaying(data.is_playing);
    setSongInfo(data);
    // 未加传输补偿的服务端位置：淡出尾音判定的基准（钉住时严格不变，
    // 不受 transportDelay 抖动影响）。先取上一帧值再更新
    const prevServerPosMs = lastServerPosMs;
    const serverPosMs = data.current_time_ms > 0 ? data.current_time_ms : -1;
    setLastServerPosMs(serverPosMs);

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
        serverBehindCount = 0;

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

        // 6) 重置插值基线：从这里开始按新歌进度推进
        resetForSongChange(freshProgress, performance.now());
        rawTimeMs = freshProgress;
    } else {
        // —— 同首歌的进度同步 ——
        const localEstimate = isPlaying && lastSampleLocalTime > 0
            ? lastSampleProgress + Math.max(0, performance.now() - lastSampleLocalTime)
            : lastSampleProgress;

        if (isPlaying !== oldIsPlaying) {
            serverBehindCount = 0;
            setLastRawTimeMs(rawTimeMs);
            if (!isPlaying) {
                // play → pause: 后端已把淡出窗口建模完毕——报暂停时携带的
                // 就是 QQ 提交的「最终位置」（淡出结束点，恢复播放也从这里
                // 继续），是权威值。本地插值因事件/传输延迟必然略超前或滞后，
                // 直接 snap 到服务端位置，冻结在真实暂停点。
                setLastSampleProgress(rawTimeMs);
                setLastSampleLocalTime(performance.now());
            } else {
                // pause → play: 以服务端位置为准，但服务端常略落后于暂停基线
                // （QQ 注册暂停的时刻早于后端轮询观察到，差距一般 <200ms）。
                // Math.max 锁存会把虚高基线永久固化（歌词超前）；
                // 直接 snap 又回退闪灰。改为把本地时间基准向未来平移落后量：
                // 显示位置原地 hold（render-loop 对负 elapsed clamp 为 0），
                // 等服务端追平后自然同速前进——不回跳也不累积超前。
                const behindMs = lastSampleProgress - rawTimeMs;
                if (behindMs > 0 && behindMs <= 500) {
                    setLastSampleLocalTime(performance.now() + behindMs);
                } else {
                    // 服务端超前（如暂停中 seek 到新位置）或大偏差 → 直接 snap
                    setLastSampleProgress(rawTimeMs);
                    setLastSampleLocalTime(performance.now());
                }
            }
        } else if (!isPlaying) {
            // 暂停期间：服务端位置恒定且权威（QQ 提交值即其自身显示/恢复点），
            // 直接跟随——同时覆盖暂停中 seek（任意幅度）与切换帧残留的
            // 本地超调，不会再把淡出提交误判成 seek 而突然前跳。
            serverBehindCount = 0;
            setLastSampleProgress(rawTimeMs);
            setLastSampleLocalTime(performance.now());
        } else if (totalMs <= 0) {
            // 切歌后 timeline 归零窗口（新歌 timeline 尚未就绪）：
            // 服务端 current_time 是占位值（归零+偏移，恒定不变）。
            // 若按下方"淡出尾音/seek/死区"逻辑处理，会把插值基线反复
            // 钉回占位值——歌词开头逐字高亮来回跳 = 疯狂闪烁。
            // 这里让基线随本地时钟自然前进，timeline 就绪后由下方分支对齐。
            serverBehindCount = 0;
            setLastSampleProgress(localEstimate);
            setLastSampleLocalTime(performance.now());
        } else if (Math.abs(rawTimeMs - localEstimate) > 1000) {
            // 大跳变（seek 或切歌后 timeline 更新）
            serverBehindCount = 0;
            setLastRawTimeMs(rawTimeMs);
            setLastSampleProgress(rawTimeMs);
            setLastSampleLocalTime(performance.now());
        } else if (serverPosMs > 0 && serverPosMs === prevServerPosMs && localEstimate > serverPosMs) {
            // 淡出尾音：后端在静音确认期间把位置钉在最后有声点（即将冻结）。
            // 正常播放时后端每帧都在前进，「is_playing=true 且未补偿位置不变」
            // 只出现在尾音窗口——钉住显示并停止插值蠕升，冻结时刻
            // （play→pause snap）的回退量压到 ≈0。
            // 注意：此分支仅当 totalMs > 0（timeline 就绪）时可达，
            // 归零窗口已由上方 totalMs <= 0 分支接管。
            serverBehindCount = 0;
            setLastSampleProgress(serverPosMs);
            setLastSampleLocalTime(performance.now());
        } else if (localEstimate - rawTimeMs > 250) {
            // 播放中的向后移动（>250ms）几乎只会是 seek：立即跟随。
            // 死区（见下）会把小幅回退 seek 拖慢 ~0.5s；锚点抖动 ±50ms
            // 不会触达该阈值
            serverBehindCount = 0;
            setLastRawTimeMs(rawTimeMs);
            setLastSampleProgress(rawTimeMs);
            setLastSampleLocalTime(performance.now());
        } else if (rawTimeMs >= localEstimate - 50) {
            // 正常：服务端大致等于或略超前本地估计，snap 到服务端
            serverBehindCount = 0;
            setLastRawTimeMs(rawTimeMs);
            setLastSampleProgress(rawTimeMs);
            setLastSampleLocalTime(performance.now());
        } else {
            // 死区（服务端落后 50~1000ms）：多为传输/快照抖动，立即 snap 会
            // 造成歌词回跳，先容忍；但持续 5 个采样（~0.5s）未收敛说明是系统
            // 性偏差——两端同速前进差距永不缩小，必须 snap 回服务端强制收敛。
            serverBehindCount++;
            if (serverBehindCount >= SERVER_BEHIND_SNAP_SAMPLES) {
                serverBehindCount = 0;
                setLastRawTimeMs(rawTimeMs);
                setLastSampleProgress(rawTimeMs);
                setLastSampleLocalTime(performance.now());
            }
        }
    }

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
    // 主窗口负责提取封面主色并发布到 localStorage（歌词条窗口经
    // storage 事件跟随），序号保证切歌竞态下只有最新结果落地。
    if (cfg.theme === 'auto') {
        const seq = ++autoColorSeq;
        if (data.album_pic_url) {
            extractAlbumColor(data.album_pic_url).then((hex) => {
                if (seq !== autoColorSeq || cfg.theme !== 'auto') return;
                try {
                    if (hex) localStorage.setItem(AUTO_COLOR_KEY, hex);
                    else localStorage.removeItem(AUTO_COLOR_KEY);
                } catch {}
                paintCustomTheme(hex);
            });
        } else {
            try {
                localStorage.removeItem(AUTO_COLOR_KEY);
            } catch {}
            paintCustomTheme(null);
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

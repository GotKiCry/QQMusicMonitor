/**
 * 单调播放时钟（主窗口与状态栏歌词条共用）。
 *
 * 背景：QQ 音乐 SMTC timeline 每 ~330ms 才刷新一次，后端漂移校正会在
 * 每次重锚时产生 ±20~100ms 的位置阶跃（实测 ~2.3 次/秒）。若前端把每个
 * 服务端位置直接 snap 到插值基线，逐字高亮会来回 1~2 个字、行边界处整行
 * 来回翻转；暂停/恢复边界同理。
 *
 * 本模块把「服务端位置流」变成「单调、不回退、不跳跃的显示位置」：
 *  - 播放中：按本地单调时钟 1× 推进；每次服务端采样只做限速纠偏
 *    （单次修正 ≤ elapsed × maxRateAdjust，等效瞬时速率 0.8~1.2×），
 *    偏差超过 seekSnapMs 才判定为 seek/切歌并直接对齐；
 *  - 暂停中：冻结显示位置（不跟随冻结值的微调），只有明显 seek
 *    （> seekSnapMs）才对齐；
 *  - 服务端位置仍在前进但 is_playing=false（WASAPI 门控把长时间静音
 *    段落判成暂停）时继续按 1× 推进，避免间奏歌词冻住后跳变；
 *  - 切歌 / 首帧 / timeline 归零窗口：按服务端位置重置或 1× 先走。
 *
 * 只依赖 performance.now()，无 DOM 依赖，两个窗口复用同一实现，
 * 避免此前「双窗口各写一份同步策略」再次漂移。
 */

/** seek / 切歌判定阈值（ms）：显示位置与观测位置偏差超过它才直接对齐 */
const DEFAULT_SEEK_SNAP_MS = 700;
/** 纠偏速率上限：单次修正 ≤ elapsed × 0.2（等效瞬时速率 0.8~1.2×） */
const DEFAULT_MAX_RATE_ADJUST = 0.2;
/** 单次纠偏下限（ms）：elapsed≈0 时仍保留最小纠偏能力 */
const MIN_ADJUST_MS = 2;
/** 「服务端位置仍在前进」判定的相对速率阈值（相对墙钟） */
const SERVER_MOVING_RATIO = 0.5;
/** 「服务端位置仍在前进」判定的最小位移（ms） */
const SERVER_MOVING_MIN_MS = 8;

/**
 * 创建单调播放时钟。
 * @param {Object} [options]
 * @param {number} [options.seekSnapMs] - seek 判定阈值（ms），两窗必须一致
 * @param {number} [options.maxRateAdjust] - 纠偏速率上限（0.2 = ±20%）
 */
export function createPlaybackClock(options = {}) {
    const seekSnapMs = Number.isFinite(options.seekSnapMs)
        ? options.seekSnapMs
        : DEFAULT_SEEK_SNAP_MS;
    const maxRateAdjust = Number.isFinite(options.maxRateAdjust)
        ? options.maxRateAdjust
        : DEFAULT_MAX_RATE_ADJUST;

    let valid = false;
    let songKey = '';
    let progress = 0;    // 基线显示位置（ms）
    let localTime = 0;   // progress 对应的 performance.now()
    let running = false; // 显示位置是否随时间前进
    let lastRaw = -1;    // 上一次服务端位置（判断是否仍在前进）
    let lastRawAt = 0;   // 上一次服务端位置对应的 performance.now()
    let movingStreak = 0; // 连续「观测在前进」的帧数（暂停提交 vs 门控静音段）

    const nowMs = () => (typeof performance !== 'undefined' ? performance.now() : Date.now());

    const predict = (at) => (running ? progress + Math.max(0, at - localTime) : progress);

    const baselineView = () => ({ progress, localTime, running });

    function settle(rawMs, run, at) {
        progress = Number.isFinite(rawMs) ? Math.max(0, rawMs) : 0;
        localTime = at;
        running = !!run;
        lastRaw = progress;
        lastRawAt = at;
        movingStreak = 0;
        return baselineView();
    }

    const clock = {
        /** 切歌 / 首帧 / 强制对齐：以服务端位置重置基线 */
        reset({ songKey: key = '', rawMs = 0, isPlaying = false } = {}) {
            valid = true;
            songKey = key || '';
            return settle(rawMs, !!isPlaying, nowMs());
        },

        /**
         * 进度采样：校准显示基线（每个服务端进度帧调用一次）。
         * @param {{songKey?:string, rawMs?:number, isPlaying?:boolean, totalMs?:number}} sample
         * @returns {{progress:number, localTime:number, running:boolean}}
         */
        update({ songKey: key = '', rawMs = 0, isPlaying = false, totalMs = 0 } = {}) {
            const at = nowMs();
            const raw = Number.isFinite(rawMs) ? Math.max(0, rawMs) : 0;
            if (!valid || (key && songKey && key !== songKey)) {
                return clock.reset({ songKey: key, rawMs: raw, isPlaying });
            }

            const predicted = predict(at);
            const diff = raw - predicted;

            // 1) timeline 归零窗口（切歌后新歌 timeline 尚未就绪）：服务端
            //    位置是恒定占位值，不能据此纠偏、也不能用「偏差 > 阈值」判
            //    seek（窗口超过阈值时会被打回 0）。始终按 1× 自然前进，
            //    timeline 就绪后由限速纠偏收敛
            if (totalMs <= 0 && isPlaying) {
                progress = predicted;
                localTime = at;
                running = true;
                lastRaw = raw;
                lastRawAt = at;
                movingStreak = 0;
                return baselineView();
            }

            // 2) 明显跳变（seek / timeline 更新 / 恢复时大偏差）→ 直接对齐
            if (Math.abs(diff) > seekSnapMs) {
                return settle(raw, !!isPlaying, at);
            }

            // 3) 服务端是否仍在前进：is_playing 只是播放器「声称」的状态，
            //    WASAPI 门控会在长时间静音段落把 is_playing 翻成 false，
            //    此时位置仍按 1× 前进，歌词不能冻结。
            //    is_playing=false 时要求连续 2 帧都在前进：暂停瞬间 QQ 会
            //    提交一次冻结值（一次性位移），单帧不足以证明音乐还在走
            const rawDt = at - lastRawAt;
            const rawAdvance = lastRaw >= 0 ? raw - lastRaw : 0;
            const obsAdvancing = rawDt > 0
                && rawAdvance > Math.max(SERVER_MOVING_MIN_MS, rawDt * SERVER_MOVING_RATIO);
            movingStreak = obsAdvancing ? movingStreak + 1 : 0;
            const serverMoving = !!isPlaying || movingStreak >= 2;

            if (!serverMoving) {
                // 真暂停 / 恢复 hold：冻结在当前显示位置，不跟随冻结值微调
                progress = predicted;
                localTime = at;
                running = false;
                lastRaw = raw;
                lastRawAt = at;
                return baselineView();
            }

            // 4) 限速纠偏：单次修正上限 = elapsed × maxRateAdjust，
            //    且 next 不低于上一基线位置 → 显示位置单调不回退
            const elapsed = running ? Math.max(0, at - localTime) : 0;
            const maxAdj = Math.max(MIN_ADJUST_MS, elapsed * maxRateAdjust);
            const adjust = Math.max(-maxAdj, Math.min(maxAdj, diff));
            progress = Math.max(progress, predicted + adjust);
            localTime = at;
            running = true;
            lastRaw = raw;
            lastRawAt = at;
            return baselineView();
        },

        /** 当前显示位置（渲染循环 / rAF 每帧调用） */
        position(at = nowMs()) {
            return valid ? predict(at) : 0;
        },

        /** 基线快照（供「progress + 本地流逝」插值公式使用） */
        baseline() {
            return valid ? baselineView() : { progress: 0, localTime: 0, running: false };
        },

        get running() {
            return running;
        },
        get progress() {
            return progress;
        },
        get songKey() {
            return songKey;
        },
    };

    return clock;
}
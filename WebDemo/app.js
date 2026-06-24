// @ts-check
/**
 * QQMusic Monitor - App Controller
 * 
 * - 结合了 Tauri Event 订阅与经典 WebSocket 兼容。
 * - 实现了高档的双向 Tauri Config 读写同步，直接更新 config.toml 并让后端实时生效。
 * - 实现了可视状态侦听与性能限速（隐藏时停止帧渲染并向后端降频）。
 * - 实现了基于 CSS transform translateY 的高级丝滑居中滚动。
 * - 基于高精度 requestAnimationFrame 的 60FPS 逐字渐变 KTV 扫光。
 */

/* ============================================================
 * Configuration Model & Local Defaults
 * ============================================================ */

const STORAGE_KEY = 'qqmusic-monitor-front-prefs';

/**
 * @typedef {Object} FrontendConfig
 * @property {number} fontSize
 * @property {number} fontWeight
 * @property {boolean} showTranslation
 * @property {string} theme
 * @property {boolean} debug
 */

/**
 * @typedef {Object} BackendConfig
 * @property {number} port
 * @property {number} offsetMs
 * @property {number} intervalMs
 * @property {boolean} outputTxt
 * @property {boolean} outputJson
 * @property {boolean} outputLyric
 */

/** @returns {FrontendConfig} */
function getDefaultFrontendConfig() {
    return {
        fontSize: 26,
        fontWeight: 700,
        showTranslation: true,
        theme: 'aurora-cyan',
        debug: false
    };
}

/** @returns {FrontendConfig} */
function loadFrontendConfig() {
    const base = getDefaultFrontendConfig();
    try {
        const raw = localStorage.getItem(STORAGE_KEY);
        if (!raw) return base;
        const parsed = JSON.parse(raw);
        return {
            fontSize: Number.isInteger(parsed.fontSize) ? parsed.fontSize : base.fontSize,
            fontWeight: Number.isInteger(parsed.fontWeight) ? parsed.fontWeight : base.fontWeight,
            showTranslation: typeof parsed.showTranslation === 'boolean' ? parsed.showTranslation : base.showTranslation,
            theme: typeof parsed.theme === 'string' ? parsed.theme : base.theme,
            debug: typeof parsed.debug === 'boolean' ? parsed.debug : base.debug
        };
    } catch {
        return base;
    }
}

/** @param {FrontendConfig} cfg */
function saveFrontendConfig(cfg) {
    try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(cfg));
    } catch {}
}

/* ============================================================
 * DOM Element Bindings
 * ============================================================ */

const $ = (id) => document.getElementById(id);

// Playback Deck
const elTitle = $('song-title');
const elArtist = $('song-artist');
const elAlbum = $('song-album');
const elProgressFill = $('progress-fill');
const elTimeCurrent = $('time-current');
const elTimeTotal = $('time-total');
const elVinyl = $('vinyl-record');
const elStylus = $('stylus-arm');
const elVinylCenterArt = $('vinyl-center-art');
const elAlbumBlurBg = $('album-blur-bg');

// Lyrics Area
const elLyricsContainer = $('lyrics-container');
const elLyricsViewport = $('lyrics-viewport');

// Header
const elStatusDot = $('status-dot');
const elStatusText = $('status-text');
const elBtnSettings = $('btn-settings');

// Drawer & Controls
const elSettingsOverlay = $('settings-overlay');
const elBtnSettingsClose = $('btn-settings-close');

// Inputs
const elCfgOffset = /** @type {HTMLInputElement} */ ($('cfg-offset'));
const elValOffset = $('val-offset');
const elBtnOffsetDec = $('btn-offset-dec');
const elBtnOffsetInc = $('btn-offset-inc');
const elCfgInterval = /** @type {HTMLInputElement} */ ($('cfg-interval'));
const elValInterval = $('val-interval');
const elCfgPort = /** @type {HTMLInputElement} */ ($('cfg-port'));
const elBtnApplyPort = $('btn-apply-port');

const elChkOutTxt = /** @type {HTMLInputElement} */ ($('chk-out-txt'));
const elChkOutJson = /** @type {HTMLInputElement} */ ($('chk-out-json'));
const elChkOutLyric = /** @type {HTMLInputElement} */ ($('chk-out-lyric'));

const elCfgFontSize = /** @type {HTMLInputElement} */ ($('cfg-font-size'));
const elValFontSize = $('val-font-size');
const elCfgFontWeight = /** @type {HTMLInputElement} */ ($('cfg-font-weight'));
const elValFontWeight = $('val-font-weight');
const elToggleTrans = /** @type {HTMLInputElement} */ ($('toggle-trans'));
const elToggleDebug = /** @type {HTMLInputElement} */ ($('toggle-debug'));

// Debug component
const elDebugPanel = $('debug-panel');
const elDebugInfo = $('debug-info');
const elRawData = $('raw-data');

/* ============================================================
 * State Properties
 * ============================================================ */

/** @type {FrontendConfig} */
let frontConfig = loadFrontendConfig();

/** @type {BackendConfig} */
let backConfig = {
    port: 3000,
    offsetMs: 200,
    intervalMs: 100,
    outputTxt: false,
    outputJson: true,
    outputLyric: true
};

/** @type {WebSocket | null} */
let ws = null;
let reconnectAttempts = 0;
let reconnectTimer = 0;
let isIntentionalClose = false;

/** @type {any | null} */
let songInfo = null;
/** @type {any[]} */
let lyricLines = [];
/** @type {any[]} */
let transMap = [];
let lastRenderKey = '';
let lastActiveIdx = -1;

let animFrame = 0;
let lastSyncMs = 0;

// High-fidelity dynamic jitter-proof timer
let lastSampleProgress = 0;
let lastSampleLocalTime = 0;
let lastRawTimeMs = -1;
let isPlaying = false;

/* ============================================================
 * Connection Handler (WebSocket Fallback)
 * ============================================================ */

/** @param {'connected' | 'connecting' | 'disconnected'} state */
function updateConnectionUI(state) {
    elStatusDot.className = 'status-indicator';
    if (state === 'connected') {
        elStatusDot.classList.add('connected');
        elStatusText.textContent = 'ACTIVE';
    } else if (state === 'connecting') {
        elStatusDot.classList.add('connecting');
        elStatusText.textContent = 'CONNECTING';
    } else {
        elStatusDot.classList.add('disconnected');
        elStatusText.textContent = 'OFFLINE';
    }
}

function connect() {
    if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
        return;
    }
    isIntentionalClose = false;
    updateConnectionUI('connecting');
    cancelAnimationFrame(animFrame);
    animFrame = 0;

    const host = location.hostname || '127.0.0.1';
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const wsUrl = `${proto}://${host}:${backConfig.port}/ws`;
    
    /** @type {WebSocket} */
    let socket;
    try {
        socket = new WebSocket(wsUrl);
    } catch {
        scheduleReconnect();
        return;
    }
    ws = socket;

    socket.onopen = () => {
        if (ws !== socket) return;
        reconnectAttempts = 0;
        updateConnectionUI('connected');
        if (!animFrame) loop();
    };

    socket.onmessage = (e) => {
        if (ws !== socket) return;
        const t0 = performance.now();
        try {
            const data = JSON.parse(e.data);
            if (data && typeof data === 'object') {
                onUpdate(data);
            }
            lastSyncMs = performance.now() - t0;
        } catch {}
    };

    socket.onerror = () => {
        if (ws !== socket) return;
        try { socket.close(); } catch {}
    };

    socket.onclose = () => {
        if (ws !== socket) return;
        updateConnectionUI('disconnected');
        cancelAnimationFrame(animFrame);
        animFrame = 0;
        if (!isIntentionalClose) scheduleReconnect();
    };
}

function disconnect() {
    isIntentionalClose = true;
    if (reconnectTimer) {
        clearTimeout(reconnectTimer);
        reconnectTimer = 0;
    }
    if (ws) {
        try { ws.close(); } catch {}
        ws = null;
    }
}

function scheduleReconnect() {
    if (reconnectTimer) return;
    const delay = Math.min(15000, 1000 * Math.pow(2, reconnectAttempts));
    reconnectAttempts++;
    elStatusText.textContent = `RETRY ${Math.round(delay / 1000)}S`;
    reconnectTimer = window.setTimeout(() => {
        reconnectTimer = 0;
        connect();
    }, delay);
}

/* ============================================================
 * Tauri Backend Configuration Bridging
 * ============================================================ */

function fetchBackendConfig() {
    if (window.__TAURI__) {
        window.__TAURI__.core.invoke('get_app_config')
            .then((/** @type {any} */ cfg) => {
                backConfig = {
                    port: cfg.server_port,
                    offsetMs: cfg.smtc_offset_ms,
                    intervalMs: cfg.update_interval_ms,
                    outputTxt: cfg.output_txt,
                    outputJson: cfg.output_json,
                    outputLyric: cfg.output_lyric
                };
                syncConfigToUI();
            })
            .catch((e) => console.error('Failed to get app config:', e));
    } else {
        // Fallback local mock configurations for normal browser testing
        syncConfigToUI();
        connect();
    }
}

function saveBackendConfig() {
    if (!window.__TAURI__) return;
    const payload = {
        newCfg: {
            server_port: backConfig.port,
            smtc_offset_ms: backConfig.offsetMs,
            update_interval_ms: backConfig.intervalMs,
            output_txt: backConfig.outputTxt,
            output_json: backConfig.outputJson,
            output_lyric: backConfig.outputLyric
        }
    };
    window.__TAURI__.core.invoke('save_app_config', payload)
        .catch((e) => console.error('Failed to save config:', e));
}

/* ============================================================
 * State Synchronization & Layout Generation
 * ============================================================ */

/** @param {number} ms */
function formatMs(ms) {
    if (!Number.isFinite(ms) || ms < 0) ms = 0;
    const totalSecs = Math.floor(ms / 1000);
    const m = String(Math.floor(totalSecs / 60)).padStart(2, '0');
    const s = String(totalSecs % 60).padStart(2, '0');
    return `${m}:${s}`;
}

/** @param {any} data */
function onUpdate(data) {
    const isSongChanged = !songInfo || songInfo.title !== data.title || songInfo.artist !== data.artist;
    
    // 解析当前播放时间
    const totalMs = data.total_time_ms && data.total_time_ms > 0 ? data.total_time_ms : data.total_time * 1000;
    const rawTimeMs = data.current_time_ms && data.current_time_ms > 0 ? data.current_time_ms : (totalMs * (data.progress_percent / 100));

    const oldIsPlaying = songInfo ? songInfo.is_playing : false;
    isPlaying = data.is_playing;
    songInfo = data;

    // Backend now sends drift-corrected positions (via SMTC LastUpdatedTime),
    // so we can always update the baseline. No threshold gate needed since
    // consecutive corrected positions advance monotonically.
    if (isSongChanged) {
        // Clear old lyrics immediately and show loading indicator to prevent old song lyrics from lingering
        lyricLines = [];
        elLyricsViewport.innerHTML = '<div class="lyric-loading">正在加载歌词...</div>';
        lastRenderKey = '';

        // Clear album art immediately on song change to avoid ghosting the previous song
        elVinylCenterArt.style.backgroundImage = '';
        elAlbumBlurBg.style.backgroundImage = '';
        elAlbumBlurBg.classList.remove('active');

        // Proactively clear/zero duration if it seems to belong to the old song
        if (data.total_time_ms > 5000) {
            data.total_time_ms = 0;
            data.total_time = 0;
        }

        // Hard reset on song change — old position is irrelevant.
        // If the first packet has a stale large progress (> 5000ms), force it to 0.
        const freshProgress = rawTimeMs > 5000 ? 0 : rawTimeMs;
        lastRawTimeMs = freshProgress;
        lastSampleProgress = freshProgress;
        lastSampleLocalTime = performance.now();
    } else {
        // Compute where the frontend thinks playback is right now
        const localEstimate = isPlaying && lastSampleLocalTime > 0
            ? lastSampleProgress + (performance.now() - lastSampleLocalTime)
            : lastSampleProgress;
        
        if (isPlaying !== oldIsPlaying) {
            // Play/pause transition: take the larger of server vs local to avoid rewind
            // (SMTC may briefly lag on resume because LastUpdatedTime hasn't caught up)
            lastRawTimeMs = rawTimeMs;
            lastSampleProgress = Math.max(rawTimeMs, localEstimate);
            lastSampleLocalTime = performance.now();
        } else if (Math.abs(rawTimeMs - localEstimate) > 1000) {
            // Force snap on large jumps (e.g. seeking backward/forward, or timeline update after track change)
            lastRawTimeMs = rawTimeMs;
            lastSampleProgress = rawTimeMs;
            lastSampleLocalTime = performance.now();
        } else if (rawTimeMs >= localEstimate - 50) {
            // Normal update: server is roughly at or ahead of our estimate — snap to server
            lastRawTimeMs = rawTimeMs;
            lastSampleProgress = rawTimeMs;
            lastSampleLocalTime = performance.now();
        }
        // else: server is behind our local clock (rare race), ignore this sample
    }

    // 更新左面板歌曲信息与唱机状态
    elTitle.textContent = data.title || '等待播放';
    elArtist.textContent = data.artist || 'QQ音乐监听器';
    elAlbum.textContent = data.album || 'SMTC 模式';
    
    // 更新专辑图片与模糊背景
    if (data.album_pic_url) {
        elVinylCenterArt.style.backgroundImage = `url("${data.album_pic_url}")`;
        const curBg = elAlbumBlurBg.style.backgroundImage;
        const newBg = `url("${data.album_pic_url}")`;
        if (curBg !== newBg) {
            elAlbumBlurBg.style.backgroundImage = newBg;
        }
        elAlbumBlurBg.classList.add('active');
    } else if (!data.title || data.title === "No music playing" || data.title === "ERROR") {
        elVinylCenterArt.style.backgroundImage = '';
        elAlbumBlurBg.style.backgroundImage = '';
        elAlbumBlurBg.classList.remove('active');
    }
    
    if (data.is_playing) {
        elVinyl.classList.add('playing');
        elStylus.classList.add('active');
    } else {
        elVinyl.classList.remove('playing');
        elStylus.classList.remove('active');
    }

    // 重建歌词
    const qrcLength = data.qrc_data ? data.qrc_data.length : 0;
    const renderKey = `${data.title}|${data.artist}|${qrcLength}`;
    if (renderKey !== lastRenderKey || lyricLines.length === 0) {
        lastRenderKey = renderKey;
        transMap = parseTranslation(data.trans);
        buildLyricsArea(data);
        lastActiveIdx = -1;
    }

    // 调试反馈
    if (frontConfig.debug) {
        elRawData.textContent = JSON.stringify(data, null, 2);
        elDebugInfo.textContent = `Offset: ${backConfig.offsetMs}ms | Poll: ${backConfig.intervalMs}ms | Sync: ${lastSyncMs.toFixed(1)}ms | QRC Lines: ${qrcLength}`;
    }
}

/**
 * @param {string} text
 * @returns {any[]}
 */
function parseTranslation(text) {
    if (!text) return [];
    const entries = [];
    const re = /\[(\d{2}):(\d{2})(?:[.:](\d{2,3}))?\](.*)/;
    for (const line of text.split('\n')) {
        const trimmed = line.trim();
        const m = trimmed.match(re);
        if (!m) continue;
        const ms = (parseInt(m[1], 10) * 60 + parseInt(m[2], 10)) * 1000 + (m[3] ? parseInt(m[3].padEnd(3, '0'), 10) : 0);
        const transText = m[4].trim();
        if (transText && transText !== '//') {
            entries.push({ time: ms, text: transText });
        }
    }
    entries.sort((a, b) => a.time - b.time);
    return entries;
}

/** @param {number} startTimeMs */
function findTranslation(startTimeMs) {
    if (transMap.length === 0) return '';
    let bestText = '', minDiff = Infinity;
    for (const entry of transMap) {
        const diff = Math.abs(entry.time - startTimeMs);
        if (diff <= 200 && diff < minDiff) {
            minDiff = diff;
            bestText = entry.text;
        }
    }
    return bestText;
}

/** @param {any} data */
function buildLyricsArea(data) {
    elLyricsViewport.innerHTML = '';
    lyricLines = [];

    // 无逐字歌词兜底
    if (!data.qrc_data || data.qrc_data.length === 0) {
        const div = document.createElement('div');
        div.className = 'lyric-empty';
        div.textContent = data.lyrics ? data.lyrics.split('\n')[0] : '无逐字歌词';
        elLyricsViewport.appendChild(div);
        return;
    }

    // 逐行组装
    for (let idx = 0; idx < data.qrc_data.length; idx++) {
        const line = data.qrc_data[idx];
        const lineDiv = document.createElement('div');
        lineDiv.className = 'lyric-line before';
        
        // 增加首尾居中留白 margin
        if (idx === 0) {
            lineDiv.style.marginTop = '180px';
        }
        if (idx === data.qrc_data.length - 1) {
            lineDiv.style.marginBottom = '180px';
        }

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
                dur: w.duration_ms || 200
            });
        }

        const durEnd = line.duration_ms > 0 ? line.start_time_ms + line.duration_ms : 0;
        const lastWordEnd = line.words.length > 0 ? line.words[line.words.length - 1].start_time_ms + line.words[line.words.length - 1].duration_ms : 0;
        let lineEnd = Math.max(durEnd, lastWordEnd);
        if (lineEnd <= 0) lineEnd = line.start_time_ms + 4000;

        const trans = findTranslation(line.start_time_ms);
        if (trans) {
            const transDiv = document.createElement('div');
            transDiv.className = 'lyric-trans';
            transDiv.textContent = trans;
            if (!frontConfig.showTranslation) transDiv.style.display = 'none';
            lineDiv.appendChild(transDiv);
        }

        elLyricsViewport.appendChild(lineDiv);
        lyricLines.push({
            el: lineDiv,
            words,
            start: line.start_time_ms,
            end: lineEnd,
            state: 'before'
        });
    }
}

/* ============================================================
 * High-FPS Interpolated Render Loop
 * ============================================================ */

function loop() {
    animFrame = requestAnimationFrame(loop);
    if (!songInfo) return;

    // 前进推算插值时间 (60fps)
    let t = lastSampleProgress;
    if (isPlaying && lastSampleLocalTime > 0) {
        const elapsed = performance.now() - lastSampleLocalTime;
        t = lastSampleProgress + elapsed + backConfig.offsetMs;
    } else {
        t = lastSampleProgress + backConfig.offsetMs;
    }
    t = Math.max(0, t);

    // 更新底部播放时间与进度轨道
    const totalMs = (songInfo.total_time_ms && songInfo.total_time_ms > 0) ? songInfo.total_time_ms : songInfo.total_time * 1000;
    const curTimeText = formatMs(t);
    const totalTimeText = formatMs(totalMs);
    if (elTimeCurrent.textContent !== curTimeText) elTimeCurrent.textContent = curTimeText;
    if (elTimeTotal.textContent !== totalTimeText) elTimeTotal.textContent = totalTimeText;

    if (totalMs > 0) {
        const pct = Math.min(100, Math.max(0, (t / totalMs) * 100));
        elProgressFill.style.width = `${pct}%`;
    }

    if (lyricLines.length === 0) return;

    let activeIdx = -1;

    // 刷新各句状态
    for (let i = 0; i < lyricLines.length; i++) {
        const line = lyricLines[i];

        let lineState;
        if (t < line.start) lineState = 'before';
        else if (t >= line.end) lineState = 'after';
        else {
            lineState = 'active';
            activeIdx = i;
        }

        if (line.state !== lineState) {
            line.state = lineState;
            line.el.className = `lyric-line ${lineState}`;
        }

        if (lineState === 'before') {
            for (const w of line.words) {
                if (w.width !== 0) {
                    w.width = 0;
                    w.fgEl.style.width = '0%';
                }
            }
        } else if (lineState === 'after') {
            for (const w of line.words) {
                if (w.width !== 100) {
                    w.width = 100;
                    w.fgEl.style.width = '100%';
                }
            }
        } else {
            // 刷新活跃句中的每个字的剪裁宽度
            for (const w of line.words) {
                const wStart = w.start;
                const wDur = w.dur;
                const wEnd = wStart + wDur;

                let targetWidth = 0;
                if (t >= wEnd) {
                    targetWidth = 100;
                } else if (t >= wStart) {
                    targetWidth = ((t - wStart) / wDur) * 100;
                } else {
                    targetWidth = 0;
                }

                const quantWidth = Math.round(targetWidth * 10) / 10;
                if (quantWidth !== w.width) {
                    w.width = quantWidth;
                    w.fgEl.style.width = `${quantWidth}%`;
                }
            }
        }
    }

    // 丝滑 Transform translateY 居中滚动 (不再改动 scrollTop 造成卡顿)
    if (activeIdx !== lastActiveIdx && activeIdx >= 0) {
        lastActiveIdx = activeIdx;
        const activeLineEl = lyricLines[activeIdx].el;
        const viewportCenter = elLyricsContainer.clientHeight / 2;
        // 计算目标 Y 平移偏移
        const targetScrollY = viewportCenter - activeLineEl.offsetTop - (activeLineEl.clientHeight / 2);
        elLyricsViewport.style.transform = `translateY(${targetScrollY}px)`;
    }
}

/* ============================================================
 * Configuration Synchronizer & UI Actions
 * ============================================================ */

function syncConfigToUI() {
    // Backend configs
    elCfgOffset.value = String(backConfig.offsetMs);
    elValOffset.textContent = `${backConfig.offsetMs}ms`;
    
    elCfgInterval.value = String(backConfig.intervalMs);
    elValInterval.textContent = `${backConfig.intervalMs}ms`;
    
    elCfgPort.value = String(backConfig.port);
    
    elChkOutTxt.checked = backConfig.outputTxt;
    elChkOutJson.checked = backConfig.outputJson;
    elChkOutLyric.checked = backConfig.outputLyric;

    // Frontend configs
    document.documentElement.style.setProperty('--lyric-font-size', `${frontConfig.fontSize}px`);
    document.documentElement.style.setProperty('--lyric-font-weight', `${frontConfig.fontWeight}`);
    document.body.setAttribute('data-theme', frontConfig.theme);
    
    elCfgFontSize.value = String(frontConfig.fontSize);
    elValFontSize.textContent = `${frontConfig.fontSize}px`;
    
    elCfgFontWeight.value = String(frontConfig.fontWeight);
    elValFontWeight.textContent = String(frontConfig.fontWeight);
    
    elToggleTrans.checked = frontConfig.showTranslation;
    elToggleDebug.checked = frontConfig.debug;
    elDebugPanel.hidden = !frontConfig.debug;

    // Theme dots active states
    document.querySelectorAll('.theme-dot').forEach((dot) => {
        const theme = dot.getAttribute('data-theme');
        if (theme === frontConfig.theme) {
            dot.classList.add('active');
        } else {
            dot.classList.remove('active');
        }
    });
}

// Drawer Open / Close
elBtnSettings.addEventListener('click', () => {
    fetchBackendConfig();
    elSettingsOverlay.classList.add('open');
});

elBtnSettingsClose.addEventListener('click', () => {
    elSettingsOverlay.classList.remove('open');
});

elSettingsOverlay.addEventListener('click', (e) => {
    if (e.target === elSettingsOverlay) elSettingsOverlay.classList.remove('open');
});

// Steppers (Backend Settings)
elCfgOffset.addEventListener('input', () => {
    const val = parseInt(elCfgOffset.value, 10);
    backConfig.offsetMs = val;
    elValOffset.textContent = `${val}ms`;
    if (songInfo) onUpdate(songInfo);
});

elCfgOffset.addEventListener('change', () => {
    saveBackendConfig();
});

elBtnOffsetDec.addEventListener('click', () => {
    const val = Math.max(-1000, backConfig.offsetMs - 10);
    backConfig.offsetMs = val;
    elCfgOffset.value = String(val);
    elValOffset.textContent = `${val}ms`;
    saveBackendConfig();
    if (songInfo) onUpdate(songInfo);
});

elBtnOffsetInc.addEventListener('click', () => {
    const val = Math.min(1000, backConfig.offsetMs + 10);
    backConfig.offsetMs = val;
    elCfgOffset.value = String(val);
    elValOffset.textContent = `${val}ms`;
    saveBackendConfig();
    if (songInfo) onUpdate(songInfo);
});

elCfgInterval.addEventListener('input', () => {
    const val = parseInt(elCfgInterval.value, 10);
    elValInterval.textContent = `${val}ms`;
});

elCfgInterval.addEventListener('change', () => {
    const val = parseInt(elCfgInterval.value, 10);
    backConfig.intervalMs = val;
    saveBackendConfig();
});

elBtnApplyPort.addEventListener('click', () => {
    const val = parseInt(elCfgPort.value, 10);
    if (!Number.isInteger(val) || val < 1 || val > 65535) {
        alert('请输入有效的端口号 (1-65535)');
        return;
    }
    backConfig.port = val;
    saveBackendConfig();
    alert('同步端口已保存到 config.toml，请重启程序以启用新的服务端口。');
});

// OBS Output switches (Backend Settings)
elChkOutTxt.addEventListener('change', () => {
    backConfig.outputTxt = elChkOutTxt.checked;
    saveBackendConfig();
});
elChkOutJson.addEventListener('change', () => {
    backConfig.outputJson = elChkOutJson.checked;
    saveBackendConfig();
});
elChkOutLyric.addEventListener('change', () => {
    backConfig.outputLyric = elChkOutLyric.checked;
    saveBackendConfig();
});

// Frontend Settings
elCfgFontSize.addEventListener('input', () => {
    const val = parseInt(elCfgFontSize.value, 10);
    elValFontSize.textContent = `${val}px`;
    document.documentElement.style.setProperty('--lyric-font-size', `${val}px`);
});

elCfgFontSize.addEventListener('change', () => {
    const val = parseInt(elCfgFontSize.value, 10);
    frontConfig.fontSize = val;
    saveFrontendConfig(frontConfig);
});

elCfgFontWeight.addEventListener('input', () => {
    const val = parseInt(elCfgFontWeight.value, 10);
    elValFontWeight.textContent = String(val);
    document.documentElement.style.setProperty('--lyric-font-weight', `${val}`);
});

elCfgFontWeight.addEventListener('change', () => {
    const val = parseInt(elCfgFontWeight.value, 10);
    frontConfig.fontWeight = val;
    saveFrontendConfig(frontConfig);
});

elToggleTrans.addEventListener('change', () => {
    frontConfig.showTranslation = elToggleTrans.checked;
    saveFrontendConfig(frontConfig);
    
    const transDivs = elLyricsViewport.querySelectorAll('.lyric-trans');
    transDivs.forEach((div) => {
        /** @type {HTMLElement} */ (div).style.display = frontConfig.showTranslation ? 'block' : 'none';
    });
});

elToggleDebug.addEventListener('change', () => {
    frontConfig.debug = elToggleDebug.checked;
    saveFrontendConfig(frontConfig);
    elDebugPanel.hidden = !frontConfig.debug;
});

// Themes selector
document.querySelectorAll('.theme-dot').forEach((dot) => {
    dot.addEventListener('click', () => {
        const theme = dot.getAttribute('data-theme') || 'aurora-cyan';
        frontConfig.theme = theme;
        document.body.setAttribute('data-theme', theme);
        saveFrontendConfig(frontConfig);
        
        document.querySelectorAll('.theme-dot').forEach((d) => d.classList.remove('active'));
        dot.classList.add('active');
    });
});

/* ============================================================
 * Throttling & Visibility States
 * ============================================================ */

document.addEventListener('visibilitychange', () => {
    const isHidden = document.visibilityState === 'hidden';

    // 后台运行时通知 Rust 降低轮询至 2s，前台恢复正常频率
    if (window.__TAURI__) {
        window.__TAURI__.core.invoke('set_background_state', { isBackground: isHidden });
    }
});

/* ============================================================
 * Bootstrapping
 * ============================================================ */

syncConfigToUI();
fetchBackendConfig();

if (window.__TAURI__) {
    // Tauri Direct Event listening (zero loopback overhead)
    const { listen } = window.__TAURI__.event;
    listen('song-info', (event) => {
        const t0 = performance.now();
        const data = event.payload;
        if (data) {
            onUpdate(data);
        }
        lastSyncMs = performance.now() - t0;
    });
    updateConnectionUI('connected');
    if (!animFrame) loop();
}

window.addEventListener('beforeunload', () => disconnect());

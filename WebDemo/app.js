const WS_URL = 'ws://127.0.0.1:3000/ws';

const elTitle = document.getElementById('song-title');
const elAlbum = document.getElementById('song-album');
const elStatusIcon = document.getElementById('status-icon');
const elProgressFill = document.getElementById('progress-fill');
const elTimeInfo = document.getElementById('time-info');
const elLyricsArea = document.getElementById('lyrics-area');
const elDebugInfo = document.getElementById('debug-info');
const elRawData = document.getElementById('raw-data');
const elToggleTrans = document.getElementById('toggle-trans');
const elToggleDebug = document.getElementById('toggle-debug');

let ws = null;
let songInfo = null;
let lyricLines = []; // { el, words: [{el, start, end}], start, end }
let transMap = [];
let animFrame = null;
let lastRenderedKey = ''; // 追踪上次渲染的歌词 key: "title|artist|qrc_len"

// Server sync point + SMTC offset compensation (matches TUI smtc_offset_ms)
const SMTC_OFFSET_MS = 250;
let serverTimeMs = 0;
let serverTimestamp = 0;
let simulatedTimeMs = 0;

function formatMs(ms) {
    const s = Math.floor(ms / 1000);
    return `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(s % 60).padStart(2, '0')}`;
}

function connect() {
    ws = new WebSocket(WS_URL);

    ws.onopen = () => {
        console.log('Connected');
        if (!animFrame) loop();
    };

    ws.onmessage = (e) => {
        try {
            const data = JSON.parse(e.data);
            onUpdate(data);
            elRawData.textContent = JSON.stringify(data, null, 2);
        } catch (err) {
            console.error('Parse error:', err);
        }
    };

    ws.onclose = () => {
        console.log('Disconnected, reconnecting...');
        elTitle.textContent = '连接断开, 重连中...';
        cancelAnimationFrame(animFrame);
        animFrame = null;
        setTimeout(connect, 3000);
    };

    ws.onerror = () => ws.close();
}

function onUpdate(data) {
    const songChanged = !songInfo || songInfo.title !== data.title || songInfo.artist !== data.artist;
    songInfo = data;

    // Sync point with SMTC offset compensation (matches TUI logic)
    const rawTimeMs = data.total_time > 0
        ? data.total_time * 1000 * (data.progress_percent / 100)
        : data.current_time * 1000;
    const newServerTimeMs = rawTimeMs + SMTC_OFFSET_MS;

    // Only jump forward, never backward (prevents flickering)
    // 切歌时重置到 rawTimeMs（不加偏移，避免新歌第一行跳过）
    if (songChanged) {
        serverTimeMs = rawTimeMs;
        simulatedTimeMs = rawTimeMs;
    } else if (newServerTimeMs > simulatedTimeMs) {
        serverTimeMs = newServerTimeMs;
        simulatedTimeMs = newServerTimeMs;
    } else {
        serverTimeMs = newServerTimeMs;
    }
    serverTimestamp = performance.now();

    // Meta
    elTitle.textContent = data.title || 'No music playing';
    elAlbum.textContent = data.album ? `专辑:${data.album}` : '';

    // Status icon
    elStatusIcon.textContent = data.is_playing ? '▶' : '▐▐';

    // Progress
    elProgressFill.style.width = `${data.progress_percent}%`;
    elTimeInfo.textContent = `${formatMs(serverTimeMs)} / ${formatMs(data.total_time * 1000)} [${data.progress_percent.toFixed(1)}%]`;

    // Lyrics: 用 title+artist+qrc长度 作为 key，任何变化都重建
    const qrcLen = (data.qrc_data && data.qrc_data.length) || 0;
    const renderKey = `${data.title}|${data.artist}|${qrcLen}`;
    if (renderKey !== lastRenderedKey || lyricLines.length === 0) {
        lastRenderedKey = renderKey;
        transMap = parseTrans(data.trans || '');
        buildLyrics(data);
    }

    // Debug
    if (elToggleDebug.checked && data.qrc_data && data.qrc_data.length > 0) {
        const words = data.qrc_data.reduce((n, l) => n + l.words.length, 0);
        elDebugInfo.textContent = `[QRC] ${qrcLen} lines, ${words} words | raw=${rawTimeMs}ms +offset=${SMTC_OFFSET_MS}ms → display=${Math.round(simulatedTimeMs)}ms`;
        elDebugInfo.classList.add('visible');
    } else {
        elDebugInfo.classList.remove('visible');
    }
}

function parseTrans(text) {
    if (!text) return [];
    const result = [];
    const re = /\[(\d{2}):(\d{2})(?:[.:](\d{2,3}))?\](.*)/;
    for (const line of text.split('\n')) {
        const t = line.trim();
        if (/^\[ti:/i.test(t) || /^\[ar:/i.test(t) || /^\[al:/i.test(t)) continue;
        const m = t.match(re);
        if (m) {
            const ms = (parseInt(m[1]) * 60 + parseInt(m[2])) * 1000 + (m[3] ? parseInt(m[3].padEnd(3, '0')) : 0);
            const content = m[4].trim();
            if (content && content !== '//') result.push({ time: ms, text: content });
        }
    }
    return result.sort((a, b) => a.time - b.time);
}

function findTrans(startTimeMs) {
    if (!transMap.length) return '';
    let best = '', diff = Infinity;
    for (const e of transMap) {
        const d = Math.abs(e.time - startTimeMs);
        if (d <= 100 && d < diff) { diff = d; best = e.text; }
    }
    return best;
}

function buildLyrics(data) {
    elLyricsArea.innerHTML = '';
    lyricLines = [];

    if (!data.qrc_data || data.qrc_data.length === 0) {
        const div = document.createElement('div');
        div.className = 'lyric-empty';
        div.textContent = data.lyrics ? data.lyrics.split('\n')[0] : '...';
        elLyricsArea.appendChild(div);
        return;
    }

    for (const line of data.qrc_data) {
        const lineDiv = document.createElement('div');
        lineDiv.className = 'lyric-line before';

        const words = [];
        for (const w of line.words) {
            const span = document.createElement('span');
            span.className = 'word pending';
            span.textContent = w.content;
            span.dataset.start = w.start_time_ms;
            span.dataset.dur = w.duration_ms;
            lineDiv.appendChild(span);
            words.push(span);
        }

        const lineEnd = line.start_time_ms + line.duration_ms;

        // Translation
        const trans = findTrans(line.start_time_ms);
        if (trans) {
            const transDiv = document.createElement('div');
            transDiv.className = 'lyric-trans';
            transDiv.textContent = trans;
            transDiv.dataset.trans = '1';
            if (!elToggleTrans.checked) transDiv.style.display = 'none';
            lineDiv.appendChild(transDiv);
        }

        elLyricsArea.appendChild(lineDiv);
        lyricLines.push({ el: lineDiv, words, start: line.start_time_ms, end: lineEnd });
    }
}

// Toggle translation
elToggleTrans.addEventListener('change', () => {
    const show = elToggleTrans.checked;
    elLyricsArea.querySelectorAll('.lyric-trans').forEach(el => {
        el.style.display = show ? '' : 'none';
    });
});

// Toggle debug
elToggleDebug.addEventListener('change', () => {
    if (!elToggleDebug.checked) elDebugInfo.classList.remove('visible');
});

// Main render loop - matches TUI logic exactly
function loop(timestamp) {
    animFrame = requestAnimationFrame(loop);

    if (!songInfo || lyricLines.length === 0) return;

    // Interpolation: advance from sync point, but never go backward
    if (songInfo.is_playing && serverTimestamp > 0) {
        const elapsed = performance.now() - serverTimestamp;
        const interpolated = serverTimeMs + elapsed;
        if (interpolated > simulatedTimeMs) {
            simulatedTimeMs = interpolated;
        }
    }
    const t = simulatedTimeMs;

    // Update time display smoothly
    elTimeInfo.textContent = `${formatMs(t)} / ${formatMs(songInfo.total_time * 1000)} [${songInfo.total_time > 0 ? (t / (songInfo.total_time * 1000) * 100).toFixed(1) : '0.0'}%]`;

    let activeIdx = -1;

    for (let i = 0; i < lyricLines.length; i++) {
        const line = lyricLines[i];

        if (t < line.start) {
            // Before this line: grey
            line.el.className = 'lyric-line before';
            for (const w of line.words) {
                w.className = 'word pending';
                w.style.removeProperty('--progress');
            }
        } else if (t >= line.end) {
            // After this line: all yellow (sung)
            line.el.className = 'lyric-line after';
            for (const w of line.words) {
                w.className = 'word sung';
                w.style.removeProperty('--progress');
            }
        } else {
            // Active line: per-word rendering (TUI logic)
            activeIdx = i;
            line.el.className = 'lyric-line active';

            for (const w of line.words) {
                const wStart = parseInt(w.dataset.start);
                const wDur = parseInt(w.dataset.dur) || 200;
                const wEnd = wStart + wDur;

                if (t >= wEnd) {
                    // Word finished: solid yellow
                    w.className = 'word sung';
                    w.style.removeProperty('--progress');
                } else if (t >= wStart) {
                    // Word being sung: KTV sweep
                    const progress = ((t - wStart) / wDur) * 100;
                    w.className = 'word singing';
                    w.style.setProperty('--progress', `${progress}%`);
                } else {
                    // Word pending: grey
                    w.className = 'word pending';
                    w.style.removeProperty('--progress');
                }
            }
        }
    }

    // Scroll active line to center
    if (activeIdx >= 0) {
        const el = lyricLines[activeIdx].el;
        const containerCenter = elLyricsArea.clientHeight / 2;
        const target = el.offsetTop - elLyricsArea.offsetTop - containerCenter + el.clientHeight / 2;
        elLyricsArea.scrollTop = target;
    }
}

connect();

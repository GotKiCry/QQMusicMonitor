const WS_URL = 'ws://127.0.0.1:3000/ws';

// DOM Elements
const elTitle = document.getElementById('song-title');
const elArtist = document.getElementById('song-artist');
const elAlbum = document.getElementById('song-album');
const elLyricsContainer = document.getElementById('lyrics-container');
const elCurrentTime = document.getElementById('current-time');
const elTotalTime = document.getElementById('total-time');
const elProgressBar = document.getElementById('progress-bar-fill');
const elRawData = document.getElementById('raw-data-view');
const elToggleTrans = document.getElementById('toggle-trans');

// Parsed translation map: startTimeMs -> translationText
let transMap = [];

let ws = null;
let currentSongInfo = null;
let reconnectTimeout = null;
let animationFrameId = null;

// Helper: Format MS to MM:SS
function formatTimeMs(ms) {
    const totalSeconds = Math.floor(ms / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}`;
}

// Connect to WebSocket Server
function connect() {
    console.log('Connecting to', WS_URL);
    ws = new WebSocket(WS_URL);

    ws.onopen = () => {
        console.log('Connected to QQMusicMonitor WebSocket!');
        elTitle.textContent = "已连接等待数据...";
        if (reconnectTimeout) clearTimeout(reconnectTimeout);
        // Start UI update loop
        if (!animationFrameId) {
            updateLoop();
        }
    };

    ws.onmessage = (event) => {
        try {
            const data = JSON.parse(event.data);
            handleDataUpdate(data);

            // Render raw data to the left panel
            elRawData.textContent = JSON.stringify(data, null, 2);
        } catch (e) {
            console.error('Failed to parse WebSocket message:', e);
        }
    };

    ws.onclose = () => {
        console.log('Disconnected. Reconnecting in 3s...');
        elTitle.textContent = "连接断开, 重连中...";
        cancelAnimationFrame(animationFrameId);
        animationFrameId = null;
        reconnectTimeout = setTimeout(connect, 3000);
    };

    ws.onerror = (err) => {
        console.error('WebSocket Error:', err);
        ws.close();
    };
}

// Global precise time based on SMTC progress percentage
let preciseTimeMs = 0;
// Timestamp (performance.now) of the last WS message, used for interpolation
let lastSyncTimestamp = 0;

function handleDataUpdate(newData) {
    const isSongChanged = !currentSongInfo ||
        currentSongInfo.title !== newData.title ||
        currentSongInfo.artist !== newData.artist;

    currentSongInfo = newData;

    // Update calculated precise time from the server's authoritative data
    preciseTimeMs = newData.total_time * 1000 * (newData.progress_percent / 100);
    // Record when we received this sync point
    lastSyncTimestamp = performance.now();
    // Snap simulated time to the server's truth
    simulatedTimeMs = preciseTimeMs;

    // Update Meta
    elTitle.textContent = newData.title || 'No music playing';
    if (!newData.is_playing && newData.title) {
        elTitle.textContent = `[已暂停] ` + elTitle.textContent;
    }
    elArtist.textContent = newData.artist || '-';
    elAlbum.textContent = newData.album || '-';

    // Update Progress text & bar
    elCurrentTime.textContent = formatTimeMs(preciseTimeMs);
    elTotalTime.textContent = formatTimeMs(newData.total_time * 1000);
    elProgressBar.style.width = `${newData.progress_percent}%`;

    // Process Lyrics if song changed or no lyrics rendered yet
    if (isSongChanged || elLyricsContainer.children.length <= 1) {
        transMap = parseLrcTrans(newData.trans || '');
        renderLyrics(newData);
    }
}

// Helper: Parse LRC translation text into sorted array of {time, text}
function parseLrcTrans(lrcText) {
    if (!lrcText) return [];
    const result = [];
    const regex = /\[(\d{2}):(\d{2})(?:[.:](\d{2,3}))?\](.*)/;
    // Skip LRC metadata tags
    const metaTags = ['ti:', 'ar:', 'al:', 'by:', 'offset:'];
    lrcText.split('\n').forEach(line => {
        const trimmed = line.trim();
        // Skip metadata lines like [ti:xxx]
        if (metaTags.some(tag => trimmed.toLowerCase().startsWith('[' + tag))) return;

        const match = trimmed.match(regex);
        if (match) {
            const minutes = parseInt(match[1], 10);
            const seconds = parseInt(match[2], 10);
            const millis = match[3] ? parseInt(match[3].padEnd(3, '0'), 10) : 0;
            const timeMs = (minutes * 60 + seconds) * 1000 + millis;
            let text = match[4].trim();
            // Filter out "//" placeholder (QRC empty line marker) and empty lines
            if (!text || text === '//' || text === '//\r') return;
            result.push({ time: timeMs, text });
        }
    });
    result.sort((a, b) => a.time - b.time);
    return result;
}

// Helper: Find translation text for a given startTimeMs (nearest-neighbor match)
function findTransForTime(startTimeMs) {
    if (transMap.length === 0) return '';
    // Allow ±100ms tolerance, pick the closest match
    const tolerance = 100;
    let bestMatch = '';
    let bestDiff = Infinity;
    for (const entry of transMap) {
        const diff = Math.abs(entry.time - startTimeMs);
        if (diff <= tolerance && diff < bestDiff) {
            bestDiff = diff;
            bestMatch = entry.text;
        }
    }
    return bestMatch;
}

// Render the initial lyric DOM structure
let lyricElements = [];

function renderLyrics(data) {
    elLyricsContainer.innerHTML = '';
    lyricElements = [];

    if (!data.qrc_data || data.qrc_data.length === 0) {
        // Fallback for normal LRC or no lyrics
        const div = document.createElement('div');
        div.className = 'lyric-line active';
        div.textContent = data.lyrics ? data.lyrics.split('\n')[0] : '暂无逐字歌词';
        elLyricsContainer.appendChild(div);
        return;
    }

    // Render QRC lines
    data.qrc_data.forEach((line, lineIndex) => {
        const lineDiv = document.createElement('div');
        lineDiv.className = 'lyric-line';
        lineDiv.dataset.start = line.start_time_ms;
        lineDiv.dataset.end = line.start_time_ms + line.duration_ms;

        // Create word spans
        let wordSpans = [];
        line.words.forEach(word => {
            const wordSpan = document.createElement('span');
            wordSpan.className = 'qrc-word';
            wordSpan.dataset.text = word.content;
            wordSpan.textContent = word.content;
            // Native format gives absolute start_time_ms.
            wordSpan.dataset.start = word.start_time_ms;
            wordSpan.dataset.duration = word.duration_ms;

            lineDiv.appendChild(wordSpan);
            wordSpans.push(wordSpan);
        });

        // Append translation line if available
        const transText = findTransForTime(line.start_time_ms);
        if (transText) {
            const transSpan = document.createElement('span');
            transSpan.className = 'lyric-trans';
            transSpan.textContent = transText;
            if (!elToggleTrans.checked) {
                transSpan.style.display = 'none';
            }
            lineDiv.appendChild(transSpan);
        }

        elLyricsContainer.appendChild(lineDiv);
        lyricElements.push({
            el: lineDiv,
            start: line.start_time_ms,
            end: line.start_time_ms + line.duration_ms,
            words: wordSpans
        });
    });
}

// Toggle translation visibility
elToggleTrans.addEventListener('change', () => {
    const show = elToggleTrans.checked;
    document.querySelectorAll('.lyric-trans').forEach(el => {
        el.style.display = show ? '' : 'none';
    });
});

// Update Loop (Runs every frame) for silky smooth transitions
let simulatedTimeMs = 0;

function updateLoop(timestamp) {
    if (!currentSongInfo || lyricElements.length === 0) {
        animationFrameId = requestAnimationFrame(updateLoop);
        return;
    }

    // Core interpolation: server gives us a sync point (preciseTimeMs at lastSyncTimestamp).
    // Between syncs, we linearly extrapolate forward in real-time if the song is playing.
    if (currentSongInfo.is_playing && lastSyncTimestamp > 0) {
        const elapsed = timestamp - lastSyncTimestamp;
        simulatedTimeMs = preciseTimeMs + elapsed;
    }
    // If paused, simulatedTimeMs stays snapped to preciseTimeMs (set in handleDataUpdate)

    let renderTimeMs = simulatedTimeMs;

    let activeIndex = -1;

    // 1. Highlight lines & auto-scroll
    for (let i = 0; i < lyricElements.length; i++) {
        const line = lyricElements[i];

        if (renderTimeMs >= line.start && renderTimeMs <= line.end) {
            activeIndex = i;
            line.el.classList.add('active');

            // 2. Highlight words
            line.words.forEach(wordSpan => {
                const wStart = parseInt(wordSpan.dataset.start);
                const wDur = parseInt(wordSpan.dataset.duration);
                const wEnd = wStart + wDur;

                if (renderTimeMs >= wEnd) {
                    // Word finished
                    wordSpan.style.setProperty('--progress', '100%');
                } else if (renderTimeMs >= wStart && renderTimeMs < wEnd) {
                    // Word playing
                    // 增加缓动，使文字扫过更平滑
                    const progress = ((renderTimeMs - wStart) / wDur) * 100;
                    wordSpan.style.setProperty('--progress', `${progress}%`);
                } else {
                    // Word not started
                    wordSpan.style.setProperty('--progress', '0%');
                }
            });

        } else if (renderTimeMs > line.end) {
            line.el.classList.remove('active');
            // Ensure finished words are full
            line.words.forEach(w => w.style.setProperty('--progress', '100%'));
        } else {
            line.el.classList.remove('active');
            // Ensure upcoming words are empty
            line.words.forEach(w => w.style.setProperty('--progress', '0%'));
        }
    }

    // Scroll active line to center
    if (activeIndex !== -1) {
        const activeEl = lyricElements[activeIndex].el;
        const containerCenter = elLyricsContainer.clientHeight / 2;
        const targetScrollTop = activeEl.offsetTop - elLyricsContainer.offsetTop - containerCenter + (activeEl.clientHeight / 2);

        // Smooth scroll fallback logic
        elLyricsContainer.scrollTop = targetScrollTop;
    }

    animationFrameId = requestAnimationFrame(updateLoop);
}

// Start
connect();

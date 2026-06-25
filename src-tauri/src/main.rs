#![allow(dead_code)]
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tokio::sync::RwLock;

static IS_BACKGROUND: AtomicBool = AtomicBool::new(false);
static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();
static CACHED_CACHE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FrontConfig {
    pub server_port: u16,
    pub smtc_offset_ms: u64,
    pub update_interval_ms: u64,
    pub output_txt: bool,
    pub output_json: bool,
    pub output_lyric: bool,
}

#[tauri::command]
// Function to set background state.
fn set_background_state(is_background: bool) {
    IS_BACKGROUND.store(is_background, Ordering::SeqCst);
}

#[tauri::command]
// Function to get current configuration asynchronously.
async fn get_app_config() -> Result<FrontConfig, String> {
    let cfg = CONFIG.get().ok_or("Config not initialized")?;
    let settings = {
        let guard = cfg.read().await;
        guard.settings.clone()
    };
    Ok(FrontConfig {
        server_port: settings.server_port,
        smtc_offset_ms: settings.smtc_offset_ms,
        update_interval_ms: settings.update_interval_ms,
        output_txt: settings.output_txt,
        output_json: settings.output_json,
        output_lyric: settings.output_lyric,
    })
}

#[tauri::command]
// Function to save configuration asynchronously.
async fn save_app_config(new_cfg: FrontConfig) -> Result<(), String> {
    let cfg_lock = CONFIG.get().ok_or("Config not initialized")?;
    
    // 1. Update memory config
    {
        let mut guard = cfg_lock.write().await;
        guard.settings.server_port = new_cfg.server_port;
        guard.settings.smtc_offset_ms = new_cfg.smtc_offset_ms;
        guard.settings.update_interval_ms = new_cfg.update_interval_ms;
        guard.settings.output_txt = new_cfg.output_txt;
        guard.settings.output_json = new_cfg.output_json;
        guard.settings.output_lyric = new_cfg.output_lyric;
    }
    
    // 2. Write config.toml
    let toml_to_write = {
        let guard = cfg_lock.read().await;
        toml::to_string_pretty(&*guard)
            .map_err(|e| format!("Failed to serialize config: {}", e))?
    };
    
    tokio::fs::write("config.toml", toml_to_write)
        .await
        .map_err(|e| format!("Failed to write config.toml: {}", e))?;
        
    Ok(())
}
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use widestring::U16String;

mod cli;
mod config;
mod smtc;
mod lyrics;
mod song_info;
mod qrc;
mod local_qrc; // Enable local QRC cache module
mod server;

use cli::Cli;
use config::Config;
use song_info::{SongInfo, QrcLine};
use lyrics::LyricFetcher;

use std::collections::HashMap;

/// 当前 Unix 毫秒时间戳，用于 LRU 淘汰
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 缓存淘汰上限：超出后淘汰最久未访问的条目。
/// 每条含歌词文本 + QRC 逐字数据，按平均 8KB/条估算，128 条约 1MB 内存。
const LYRICS_CACHE_MAX_ENTRIES: usize = 128;

struct LyricsCacheEntry {
    lyrics: String,
    trans: String,
    qrc_raw: String,
    qrc_data: Vec<QrcLine>,
    album_pic_url: String,
    /// 最后访问时间戳（毫秒），用于 LRU 淘汰
    last_accessed: u64,
}

/// 后台歌词缓存 — 由主循环读取、后台任务写入 (使用 HashMap 支持多歌曲缓存，防止切歌竞态)
/// 超过 LYRICS_CACHE_MAX_ENTRIES 时淘汰最久未访问的条目（近似 LRU）。
struct LyricsCache {
    entries: HashMap<String, LyricsCacheEntry>,
}

impl LyricsCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// 检查歌曲是否已缓存
    fn has_song(&self, title: &str, artist: &str) -> bool {
        let key = format!("{}|{}", title, artist);
        self.entries.contains_key(&key)
    }

    fn get_entry(&mut self, title: &str, artist: &str) -> Option<&LyricsCacheEntry> {
        let key = format!("{}|{}", title, artist);
        let now = current_timestamp_ms();
        let entry = self.entries.get_mut(&key)?;
        entry.last_accessed = now;
        Some(entry)
    }

    fn insert_entry(&mut self, title: &str, artist: &str, mut entry: LyricsCacheEntry) {
        let key = format!("{}|{}", title, artist);
        entry.last_accessed = current_timestamp_ms();

        if self.entries.len() >= LYRICS_CACHE_MAX_ENTRIES && !self.entries.contains_key(&key) {
            self.evict_oldest();
        }

        self.entries.insert(key, entry);
    }

    /// 部分更新：仅覆盖缓存中某首歌的 album_pic_url
    fn update_album_pic_url(&mut self, title: &str, artist: &str, url: String) {
        let key = format!("{}|{}", title, artist);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.album_pic_url = url;
            entry.last_accessed = current_timestamp_ms();
        }
    }

    /// 淘汰 last_accessed 最小的条目
    fn evict_oldest(&mut self) {
        if let Some((oldest_key, _)) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_accessed)
            .map(|(k, v)| (k.clone(), v.last_accessed))
        {
            self.entries.remove(&oldest_key);
        }
    }
}

/// 缓存 QQ 音乐本地缓存根目录，只探测一次避免每帧扫盘
fn get_cache_root() -> Option<PathBuf> {
    CACHED_CACHE_ROOT.get_or_init(|| local_qrc::auto_detect_cache_root()).clone()
}

/// 获取歌词缓存目录（QQMusicLyricNew）
fn get_lyric_cache_dir() -> Option<PathBuf> {
    get_cache_root().map(|root| root.join("QQMusicLyricNew"))
}

/// 获取封面图缓存目录（QQMusicPicture）
fn get_picture_cache_dir() -> Option<PathBuf> {
    get_cache_root().map(|root| root.join("QQMusicPicture"))
}

/// 清洗专辑名：去掉中英文括号内容及前后空格，用于 SmartBox 模糊匹配。
/// 例：`ONE PIECE SUPER BEST (海贼王10周年 SUPER BEST)` → `ONE PIECE SUPER BEST`
fn clean_album_name(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(start) = s.find('(') {
        if let Some(end) = s[start..].find(')') {
            s.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }
    while let Some(start) = s.find('（') {
        if let Some(end) = s[start..].find('）') {
            let end_byte = start + end + '）'.len_utf8();
            s.replace_range(start..end_byte, "");
        } else {
            break;
        }
    }
    s.trim().to_string()
}

/// 优先使用本地缓存的专辑封面图，找不到则返回原始在线 URL。
///
/// 本地封面文件名含 album_mid，可从在线 URL 中提取 mid 后在 QQMusicPicture 目录查找。
/// 找到则读取文件转为 base64 data URI（与 SMTC 缩略图格式一致，前端可直接加载）。
///
/// 当 API 返回的 album_mid 在本地没找到时（如单曲版 vs 合辑版不一致），
/// 用 SMTC 报告的专辑名通过 SmartBox 搜索正确的 album_mid，再查本地。
async fn resolve_album_pic_url(online_url: &str, album_name: &str, fetcher: &LyricFetcher) -> String {
    if online_url.is_empty() {
        return String::new();
    }

    // 只处理 QQ 音乐 CDN 的在线 URL（data:image base64 是 SMTC 缩略图，不替换）
    if !online_url.contains("y.gtimg.cn") {
        return online_url.to_string();
    }

    let Some(pic_dir) = get_picture_cache_dir() else {
        return online_url.to_string();
    };

    // 第一次尝试：用在线 URL 中的 album_mid 查本地
    if let Some(album_mid) = local_qrc::extract_album_mid_from_url(online_url) {
        if let Some(local_path) = local_qrc::find_album_pic(&pic_dir, &album_mid) {
            if let Ok(data) = std::fs::read(&local_path) {
                use base64::{Engine as _, engine::general_purpose::STANDARD};
                return format!("data:image/jpeg;base64,{}", STANDARD.encode(&data));
            }
        }
    }

    // 第二次尝试：用 SMTC 专辑名搜 SmartBox，拿到用户实际播放专辑的 album_mid。
    // 尝试原始名和去括号清洗后的变体，处理 SMTC 返回"ONE PIECE SUPER BEST (海贼王…)" 但 SmartBox 只索引"ONE PIECE SUPER BEST"的情况。
    if !album_name.is_empty() {
        let mut variants: Vec<&str> = vec![album_name];
        let cleaned = clean_album_name(album_name);
        if cleaned != album_name {
            variants.push(&cleaned);
        }
        for variant in &variants {
            if let Some(correct_mid) = fetcher.search_album_mid_by_name(variant).await {
                if let Some(local_path) = local_qrc::find_album_pic(&pic_dir, &correct_mid) {
                    if let Ok(data) = std::fs::read(&local_path) {
                        use base64::{Engine as _, engine::general_purpose::STANDARD};
                        return format!("data:image/jpeg;base64,{}", STANDARD.encode(&data));
                    }
                }
            }
        }
    }

    online_url.to_string()
}

/// 从本地 QQ 音乐缓存目录查找歌词（QRC 优先，LRC 兜底），返回完整的缓存条目。
/// 此函数包含文件 I/O 与 DES 解密，仅应在 spawn_blocking 中调用。
fn lookup_local_lyrics(title: &str, artist: &str) -> LyricsCacheEntry {
    let mut entry = LyricsCacheEntry {
        lyrics: String::new(),
        trans: String::new(),
        qrc_raw: String::new(),
        qrc_data: Vec::new(),
        album_pic_url: String::new(),
        last_accessed: 0,
    };

    let Some(cache_dir) = get_lyric_cache_dir() else {
        return entry;
    };

    // QRC 优先（逐字歌词）
    if let Some(qrc_file) = local_qrc::find_qrc_file(&cache_dir, title, artist) {
        if let Ok(xml) = qrc::decode_qrc_from_file(&qrc_file) {
            entry.qrc_raw = "[local]".to_string();
            if let Ok(lines) = qrc::parse_qrc_xml(&xml) {
                entry.qrc_data = lines;
            }
            if entry.lyrics.is_empty() {
                entry.lyrics = qrc::extract_lrc_from_xml(&xml).unwrap_or_default();
            }
            if let Some(trans_file) = local_qrc::find_qrc_trans_file(&qrc_file) {
                if let Ok(trans_xml) = qrc::decode_qrc_from_file(&trans_file) {
                    entry.trans = trans_xml;
                }
            }
        }
    }

    // LRC 兜底（普通歌词，无逐字）
    if entry.lyrics.is_empty() {
        if let Some(lrc_file) = local_qrc::find_lrc_file(&cache_dir, title, artist) {
            let lrc_raw = match qrc::decode_qrc_from_file(&lrc_file) {
                Ok(decrypted) => decrypted,
                Err(_) => std::fs::read_to_string(&lrc_file).unwrap_or_default(),
            };
            entry.lyrics = qrc::extract_lrc_from_xml(&lrc_raw).unwrap_or(lrc_raw);
            if let Some(trans_lrc_file) = local_qrc::find_lrc_trans_file(&lrc_file) {
                let trans_raw = match qrc::decode_qrc_from_file(&trans_lrc_file) {
                    Ok(decrypted) => decrypted,
                    Err(_) => std::fs::read_to_string(&trans_lrc_file).unwrap_or_default(),
                };
                entry.trans = qrc::extract_lrc_from_xml(&trans_raw).unwrap_or(trans_raw);
            }
        }
    }

    entry
}

// Function to run the main monitor loop in a background thread.
async fn run_monitor(app_handle: Option<tauri::AppHandle>, args: Cli, config: Config) -> Result<()> {

    // 设置Ctrl+C处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;
    
    if !args.quiet && config.settings.debug_mode {
        println!("🎵 QQMusic Reader v{} (SMTC Mode)", env!("CARGO_PKG_VERSION"));
        println!("========================================");
        println!("⚠️  正在使用 Windows 系统媒体控制接口");
        println!("========================================\n");
    }

    // 初始化歌词获取器和后台缓存
    let lyric_fetcher = Arc::new(LyricFetcher::with_debug(config.settings.debug_mode));
    let lyrics_cache = Arc::new(RwLock::new(LyricsCache::new()));

    // 初始化数据广播通道并启动服务
    let (tx, rx) = tokio::sync::watch::channel(SongInfo::default());
    if config.settings.enable_server {
        let port = config.settings.server_port;
        if config.settings.debug_mode {
            // 在 TUI 接管前打印服务器信息，避免与渲染竞态
            println!("🚀 本地同步服务已启动: http://127.0.0.1:{}", port);
            println!("📡 WebSocket 接口: ws://127.0.0.1:{}/ws", port);
            println!("📄 当前状态接口: http://127.0.0.1:{}/api/current", port);
        }
        tokio::spawn(async move {
            server::start_server(port, rx).await;
        });
    }





    let mut last_song_info: Option<SongInfo> = None;
    let mut update_count = 0;
    // 切歌后追踪旧歌 total_time_ms，用于跨帧检测 SMTC timeline 是否仍报旧值
    let mut stale_song_total: Option<u64> = None;

    // 主循环
    while running.load(Ordering::SeqCst) {
        let loop_start = tokio::time::Instant::now();
        // Read active config dynamically in each loop iteration
        let config = {
            if let Some(cfg_lock) = CONFIG.get() {
                cfg_lock.read().await.clone()
            } else {
                config.clone()
            }
        };
        let loop_result: Result<()> = async {
            // 使用 SMTC 读取媒体信息（含会话源过滤：只接受 QQ Music，过滤其他音源）
            let mut current_song_info = match smtc::get_current_media_info().await {
            Ok(info) => {
                if let Some(mut info) = info {
                    // 后台歌词加载：切歌时后台请求，不阻塞 TUI
                    let cached_has_song = lyrics_cache.read().await.has_song(&info.title, &info.artist);

                    if !cached_has_song {
                        let quiet = args.quiet;
                        let debug = config.settings.debug_mode;
                        if debug { eprintln!("[lyrics] Fetching lyrics for '{} - {}' in background", info.title, info.artist); }
                        let cache = lyrics_cache.clone();
                        let fetcher = lyric_fetcher.clone();
                        let t = info.title.clone();
                        let a = info.artist.clone();
                        let album = info.album.clone();
                        tokio::spawn(async move {
                            // 1. 本地歌词快速查找并立即插入缓存（~50ms，不阻塞在线获取）
                            {
                                let t2 = t.clone();
                                let a2 = a.clone();
                                let local = tokio::task::spawn_blocking(move || lookup_local_lyrics(&t2, &a2))
                                    .await
                                    .unwrap_or_else(|_| LyricsCacheEntry {
                                        lyrics: String::new(), trans: String::new(),
                                        qrc_raw: String::new(), qrc_data: Vec::new(),
                                        album_pic_url: String::new(),
                                        last_accessed: 0,
                                    });
                                if !local.lyrics.is_empty() || !local.qrc_data.is_empty() {
                                    let mut w = cache.write().await;
                                    w.insert_entry(&t, &a, local);
                                    if debug { eprintln!("[lyrics] Local cache populated for '{} - {}'", t, a); }
                                }
                            }

                            // 2. 在线歌词获取（网络 API，1-3s），完成后覆盖本地数据
                            match fetcher.fetch_lyrics(&t, &a).await {
                                Ok((l, trans, q, pic_url)) => {
                                    let resolved_pic_url = resolve_album_pic_url(&pic_url, &album, &fetcher).await;
                                    let mut entry = LyricsCacheEntry {
                                        lyrics: l,
                                        trans,
                                        qrc_raw: q,
                                        qrc_data: Vec::new(),
                                        album_pic_url: resolved_pic_url,
                                        last_accessed: 0,
                                    };

                                    // 在线 QRC 解密
                                    if !entry.qrc_raw.is_empty() {
                                        if debug { eprintln!("[QRC] Raw data: {} bytes", entry.qrc_raw.len()); }
                                        match qrc::decode_qrc(&entry.qrc_raw) {
                                            Ok(xml) => {
                                                if debug { eprintln!("[QRC] Decrypted XML: {} bytes", xml.len()); }
                                                match qrc::parse_qrc_xml(&xml) {
                                                    Ok(lines) => {
                                                        if debug { eprintln!("[QRC] Parsed {} lines from XML", lines.len()); }
                                                        entry.qrc_data = lines;
                                                    }
                                                    Err(err) => {
                                                        if !quiet { eprintln!("[QRC] XML parse failed for '{} - {}': {}", t, a, err); }
                                                    }
                                                }
                                                if entry.lyrics.is_empty() {
                                                    entry.lyrics = qrc::extract_lrc_from_xml(&xml).unwrap_or_default();
                                                    if debug && !entry.lyrics.is_empty() { eprintln!("[QRC] Extracted LRC from XML: {} chars", entry.lyrics.len()); }
                                                }
                                                if entry.qrc_data.is_empty() && !entry.lyrics.is_empty() {
                                                    let parsed = qrc::parse_qrc_text(&entry.lyrics);
                                                    if !parsed.is_empty() {
                                                        if debug { eprintln!("[QRC] Text fallback parsed {} lines", parsed.len()); }
                                                        entry.qrc_data = parsed;
                                                    }
                                                }
                                            },
                                            Err(err) => {
                                                if !quiet { eprintln!("[QRC] Decode failed for '{} - {}': {}", t, a, err); }
                                            }
                                        }
                                    }

                                    // 在线数据未覆盖的字段回填本地缓存
                                    if entry.lyrics.is_empty() || entry.qrc_data.is_empty() || entry.trans.is_empty() {
                                        let mut cached = cache.write().await;
                                        if let Some(cur) = cached.get_entry(&t, &a) {
                                            if entry.lyrics.is_empty() { entry.lyrics = cur.lyrics.clone(); }
                                            if entry.qrc_data.is_empty() { entry.qrc_data = cur.qrc_data.clone(); }
                                            if entry.trans.is_empty() { entry.trans = cur.trans.clone(); }
                                        }
                                    }

                                    let mut w = cache.write().await;
                                    w.insert_entry(&t, &a, entry);
                                    if debug { eprintln!("[lyrics] ✓ Background fetch complete for '{} - {}'", t, a); }
                                }
                                Err(e) => {
                                    if !quiet { eprintln!("[lyrics] Background fetch failed for '{} - {}': {}", t, a, e); }
                                    // 本地数据已在步骤 1 插入，无需额外处理
                                }
                            }
                        });
                    }

                    // 从缓存中获取歌词 + QRC 数据
                    {
                        let mut cached = lyrics_cache.write().await;
                        if let Some(entry) = cached.get_entry(&info.title, &info.artist) {
                            info.lyrics = entry.lyrics.clone();
                            info.trans = entry.trans.clone();
                            info.qrc_raw = entry.qrc_raw.clone();
                            info.qrc_data = entry.qrc_data.clone();
                            if !entry.album_pic_url.is_empty() {
                                info.album_pic_url = entry.album_pic_url.clone();
                            }
                        }
                    }

                    // 即时本地封面重解析：若缓存的 album_pic_url 是 QQ 音乐在线 URL，
                    // 尝试找本地缓存文件替代。对已缓存歌曲避免永远使用旧 URL。
                    // resolve_album_pic_url 分两步：URL 中的 album_mid 查本地 → SmartBox 用专辑名查。
                    if info.album_pic_url.contains("y.gtimg.cn") {
                        let resolved = resolve_album_pic_url(&info.album_pic_url, &info.album, &lyric_fetcher).await;
                        if resolved != info.album_pic_url {
                            let mut w = lyrics_cache.write().await;
                            w.update_album_pic_url(&info.title, &info.artist, resolved.clone());
                            info.album_pic_url = resolved;
                        }
                    }

                    if info.lyrics.is_empty() && !info.trans.is_empty() {
                        info.lyrics = info.trans.clone();
                    }

                    info
                } else {
                    SongInfo {
                        title: "No music playing".to_string(),
                        artist: String::new(),
                        album: String::new(),
                        lyrics: String::new(),
                        trans: String::new(),
                        qrc_raw: String::new(),
                        qrc_data: Vec::new(),
                        current_time: 0,
                        total_time: 0,
                        current_time_ms: 0,
                        total_time_ms: 0,
                        progress_percent: 0.0,
                        is_playing: false,
                        album_pic_url: String::new(),
                        server_ts: 0,
                    }
                }
            },
            Err(e) => {
                if !args.quiet {
                    eprintln!("Error reading SMTC info: {}", e);
                }
                SongInfo {
                    title: "ERROR".to_string(),
                    artist: String::new(),
                    album: String::new(),
                    lyrics: format!("System Media Control Error: {}", e),
                    trans: String::new(),
                    qrc_raw: String::new(),
                    qrc_data: Vec::new(),
                    current_time: 0,
                    total_time: 0,
                    current_time_ms: 0,
                    total_time_ms: 0,
                    progress_percent: 0.0,
                    is_playing: false,
                    album_pic_url: String::new(),
                    server_ts: 0,
                }
            }
        };

        // 检查歌曲是否有变化 (用于切歌修正与文件输出)
        let song_changed = match &last_song_info {
            Some(last) => last.title != current_song_info.title || last.artist != current_song_info.artist,
            None => true,
        };

        // SMTC 切歌瞬间修正：media properties（title/artist）先更新，
        // timeline（Position/EndTime）可能滞后 1~2 秒仍报旧歌数据。
        //
        // 只靠 song_changed 一帧归零不够：后续帧 title 未变但 timeline 仍旧，
        // 前端会收到"新歌名 + 旧进度"错配——旧歌词继续渲染，进度来回跳。
        //
        // 解法：用 last_song_info.total_time_ms 做锚点——若当前 total 与旧歌
        // 相同（±500ms），判定 timeline 未更新，强制归零。total 变化后清除追踪。
        if song_changed {
            if let Some(old) = &last_song_info {
                if old.total_time_ms > 0 {
                    stale_song_total = Some(old.total_time_ms);
                }
            }
            current_song_info.current_time_ms = 0;
            current_song_info.current_time = 0;
            current_song_info.progress_percent = 0.0;
            current_song_info.total_time_ms = 0;
            current_song_info.total_time = 0;
        } else if let Some(old_total) = stale_song_total {
            if current_song_info.total_time_ms == 0
                || current_song_info.total_time_ms == old_total
            {
                current_song_info.current_time_ms = 0;
                current_song_info.current_time = 0;
                current_song_info.progress_percent = 0.0;
                current_song_info.total_time_ms = 0;
                current_song_info.total_time = 0;
            } else {
                stale_song_total = None;
            }
        }

        // SMTC positions are now drift-corrected via LastUpdatedTime in smtc.rs.
        // Only apply the user-configurable offset for fine-tuning.
        let smtc_offset_ms = config.settings.smtc_offset_ms;
        let display_time_ms = if current_song_info.is_playing {
            let adjusted = current_song_info.current_time_ms + smtc_offset_ms;
            if current_song_info.total_time_ms > 0 {
                adjusted.min(current_song_info.total_time_ms)
            } else {
                adjusted
            }
        } else {
            current_song_info.current_time_ms
        };

        // 广播最新状态给所有 WebSocket 客户端
        let _ = tx.send(current_song_info.clone());

        // 如果存在 Tauri app_handle，则广播给 GUI 前端
        if let Some(ref handle) = app_handle {
            use tauri::Emitter;
            let _ = handle.emit("song-info", current_song_info.clone());
        }

        // 写入文件
        if config.settings.output_txt && song_changed {
            if let Err(e) = write_info_to_txt(&current_song_info, &config.settings.txt_filename) {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error writing to txt file: {}", e);
                }
            }
        }

        if config.settings.output_json {
            if let Err(e) = write_info_to_json(&current_song_info, &config.settings.json_filename) {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error writing to json file: {}", e);
                }
            }
        }

        if config.settings.output_lyric {
            let precise_time_ms = display_time_ms;
            let mut filtered_lyrics = String::new();

            if !current_song_info.qrc_data.is_empty() {
                let (qrc_line, trans_line) = get_current_qrc_line(&current_song_info.qrc_data, &current_song_info.trans, precise_time_ms);
                if let Some(line) = qrc_line {
                    if trans_line.is_empty() {
                        filtered_lyrics = line.content.clone();
                    } else {
                        filtered_lyrics = format!("{}\n{}", line.content, trans_line);
                    }
                }
            }

            if filtered_lyrics.is_empty() {
                filtered_lyrics = filter_lyrics(&current_song_info.lyrics, &current_song_info.trans, current_song_info.current_time);
            }

            let display_lyric = if filtered_lyrics.trim().is_empty() {
                if current_song_info.is_valid() && current_song_info.title != "No music playing" {
                    "...\n\n".to_string()
                } else {
                    String::new()
                }
            } else {
                filtered_lyrics
            };

            if let Err(e) = write_info_to_lyric_txt(&display_lyric, &config.settings.lyric_filename) {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error writing lyric to txt file: {}", e);
                }
            }
        }

        last_song_info = Some(current_song_info);
        update_count += 1;

            Ok(())
        }.await;

        if let Err(e) = loop_result {
            if config.settings.debug_mode && !args.quiet {
                eprintln!("Critical loop error: {}", e);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        } else {
            let elapsed = loop_start.elapsed();
            let current_interval = if IS_BACKGROUND.load(Ordering::Relaxed) {
                Duration::from_millis(2000)
            } else {
                Duration::from_millis(config.settings.update_interval_ms)
            };
            if elapsed < current_interval {
                tokio::time::sleep(current_interval - elapsed).await;
            }
        }
    }


    Ok(())
}

/// 应用命令行参数覆盖配置
fn apply_cli_overrides(mut config: Config, args: &Cli) -> Config {
    if args.debug {
        config.settings.debug_mode = true;
    }
    
    if args.no_txt {
        config.settings.output_txt = false;
    }
    
    if args.no_json {
        config.settings.output_json = false;
    }

    if args.no_lyric {
        config.settings.output_lyric = false;
    }
    
    if args.txt_file != "now_playing.txt" {
        config.settings.txt_filename = args.txt_file.clone();
    }
    
    if args.json_file != "now_playing.json" {
        config.settings.json_filename = args.json_file.clone();
    }
    
    if args.lyric_file != "current_lyric.txt" {
        config.settings.lyric_filename = args.lyric_file.clone();
    }
    
    // CLI --interval 如果指定，则覆盖配置文件
    if let Some(interval) = args.interval {
        config.settings.update_interval_ms = interval;
    }

    // CLI --offset 如果指定，则覆盖配置文件
    if let Some(offset) = args.offset {
        config.settings.smtc_offset_ms = offset;
    }
    
    if args.no_server {
        config.settings.enable_server = false;
    }

    if args.port != 3000 {
        config.settings.server_port = args.port;
    }

    if args.retries != 3 {
        config.settings.max_retries = args.retries;
    }

    
    config
}

/// 将歌曲信息写入文本文件 (UTF-16 LE)
fn write_info_to_txt(info: &SongInfo, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let u16_string = U16String::from_str(&info.title);
    let bytes: Vec<u8> = u16_string.into_vec().into_iter().flat_map(|c| c.to_le_bytes().to_vec()).collect();
    let mut final_bytes = vec![0xFF, 0xFE]; 
    final_bytes.extend(bytes);
    file.write_all(&final_bytes)?;
    Ok(())
}

/// 过滤并提取当前进度的歌词
/// 注意：这里的实现比较简化，SMTC不提供精确的歌词行时间戳，
/// 且从网络下载的LRC是整个文件。
/// 根据当前时间获取对应的 QRC 行和翻译
fn get_current_qrc_line<'a>(qrc_data: &'a [QrcLine], trans: &'a str, current_time_ms: u64) -> (Option<&'a QrcLine>, String) {
    let mut current_line: Option<&QrcLine> = None;
    let mut max_start_time = 0;

    for line in qrc_data {
        // 如果当前时间已经超过这行的开始时间
        if current_time_ms >= line.start_time_ms {
            if line.start_time_ms >= max_start_time {
                max_start_time = line.start_time_ms;
                current_line = Some(line);
            }
        }
    }

    let mut current_trans = String::new();
    if let Some(line) = current_line {
        // 尝试从翻译中找出对应时间戳的行（最近邻匹配）
        if !trans.is_empty() {
            let line_time_sec = line.start_time_ms as f64 / 1000.0;
            let mut best_diff = f64::MAX;
            for trans_line in trans.lines() {
                if let Some(start_bracket) = trans_line.find('[') {
                    if let Some(end_bracket) = trans_line.find(']') {
                        let text = trans_line[end_bracket+1..].trim();
                        // Skip empty lines and "//" placeholders
                        if text.is_empty() || text == "//" {
                            continue;
                        }
                        let time_str = &trans_line[start_bracket+1..end_bracket];
                        if let Ok(time_val) = parse_lrc_time(time_str) {
                            let diff = (time_val - line_time_sec).abs();
                            if diff < 0.1 && diff < best_diff {
                                best_diff = diff;
                                current_trans = text.to_string();
                            }
                        }
                    }
                }
            }
        }
    }

    (current_line, current_trans)
}

/// 在字符索引处拆分字符串（支持中文字符）
fn split_str_at_char(s: &str, char_idx: usize) -> (&str, &str) {
    let mut ci = 0;
    for (i, _) in s.char_indices() {
        if ci == char_idx {
            return (&s[..i], &s[i..]);
        }
        ci += 1;
    }
    (s, "")
}

/// 渲染带逐字进度高亮的 QRC 歌词
fn render_qrc_line(line: &QrcLine, current_time_ms: u64, debug: bool) -> String {
    use crossterm::style::Stylize;

    // 计算行的有效结束时间：取行时长终点与每个字终点中的最大值，防止行时长小于字时长导致提前截断
    let line_end_ms = {
        let dur_end = if line.duration_ms > 0 {
            line.start_time_ms + line.duration_ms
        } else {
            0
        };
        let word_end = line.words.last()
            .map(|w| w.start_time_ms + w.duration_ms)
            .unwrap_or(0);
        let end = dur_end.max(word_end);
        if end > 0 { end } else { line.start_time_ms + 5000 }
    };

    if debug {
        eprintln!("[render] cur={} line_start={} line_end={} line_dur={} words={} content={:?}",
            current_time_ms, line.start_time_ms, line_end_ms, line.duration_ms,
            line.words.len(), line.content);
    }

    // 如果还没唱到这行，全灰
    if current_time_ms < line.start_time_ms {
        if debug { eprintln!("[render] -> BEFORE (dark_grey)"); }
        return line.content.clone().dark_grey().to_string();
    }

    // 如果这行已经唱完了，全黄
    if current_time_ms >= line_end_ms {
        if debug { eprintln!("[render] -> AFTER (yellow bold)"); }
        return line.content.clone().yellow().bold().to_string();
    }

    // 没有逐字数据时，按行整体进度显示
    if line.words.is_empty() {
        let progress = ((current_time_ms - line.start_time_ms) as f64
            / (line_end_ms - line.start_time_ms) as f64)
            .clamp(0.0, 1.0);
        let char_count = line.content.chars().count();
        let split_idx = ((char_count as f64) * progress).ceil() as usize;
        let (done, todo) = split_str_at_char(&line.content, split_idx);
        if debug { eprintln!("[render] -> EMPTY_WORDS progress={:.2}", progress); }
        return format!("{}{}", done.yellow().bold(), todo.dark_grey());
    }

    // 正在唱这一行：逐字渲染，当前字按进度分高低亮
    // word.start_time_ms 已是基于歌曲开头的绝对时间
    let mut result = String::new();
    for word in &line.words {
        let word_start_abs = word.start_time_ms;
        let word_dur = if word.duration_ms > 0 { word.duration_ms } else { 200 };
        let word_end_abs = word_start_abs + word_dur;

        if current_time_ms >= word_end_abs {
            if debug { eprintln!("[render]   word={:?} start={} end={} -> FINISHED", word.content, word_start_abs, word_end_abs); }
            // 这个字已经唱完，显示黄色
            result.push_str(&word.content.clone().yellow().bold().to_string());
        } else if current_time_ms >= word_start_abs && current_time_ms < word_end_abs {
            // 正在唱的字：按时间进度拆分为"已唱"和"未唱"两部分
            let progress = ((current_time_ms - word_start_abs) as f64 / word_dur as f64)
                .clamp(0.0, 1.0);
            let char_count = word.content.chars().count();
            let split_idx = ((char_count as f64) * progress).ceil() as usize;
            let (done, todo) = split_str_at_char(&word.content, split_idx);
            if debug { eprintln!("[render]   word={:?} start={} end={} -> IN_PROGRESS progress={:.2} split={}", word.content, word_start_abs, word_end_abs, progress, split_idx); }
            result.push_str(&done.yellow().bold().to_string());
            result.push_str(&todo.dark_grey().to_string());
        } else {
            if debug { eprintln!("[render]   word={:?} start={} end={} -> PENDING", word.content, word_start_abs, word_end_abs); }
            // 还没唱到的字，显示暗色
            result.push_str(&word.content.clone().dark_grey().to_string());
        }
    }

    result
}

/// 过滤并提取当前进度的歌词（支持双语）
fn filter_lyrics(lyrics: &str, trans: &str, current_time_sec: u64) -> String {
    let mut current_lyric = String::new();
    let mut current_trans = String::new();
    let mut max_time = -1.0;

    // 查找原文
    for line in lyrics.lines() {
        if let Some(start_bracket) = line.find('[') {
            if let Some(end_bracket) = line.find(']') {
                let time_str = &line[start_bracket+1..end_bracket];
                if let Ok(time_val) = parse_lrc_time(time_str) {
                    if time_val <= current_time_sec as f64 {
                        if time_val > max_time {
                            max_time = time_val;
                            current_lyric = line[end_bracket+1..].trim().to_string();
                        }
                    }
                }
            }
        }
    }

    // 如果有翻译，查找对应的翻译行
    if !trans.is_empty() && max_time >= 0.0 {
        for line in trans.lines() {
            if let Some(start_bracket) = line.find('[') {
                if let Some(end_bracket) = line.find(']') {
                    let time_str = &line[start_bracket+1..end_bracket];
                    if let Ok(time_val) = parse_lrc_time(time_str) {
                        let text = line[end_bracket+1..].trim();
                        // Skip empty lines and "//" placeholders
                        if text.is_empty() || text == "//" {
                            continue;
                        }
                        if (time_val - max_time).abs() < 0.1 {
                            current_trans = text.to_string();
                            break;
                        }
                    }
                }
            }
        }
    }
    
    // 如果没有带时间轴的原文，则退而求其次显示第一行
    if max_time == -1.0 && !lyrics.is_empty() {
        for line in lyrics.lines() {
            if !line.trim().is_empty() && !line.starts_with("[") {
                 current_lyric = line.trim().to_string();
                 break;
            }
        }
    }

    if current_trans.is_empty() {
        current_lyric
    } else {
        format!("{}\n{}", current_lyric, current_trans)
    }
}

fn parse_lrc_time(time_str: &str) -> Result<f64, ()> {
    // mm:ss.xx or mm:ss
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() == 2 {
        let min: f64 = parts[0].parse().map_err(|_| ())?;
        let sec: f64 = parts[1].parse().map_err(|_| ())?;
        return Ok(min * 60.0 + sec);
    }
    Err(())
}

/// 渲染内容并补空格到指定可见宽度
/// 解决 Stylize 的 ANSI 转义码被 `{:<w}` 计入宽度的问题
fn pad_line(text: &str, styled: String, width: usize) -> String {
    let visible = text.chars().count();
    if visible < width {
        format!("{}{}\n", styled, " ".repeat(width - visible))
    } else {
        format!("{}\n", styled)
    }
}

/// 对已包含 ANSI 转义码的字符串按可见宽度补空格
fn pad_styled(styled: &str, width: usize) -> String {
    // 粗略计算可见宽度：去掉 \x1b[... 转义序列后数可见字符
    // 不引入 regex 依赖，通过简单状态机跳过 ANSI 序列
    let mut visible = 0;
    let mut in_escape = false;
    for c in styled.chars() {
        if in_escape {
            if c == 'm' || c == 'H' { in_escape = false; }
            continue;
        }
        if c == '\x1b' { in_escape = true; continue; }
        visible += 1;
    }
    if visible < width {
        format!("{}{}", styled, " ".repeat(width - visible))
    } else {
        styled.to_string()
    }
}

/// 将歌曲信息完整写入JSON文件
fn write_info_to_json(info: &SongInfo, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let json_string = serde_json::to_string_pretty(info)?;
    file.write_all(json_string.as_bytes())?;
    Ok(())
}

/// 单独将当前歌词写入文本文件 (UTF-16 LE)
fn write_info_to_lyric_txt(lyric: &str, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let u16_string = U16String::from_str(lyric);
    let bytes: Vec<u8> = u16_string.into_vec().into_iter().flat_map(|c| c.to_le_bytes().to_vec()).collect();
    // 添加 UTF-16 LE BOM 方便 Windows Notepad 和 OBS 等识别
    let mut final_bytes = vec![0xFF, 0xFE]; 
    final_bytes.extend(bytes);
    file.write_all(&final_bytes)?;
    Ok(())
}

#[tokio::main]
// Function to launch the Tauri GUI and background monitor loop.
async fn main() -> Result<()> {
    // 解析命令行参数
    let args = Cli::parse_args();
    
    // 处理帮助和版本信息
    if args.help {
        Cli::show_help();
        return Ok(());
    }
    
    if args.version {
        Cli::show_version();
        return Ok(());
    }

    // 加载配置
    let config = Config::get_config();
    
    // 应用命令行参数覆盖配置
    let config = apply_cli_overrides(config, &args);

    // Initialize global config lock
    CONFIG.set(RwLock::new(config.clone()))
        .map_err(|_| anyhow::anyhow!("Failed to initialize global config OnceLock"))?;

    let monitor_args = args.clone();
    let monitor_config = config.clone();

    // 启动 Tauri 窗口
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![set_background_state, get_app_config, save_app_config])
        .setup(move |app| {
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build tokio runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async {
                    if let Err(e) = run_monitor(Some(app_handle), monitor_args, monitor_config).await {
                        eprintln!("Monitor loop error: {:?}", e);
                    }
                });
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    Ok(())
}

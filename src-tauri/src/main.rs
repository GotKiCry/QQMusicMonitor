#![allow(dead_code)]
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tokio::sync::RwLock;

static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();
static CACHED_CACHE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
static LATEST_SONG_INFO: OnceLock<RwLock<Arc<SongInfo>>> = OnceLock::new();
static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

/// 全局调试开关（main 启动时按配置设置，供非异步路径打印诊断）
fn debug_mode_enabled() -> bool {
    DEBUG_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FrontConfig {
    pub server_port: u16,
    pub smtc_offset_ms: i64,
    pub update_interval_ms: u64,
    pub output_txt: bool,
    pub output_json: bool,
    pub output_lyric: bool,
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

#[tauri::command]
async fn media_toggle_play_pause() -> Result<(), String> {
    smtc::try_toggle_play_pause().await.map_err(|e| format!("{}", e))
}

#[tauri::command]
async fn media_next() -> Result<(), String> {
    smtc::try_skip_next().await.map_err(|e| format!("{}", e))
}

#[tauri::command]
async fn media_previous() -> Result<(), String> {
    smtc::try_skip_previous().await.map_err(|e| format!("{}", e))
}

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use widestring::U16String;

mod cli;
mod config;
mod smtc;
mod lyrics;
mod song_info;
mod qrc;
mod local_qrc;
mod server;
mod audio_session;

use cli::Cli;
use config::Config;
use song_info::{PlaybackProgress, QrcLine, SongInfo};
use lyrics::LyricFetcher;

use std::collections::HashMap;

/// 当前 Unix 毫秒时间戳，用于 LRU 淘汰
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tauri::command]
async fn get_current_song_info() -> Result<SongInfo, String> {
    let latest = LATEST_SONG_INFO
        .get()
        .ok_or("Song state not initialized")?;
    Ok(latest.read().await.as_ref().clone())
}

fn apply_signed_offset(time_ms: u64, offset_ms: i64) -> u64 {
    if offset_ms >= 0 {
        time_ms.saturating_add(offset_ms as u64)
    } else {
        time_ms.saturating_sub(offset_ms.unsigned_abs())
    }
}

struct LastSongState {
    title: String,
    artist: String,
    total_time_ms: u64,
}

impl From<&SongInfo> for LastSongState {
    fn from(info: &SongInfo) -> Self {
        Self {
            title: info.title.clone(),
            artist: info.artist.clone(),
            total_time_ms: info.total_time_ms,
        }
    }
}

fn same_gui_payload(previous: &SongInfo, current: &SongInfo) -> bool {
    previous.title == current.title
        && previous.artist == current.artist
        && previous.album == current.album
        && previous.lyrics == current.lyrics
        && previous.trans == current.trans
        && previous.qrc_data == current.qrc_data
        && previous.album_pic_url == current.album_pic_url
        && previous.album_pic_hd_url == current.album_pic_hd_url
}

#[cfg(test)]
mod synchronization_tests {
    use super::*;

    #[test]
    fn signed_offset_supports_advance_and_delay() {
        assert_eq!(apply_signed_offset(1_000, 200), 1_200);
        assert_eq!(apply_signed_offset(1_000, -200), 800);
        assert_eq!(apply_signed_offset(100, -200), 0);
        assert_eq!(apply_signed_offset(u64::MAX - 5, 10), u64::MAX);
    }

    #[test]
    fn gui_payload_ignores_progress_only_changes() {
        let previous = SongInfo {
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            lyrics: "lyrics".to_string(),
            current_time_ms: 1_000,
            ..SongInfo::default()
        };
        let mut current = previous.clone();
        current.current_time_ms = 1_100;
        current.progress_percent = 12.5;
        current.server_ts = 42;

        assert!(same_gui_payload(&previous, &current));

        current.lyrics = "updated lyrics".to_string();
        assert!(!same_gui_payload(&previous, &current));
    }

    #[test]
    fn lyrics_cache_blocks_duplicate_and_immediate_retry() {
        let mut cache = LyricsCache::new();
        assert!(cache.begin_fetch("Song", "Artist"));
        assert!(!cache.begin_fetch("Song", "Artist"));

        cache.mark_retry("Song", "Artist");
        assert!(!cache.begin_fetch("Song", "Artist"));
    }

    #[test]
    fn album_names_match_accepts_consistent_names() {
        // 标点/括号差异容忍（SMTC 带括号后缀 vs 实际专辑名）
        assert!(album_names_match("ONE PIECE SUPER BEST (海贼王10周年 SUPER BEST)", "ONE PIECE SUPER BEST"));
        assert!(album_names_match("听！我们的歌", "听！我们的歌"));
        // 完全无关的专辑名不得误判为一致（如搜索 "Jay" 首条返回"钢琴曲纯音乐经典"）
        assert!(!album_names_match("Jay", "钢琴曲纯音乐经典"));
        assert!(!album_names_match("COUP D'ETAT [+ ONE OF A KIND & HEARTBREAKER]", "G-DRAGON 2ND ALBUM : COUP D`ETAT"));
        // 空串不可匹配
        assert!(!album_names_match("", "Jay"));
        assert!(!album_names_match("Jay", ""));
    }

    #[test]
    fn album_names_match_accepts_variant_naming() {
        // 同一专辑的不同命名变体：SMTC 报 "World War Joy (Explicit)"，
        // 歌曲详情返回 "World War Joy...Takeaway (Explicit)"——单词重叠应判为同一专辑
        assert!(album_names_match("World War Joy (Explicit)", "World War Joy...Takeaway (Explicit)"));
        // 重叠度不足的仍拒绝：相似但不同的专辑（0.75 重叠）与无关专辑
        assert!(!album_names_match("World War Joy (Explicit)", "World War III (Explicit)"));
        assert!(!album_names_match("Suicide Squad: The Album", "The Eminem Show (Clean)"));
        // 子集关系（"The Monster" ⊂ "The Monster Mash"）属于包含匹配，仍放行
        assert!(album_names_match("The Monster", "The Monster Mash"));
    }

    #[test]
    fn artist_names_match_stays_strict() {
        // 歌手判断必须严格：The Chainsmokers/Halsey 与 新田真剑佑/The Chainsmokers
        // 共享单词但完全不是同一首歌的歌手，不得误判
        assert!(!artist_names_match("The Chainsmokers/Halsey", "新田真剑佑/The Chainsmokers"));
        // 多歌手子集仍应放行（feat. 变体）
        assert!(artist_names_match("Eminem/Survivor", "Eminem"));
        assert!(artist_names_match("G-DRAGON (권지용)", "G-DRAGON"));
    }

    #[test]
    fn song_match_ok_rejects_wrong_artist_cover() {
        use lyrics::{AlbumCover, CoverKind};
        let refs = ["STAY (Explicit)"];
        // "Stay explicit" - 屈直林：歌手与查询不符，Single 封面也必须拒绝
        let wrong = AlbumCover {
            mid: "004LS1ro32YK1a".into(),
            name: String::new(),
            singer: "屈直林（Quandos-Lin）".into(),
            kind: CoverKind::Single,
        };
        assert!(!song_match_ok(&wrong, "The Kid LAROI/Justin Bieber", &refs),
            "歌手不符的单曲封面必须被拒绝");

        // 正确候选：STAY - The Kid LAROI/Justin Bieber，专辑名一致
        let right = AlbumCover {
            mid: "004OTM9c0OfNRw".into(),
            name: "STAY (Explicit)".into(),
            singer: "The Kid LAROI/Justin Bieber".into(),
            kind: CoverKind::Album,
        };
        assert!(song_match_ok(&right, "The Kid LAROI/Justin Bieber", &refs));

        // 多歌手子集（Eminem/Survivor vs Eminem）应放行
        let sub = AlbumCover {
            mid: "x".into(),
            name: "The Eminem Show (Clean)".into(),
            singer: "Eminem/Survivor".into(),
            kind: CoverKind::Album,
        };
        assert!(song_match_ok(&sub, "Eminem", &["The Eminem Show (Clean)"]));

        // 专辑名不符的 Album 候选拒绝（同歌手、不同专辑 = 不同版本）
        let wrong_album = AlbumCover {
            mid: "001sDxZZ0lzDJX".into(),
            name: "Suicide Squad: The Album".into(),
            singer: "Eminem".into(),
            kind: CoverKind::Album,
        };
        assert!(!song_match_ok(&wrong_album, "Eminem", &["The Eminem Show (Clean)"]));
    }

    #[test]
    fn album_pic_is_hd_detects_500_vs_150() {
        assert!(album_pic_is_hd(Path::new("T002R500x500M000004JAIMT1Pl1z9_1.jpg")));
        assert!(album_pic_is_hd(Path::new("T062R500x500M000002PE0rX0FxNck.jpg")));
        assert!(!album_pic_is_hd(Path::new("T002R150x150M000001rWGUJ3Gvypx_2.jpg")));
        assert!(!album_pic_is_hd(Path::new("T062R150x150M000002PE0rX0FxNck.jpg")));
        assert!(!album_pic_is_hd(Path::new("weird.txt")));
    }

    #[test]
    fn cover_cdn_url_uses_kind_prefix() {
        use lyrics::CoverKind;
        let album = cover_cdn_url(CoverKind::Album, "004JAIMT1Pl1z9");
        assert!(album.starts_with("https://y.gtimg.cn/music/photo_new/T002R800x800M000004JAIMT1Pl1z9.jpg"));
        let single = cover_cdn_url(CoverKind::Single, "002PE0rX0FxNck");
        assert!(single.starts_with("https://y.gtimg.cn/music/photo_new/T062R800x800M000002PE0rX0FxNck.jpg"));
    }
}

/// 缓存淘汰上限：超出后淘汰最久未访问的条目。
/// 每条含歌词文本 + QRC 逐字数据，按平均 8KB/条估算，128 条约 1MB 内存。
const LYRICS_CACHE_MAX_ENTRIES: usize = 128;
const LYRICS_RETRY_DELAY_MS: u64 = 30_000;
const FILE_OUTPUT_MIN_INTERVAL_MS: u64 = 100;

#[derive(Clone, Copy)]
enum LyricsFetchState {
    Loading,
    Ready,
    RetryAfter(u64),
}

/// 歌词/封面数据来源（日志与缓存标识：让用户明确看到用的是本地还是在线）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LyricsSource {
    Local,
    Online,
    None,
}

struct LyricsCacheEntry {
    lyrics: String,
    trans: String,
    qrc_raw: String,
    qrc_data: Vec<QrcLine>,
    album_pic_url: String,
    /// 高清封面 URL：album_pic_url 为低清本地图(150x150)时携带 800x800 在线地址
    album_pic_hd_url: String,
    /// 本地缓存文件名中解析出的专辑名（QQ 音乐索引时的原始名），
    /// 比依赖 SMTC 报告的 album 更可靠，用作在线 album_mid 解析的搜索词。
    local_album: String,
    /// 最后访问时间戳（毫秒），用于 LRU 淘汰
    last_accessed: u64,
    fetch_state: LyricsFetchState,
    source: LyricsSource,
}

impl LyricsCacheEntry {
    fn loading() -> Self {
        Self {
            lyrics: String::new(),
            trans: String::new(),
            qrc_raw: String::new(),
            qrc_data: Vec::new(),
            album_pic_url: String::new(),
            album_pic_hd_url: String::new(),
            local_album: String::new(),
            last_accessed: 0,
            fetch_state: LyricsFetchState::Loading,
            source: LyricsSource::None,
        }
    }
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

    fn begin_fetch(&mut self, title: &str, artist: &str) -> bool {
        let key = format!("{}|{}", title, artist);
        let now = current_timestamp_ms();

        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_accessed = now;
            return match entry.fetch_state {
                LyricsFetchState::RetryAfter(retry_at) if now >= retry_at => {
                    entry.fetch_state = LyricsFetchState::Loading;
                    true
                }
                LyricsFetchState::Loading
                | LyricsFetchState::Ready
                | LyricsFetchState::RetryAfter(_) => false,
            };
        }

        self.insert_entry(title, artist, LyricsCacheEntry::loading());
        true
    }

    fn mark_retry(&mut self, title: &str, artist: &str) {
        let key = format!("{}|{}", title, artist);
        if let Some(entry) = self.entries.get_mut(&key) {
            let now = current_timestamp_ms();
            entry.last_accessed = now;
            entry.fetch_state = LyricsFetchState::RetryAfter(
                now.saturating_add(LYRICS_RETRY_DELAY_MS),
            );
        }
    }

    fn get_entry(&mut self, title: &str, artist: &str) -> Option<&LyricsCacheEntry> {
        let key = format!("{}|{}", title, artist);
        let now = current_timestamp_ms();
        let entry = self.entries.get_mut(&key)?;
        entry.last_accessed = now;
        Some(entry)
    }

    /// 只读访问条目（不更新 LRU 时间戳），用于在 read guard 下读取字段。
    fn peek_entry(&self, title: &str, artist: &str) -> Option<&LyricsCacheEntry> {
        let key = format!("{}|{}", title, artist);
        self.entries.get(&key)
    }

    fn insert_entry(&mut self, title: &str, artist: &str, mut entry: LyricsCacheEntry) {
        let key = format!("{}|{}", title, artist);
        entry.last_accessed = current_timestamp_ms();

        if self.entries.len() >= LYRICS_CACHE_MAX_ENTRIES && !self.entries.contains_key(&key) {
            self.evict_oldest();
        }

        self.entries.insert(key, entry);
    }

    /// 部分更新：覆盖缓存中某首歌的封面（主图 + 高清升级地址）
    fn update_album_pic_url(&mut self, title: &str, artist: &str, url: String, hd_url: String) {
        let key = format!("{}|{}", title, artist);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.album_pic_url = url;
            entry.album_pic_hd_url = hd_url;
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
    CACHED_CACHE_ROOT
        .get_or_init(local_qrc::auto_detect_cache_root)
        .clone()
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

/// 歌手名一致性校验：normalize（小写 + 标点折叠）后互相包含即视为一致。
/// 严格匹配——歌手判断不能放宽（防止标题模糊命中其他歌手的翻唱）。
fn artist_names_match(a: &str, b: &str) -> bool {
    let na = local_qrc::normalize(a);
    let nb = local_qrc::normalize(b);
    !na.is_empty() && !nb.is_empty() && (na.contains(&nb) || nb.contains(&na))
}

/// 专辑名一致性校验：normalize 后互相包含即视为一致；
/// 或单词重叠度足够高（处理同一专辑的不同命名变体，
/// 如 "World War Joy (Explicit)" vs "World War Joy...Takeaway (Explicit)"）。
fn album_names_match(a: &str, b: &str) -> bool {
    let na = local_qrc::normalize(a);
    let nb = local_qrc::normalize(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na.contains(&nb) || nb.contains(&na) {
        return true;
    }
    // 单词重叠：过滤单字母词后，共同词数占较短者比例足够高即视为同一专辑
    let wa: std::collections::HashSet<String> = na
        .split(' ')
        .filter(|w| w.chars().count() > 1)
        .map(String::from)
        .collect();
    let wb: std::collections::HashSet<String> = nb
        .split(' ')
        .filter(|w| w.chars().count() > 1)
        .map(String::from)
        .collect();
    if wa.is_empty() || wb.is_empty() {
        return false;
    }
    let shared = wa.intersection(&wb).count();
    let min_len = wa.len().min(wb.len());
    // 0.8 阈值：变体命名（"World War Joy...Takeaway (Explicit)" vs
    // "World War Joy (Explicit)"，重叠 1.0）放行；相似但不同的专辑
    // （"World War Joy" vs "World War III"，重叠 0.75）拒绝
    shared >= 2 && (shared as f64 / min_len as f64) >= 0.8
}

/// 歌曲候选身份校验：只有候选歌曲与播放中歌曲一致时才采信其封面。
///
/// 1. 歌手必须匹配（normalize 互相包含）；候选或查询歌手为空时跳过。
///    防止标题模糊匹配命中别的歌（如 "STAY (Explicit)" 命中 "Stay explicit"，
///    歌手完全不同，其 vs 单曲封面也是错的）。
/// 2. Album 类候选：专辑名必须与参考专辑（本地 QRC 文件名 / SMTC）一致。
///    Single 类（无专辑，vs 单曲封面）在歌手通过后直接采信。
fn song_match_ok(cover: &lyrics::AlbumCover, query_artist: &str, refs: &[&str]) -> bool {
    if !cover.singer.is_empty() && !query_artist.is_empty() {
        if !artist_names_match(&cover.singer, query_artist) {
            return false;
        }
    }
    if cover.kind == lyrics::CoverKind::Album {
        if refs.is_empty() {
            return true;
        }
        return refs.iter().any(|r| album_names_match(r, &cover.name));
    }
    true
}

/// 本地封面文件是否为高清（R500x500）。
/// 仅 R150x150 时视为低清：调用方应以低清 data URI 即时显示，
/// 并携带 800x800 在线地址（album_pic_hd_url）供前端预加载后升级。
fn album_pic_is_hd(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains("R500x500"))
        .unwrap_or(false)
}

/// 按封面类型生成 800x800 在线 CDN URL（T002 专辑 / T062 单曲）
fn cover_cdn_url(kind: lyrics::CoverKind, mid: &str) -> String {
    let prefix = match kind {
        lyrics::CoverKind::Album => "T002",
        lyrics::CoverKind::Single => "T062",
    };
    format!("https://y.gtimg.cn/music/photo_new/{}R800x800M000{}.jpg?max_age=2592000", prefix, mid)
}

/// 优先使用本地缓存的专辑封面图，找不到则返回原始在线 URL。
///
/// 解析顺序：
///  0. SMTC 专辑名可用时，先解析「实际播放专辑」的封面——歌词搜索匹配到的
///     可能是不同版本（单曲版/合辑版，如 High Hopes 的 Pray for the Wicked
///     vs Summer 2018），其专辑封面与正在播放的不一致。专辑名搜索
///     （候选名逐条验证）→ legacy 兜底（强制歌手校验，防止通用专辑名
///     匹配到别的歌手的同名专辑）；本地优先，无本地图直接采用其 CDN URL。
///  1. 原在线 URL 的 album_mid 查本地（SMTC 专辑名为空时的兜底）。
///  2. 都未命中 → 原在线 URL。
///
/// 覆盖采用判定（matched_album 为歌词匹配版本的专辑名）：
///  解析出的 mid 与原文 URL mid 不同时，若歌词版本与 SMTC 专辑一致
///  （"Unstoppable" 场景：The Score 有两张同名专辑，legacy 排序可能
///  命中错版 002vztFj10OO9E）→ 原 URL（歌词精确匹配版本）更权威，回退；
///  不一致（合辑版场景）→ 解析结果才是实际播放专辑，采用。
///
/// 返回 (主图, 高清升级地址)：
///  - 本地命中 500x500 → (data URI, "")：已高清，无需升级
///  - 本地仅 150x150 → (data URI, 800x800 在线 URL)：先即时显示低清，
///    前端预加载高清后平滑升级（避免直接上在线 URL 的加载空白）
///  - 无本地图 → (在线 URL, "")：主图即在线高清图
async fn resolve_album_pic_url(
    online_url: &str,
    album_name: &str,
    matched_album: &str,
    title: &str,
    artist: &str,
    fetcher: &LyricFetcher,
) -> (String, String) {
    if online_url.is_empty() {
        return (String::new(), String::new());
    }

    // 只处理 QQ 音乐 CDN 的在线 URL（data:image base64 是 SMTC 缩略图，不替换）
    if !online_url.contains("y.gtimg.cn") {
        return (online_url.to_string(), String::new());
    }

    let Some(pic_dir) = get_picture_cache_dir() else {
        return (online_url.to_string(), String::new());
    };

    // 本地图 → data URI；返回 (uri, 该 mid 的高清在线 URL)。
    // 本地只有 150x150 时主图仍是 data URI（即时显示），高清地址交给前端升级。
    fn local_to_uri(pic_dir: &Path, mid: &str, hd_url: &str) -> Option<(String, String)> {
        let local_path = local_qrc::find_album_pic(pic_dir, mid)?;
        let data = std::fs::read(&local_path).ok()?;
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let uri = format!("data:image/jpeg;base64,{}", STANDARD.encode(&data));
        let hd = if album_pic_is_hd(&local_path) { String::new() } else { hd_url.to_string() };
        Some((uri, hd))
    }

    // 第 0 步：SMTC 专辑名解析"实际播放专辑"的封面（正确性优先——
    // 歌词匹配到的版本可能来自不同专辑，如合辑版 vs 原版）。
    if !album_name.is_empty() {
        let mut variants: Vec<String> = vec![album_name.to_string()];
        let cleaned = clean_album_name(album_name);
        if cleaned != album_name {
            variants.push(cleaned);
        }
        // 0a. SmartBox 专辑搜索（候选名去括号验证 + 歌手校验，
        //     通用专辑名会命中别的歌手的同名专辑，如 "Way Back" → "Way Back Home"）
        let mut correct_mid: Option<String> = None;
        for variant in &variants {
            if let Some(am) = fetcher.search_album_mid_by_name(variant, artist).await {
                correct_mid = Some(am);
                break;
            }
        }
        // 0b. legacy 歌曲搜索按专辑名兜底（三重校验：专辑名 + 歌名 + 歌手）
        if correct_mid.is_none() {
            if let Some(songmid) = fetcher.find_song_by_album_name(&variants, title, artist, album_name).await {
                if let Ok(cover) = fetcher.get_album_cover(&songmid).await {
                    if cover.kind == lyrics::CoverKind::Album && album_names_match(album_name, &cover.name) {
                        correct_mid = Some(cover.mid);
                    }
                }
            }
        }
        if let Some(mid) = correct_mid.as_ref() {
            // 采用判定：解析结果与原 URL 不同时，若歌词匹配版本与 SMTC 专辑一致，
            // 则原 URL（歌词精确匹配的专辑）更权威，拒绝覆盖（同名同歌手双专辑
            // 场景，如 The Score 的 Unstoppable 有 001wHjwD0IWIRm 与 002vztFj10OO9E
            // 两张同名专辑，legacy 排序可能命中错版）。
            let online_mid = local_qrc::extract_album_mid_from_url(online_url);
            if online_mid.as_deref() != Some(mid.as_str()) {
                if !matched_album.is_empty() && album_names_match(matched_album, album_name) {
                    if debug_mode_enabled() {
                        eprintln!("[lyrics] 封面: 歌词版本专辑'{}'与SMTC'{}'一致,拒绝覆盖解析结果 {}", matched_album, album_name, mid);
                    }
                    correct_mid = None;
                } else if debug_mode_enabled() {
                    eprintln!("[lyrics] 封面: 歌词版本'{}'≠SMTC'{}',采用解析结果 {}", matched_album, album_name, mid);
                }
            }
        }
        if let Some(mid) = correct_mid {
            let hd_url = format!("https://y.gtimg.cn/music/photo_new/T002R800x800M000{}.jpg?max_age=2592000", mid);
            if let Some(res) = local_to_uri(&pic_dir, &mid, &hd_url) {
                return res;
            }
            return (hd_url, String::new());
        }
    }

    // 第 1 步（兜底）：用在线 URL 中的 album_mid 查本地
    if let Some(album_mid) = local_qrc::extract_album_mid_from_url(online_url) {
        if let Some(res) = local_to_uri(&pic_dir, &album_mid, online_url) {
            return res;
        }
    }

    (online_url.to_string(), String::new())
}

/// 从本地 QQ 音乐缓存目录查找歌词（QRC 优先，LRC 兜底），返回完整的缓存条目。
/// 此函数包含文件 I/O 与 DES 解密，仅应在 spawn_blocking 中调用。
fn lookup_local_lyrics(title: &str, artist: &str) -> LyricsCacheEntry {
    let mut entry = LyricsCacheEntry::loading();

    let Some(cache_dir) = get_lyric_cache_dir() else {
        return entry;
    };

    // QRC 优先（逐字歌词）
    if let Some(qrc_file) = local_qrc::find_qrc_file(&cache_dir, title, artist) {
        // 记录本地文件名中的专辑名，供后续在线专辑封面解析复用
        if let Some(fname) = qrc_file.file_name().and_then(|n| n.to_str()) {
            entry.local_album = local_qrc::parse_lyric_filename(fname).2;
        }
        match qrc::decode_qrc_from_file(&qrc_file) {
            Ok(xml) => {
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
            Err(e) => {
                eprintln!("  ⚠ QRC解密失败: {:?} — {}", qrc_file.file_name().unwrap_or_default(), e);
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
        } else {
            // LRC 也未找到：打印诊断，帮助判断是文件名匹配问题还是真的没有文件
            if let Ok(entries) = std::fs::read_dir(&cache_dir) {
                let norm_title = local_qrc::normalize(title);
                let mut lrc_candidates: Vec<String> = Vec::new();
                for e in entries.flatten() {
                    let fname = match e.file_name().to_str() {
                        Some(n) => n.to_string(), None => continue,
                    };
                    if !fname.ends_with("_qm.lrc") { continue; }
                    let base = fname.trim_end_matches("_qm.lrc");
                    let parts: Vec<&str> = base.splitn(4, " - ").collect();
                    if parts.len() >= 2 && local_qrc::normalize(parts[1]).contains(&norm_title) {
                        lrc_candidates.push(fname);
                    }
                }
                if !lrc_candidates.is_empty() {
                    eprintln!("  ℹ LRC候选(未匹配): {}", lrc_candidates.join(", "));
                }
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
    DEBUG_ENABLED.store(config.settings.debug_mode, Ordering::SeqCst);

    // 初始化歌词获取器和后台缓存
    let lyric_fetcher = Arc::new(LyricFetcher::with_debug(config.settings.debug_mode));
    let lyrics_cache = Arc::new(RwLock::new(LyricsCache::new()));

    // WASAPI 音频会话门控：实时检测 QQ音乐是否真的在出声
    audio_session::AudioGate::ensure_com_init();
    let mut audio_gate = audio_session::AudioGate::new();

    // 初始化数据广播通道并启动服务
    let (tx, rx) = tokio::sync::watch::channel(Arc::new(SongInfo::default()));
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





    let mut last_song_state: Option<LastSongState> = None;
    let mut last_gui_snapshot: Option<SongInfo> = None;
    let mut stale_song_total: Option<u64> = None;
    let mut last_logged_lyric_key = String::new();
    let mut last_json_write_at: Option<tokio::time::Instant> = None;
    let mut last_written_lyric: Option<String> = None;

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
                    let quiet = args.quiet;
                    let debug = config.settings.debug_mode;
                    let song_key = format!("{}|{}", info.title, info.artist);
                    let should_fetch = lyrics_cache
                        .write()
                        .await
                        .begin_fetch(&info.title, &info.artist);

                    if should_fetch {
                        // 标记已记录：避免同一首歌在获取完成前被缓存命中路径重复输出
                        last_logged_lyric_key = song_key;
                        let cache = lyrics_cache.clone();
                        let fetcher = lyric_fetcher.clone();
                        let t = info.title.clone();
                        let a = info.artist.clone();
                        let album = info.album.clone();
                        let max_attempts = config.settings.max_retries.saturating_add(1);
                        tokio::spawn(async move {
                            let has_local_qrc;
                            // 1. 本地歌词快速查找并立即插入缓存（~50ms，不阻塞在线获取）
                            {
                                let t2 = t.clone();
                                let a2 = a.clone();
                                let mut local = tokio::task::spawn_blocking(move || lookup_local_lyrics(&t2, &a2))
                                    .await
                                    .unwrap_or_else(|_| LyricsCacheEntry::loading());

                                let local_qrc = local.qrc_data.len();
                                let local_lrc = !local.lyrics.is_empty();
                                let local_trans = !local.trans.is_empty();
                                if debug {
                                    if local_qrc > 0 || local_lrc || local_trans {
                                        let mut parts: Vec<String> = Vec::new();
                                        if local_qrc > 0 { parts.push(format!("QRC {}行", local_qrc)); }
                                        if local_lrc { parts.push("歌词".to_string()); }
                                        if local_trans { parts.push("翻译".to_string()); }
                                        eprintln!("[lyrics] 本地: ✓ {}", parts.join(" + "));
                                    } else {
                                        eprintln!("[lyrics] 本地: ✗ 无数据");
                                        // 诊断：显示文件名格式 + 模糊匹配候选
                                        eprintln!("[lyrics]   搜索词: artist='{}' title='{}'", local_qrc::normalize(&a), local_qrc::normalize(&t));
                                        if let Some(ref dir) = get_lyric_cache_dir() {
                                            if let Ok(entries) = std::fs::read_dir(dir) {
                                                let mut qrc_count = 0u32;
                                                let mut best_candidate: Option<(String, u32)> = None;
                                                let norm_title = local_qrc::normalize(&t);
                                                let norm_artist = local_qrc::normalize(&a);
                                                for entry in entries.flatten() {
                                                    let fname = match entry.file_name().to_str() {
                                                        Some(n) => n.to_string(),
                                                        None => continue,
                                                    };
                                                    if !fname.ends_with("_qm.qrc") { continue; }
                                                    qrc_count += 1;
                                                    // 计算与当前歌曲的匹配度
                                                    let base = fname.trim_end_matches("_qm.qrc");
                                                    let parts: Vec<&str> = base.splitn(4, " - ").collect();
                                                    if parts.len() >= 2 {
                                                        let f_artist = local_qrc::normalize(parts[0]);
                                                        let f_title = local_qrc::normalize(parts[1]);
                                                        let mut score: u32 = 0;
                                                        if f_title == norm_title { score += 100; }
                                                        else if f_title.contains(&norm_title) || norm_title.contains(&f_title) { score += 60; }
                                                        if f_artist == norm_artist { score += 50; }
                                                        else if f_artist.contains(&norm_artist) || norm_artist.contains(&f_artist) { score += 30; }
                                                        if score > 0 && (best_candidate.is_none() || score > best_candidate.as_ref().unwrap().1) {
                                                            best_candidate = Some((fname, score));
                                                        }
                                                    }
                                                }
                                                eprintln!("[lyrics]   目录QRC总数: {} | 最佳候选: {}", qrc_count,
                                                    best_candidate.map_or("无部分匹配".to_string(), |(f, s)| format!("'{}' (分数:{})", f, s)));
                                            }
                                        }
                                    }
                                }

                                has_local_qrc = !local.qrc_data.is_empty();

                                if !local.lyrics.is_empty() || has_local_qrc {
                                    local.fetch_state = if has_local_qrc {
                                        LyricsFetchState::Ready
                                    } else {
                                        LyricsFetchState::Loading
                                    };
                                    local.source = LyricsSource::Local;
                                    let mut w = cache.write().await;
                                    w.insert_entry(&t, &a, local);
                                    if debug { eprintln!("[lyrics] Local cache populated for '{} - {}'", t, a); }
                                }
                            }

                            // 2. 在线歌词获取（仅当本地无 QRC 时才请求，作为备用手段）
                            if !has_local_qrc {
                            let mut attempt = 0u32;
                            let online_result = loop {
                                attempt += 1;
                                match fetcher.fetch_lyrics(&t, &a).await {
                                    Ok(result) => break Ok(result),
                                    Err(error) if attempt < max_attempts => {
                                        let backoff_ms = 250u64
                                            .saturating_mul(1u64 << (attempt - 1).min(3));
                                        if debug {
                                            eprintln!(
                                                "[lyrics] attempt {}/{} failed, retrying in {}ms: {}",
                                                attempt, max_attempts, backoff_ms, error
                                            );
                                        }
                                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                                    }
                                    Err(error) => break Err(error),
                                }
                            };

                            match online_result {
                                Ok((l, trans, q, pic_url, matched_album)) => {
                                    let (resolved_pic_url, resolved_hd_url) =
                                        resolve_album_pic_url(&pic_url, &album, &matched_album, &t, &a, &fetcher).await;
                                    let mut entry = LyricsCacheEntry {
                                        lyrics: l,
                                        trans,
                                        qrc_raw: q,
                                        qrc_data: Vec::new(),
                                        album_pic_url: resolved_pic_url,
                                        album_pic_hd_url: resolved_hd_url,
                                        local_album: String::new(),
                                        last_accessed: 0,
                                        fetch_state: LyricsFetchState::Ready,
                                        source: LyricsSource::None,
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

                                    // 复用本地缓存条目中的专辑名（优于依赖 SMTC album）
                                    if entry.local_album.is_empty() {
                                        let mut cached = cache.write().await;
                                        if let Some(cur) = cached.get_entry(&t, &a) {
                                            entry.local_album = cur.local_album.clone();
                                        }
                                    }

                                    // 汇总在线结果
                                    if !quiet {
                                        let has_lyric = !entry.lyrics.is_empty() || !entry.qrc_data.is_empty();
                                        let art_label = if entry.album_pic_url.starts_with("data:") {
                                            "本地"
                                        } else if !entry.album_pic_url.is_empty() {
                                            "在线"
                                        } else {
                                            "无"
                                        };
                                        eprintln!("[歌词] {} - {} | 歌词: {} | 封面: {}",
                                            a, t,
                                            if has_lyric { "在线" } else { "无" },
                                            art_label);
                                    }

                                    entry.source = LyricsSource::Online;
                                    let mut w = cache.write().await;
                                    w.insert_entry(&t, &a, entry);
                                    if debug { eprintln!("[lyrics] ✓ Background fetch complete for '{} - {}'", t, a); }
                                }
                                Err(e) => {
                                    if !quiet {
                                        eprintln!("[歌词] {} - {} | 歌词: 无 | 封面: 无 (失败,稍后重试)", a, t);
                                    }
                                    if debug { eprintln!("[lyrics] 在线获取失败: {}", e); }
                                    let mut cached = cache.write().await;
                                    cached.mark_retry(&t, &a);
                                }
                            }
                            } // if !has_local_qrc
                            else {
                                // 本地有 QRC 但缺少封面：解析封面 mid → 优先查本地 QQMusicPicture → 在线 CDN 兜底
                                //
                                // 1) 取封面 mid：
                                //    a. 多策略搜歌曲 songmid → get_album_cover（解决 "(Explicit)" 等括号后缀
                                //       导致单策略搜不到的问题，与 fetch_lyrics 策略对齐）；
                                //       有可对照专辑名时校验一致性，防止匹配到同名翻唱/合辑版本
                                //    b. 失败/被拒则用专辑名搜 SmartBox 专辑（候选名逐条验证，
                                //       候选为本地文件名专辑名（QQ 音乐索引原始名，比 SMTC album
                                //       更可靠）与 SMTC album，均清洗去括号后再搜）
                                // 2) 有了封面 mid：本地命中→base64 data URI（与在线路径一致，
                                //    避免 file:// 在 webview 受限）；否则返回在线 CDN URL
                                //    （T002 专辑封面 / T062 无专辑歌曲的单曲封面）
                                let get_pic = async {
                                    // 从本地缓存条目取文件名解析的专辑名（QQ 音乐索引时的原始名，
                                    // 比 SMTC 报告的 album 更可靠，用作校验与专辑搜索的参照）
                                    let local_album = {
                                        let cached = cache.read().await;
                                        cached.peek_entry(&t, &a).map(|e| e.local_album.clone()).unwrap_or_default()
                                    };
                                    let mut cover: Option<lyrics::AlbumCover> = None;
                                    // 无专辑单曲(vs 封面)兜底：1a 命中的候选无专辑信息时先记住，
                                    // 等 1b/1c 按专辑名解析失败后再回退——避免 vs 单曲封面
                                    // 直接胜出、错失正确专辑（如 BIGBANG 一天一天：smartbox 命中
                                    // 无专辑单曲版，但正在播放的是 Stand Up 专辑版）。
                                    let mut single_fallback: Option<lyrics::AlbumCover> = None;

                                    // 1a) 多策略搜歌曲 → 歌曲详情取封面 mid。
                                    //     候选必须通过身份校验（歌手一致 + 有专辑时专辑名与
                                    //     本地文件名/SMTC 一致），防止标题模糊匹配命中别的歌
                                    //     （如 "STAY (Explicit)" 命中 "Stay explicit" 的翻唱，
                                    //     其 vs 单曲封面是错歌的封面）。
                                    if let Some(songmid) = fetcher.search_song_mid(&t, &a).await {
                                        if let Ok(c) = fetcher.get_album_cover(&songmid).await {
                                            let refs: Vec<&str> = [local_album.as_str(), album.as_str()]
                                                .into_iter()
                                                .filter(|s| !s.is_empty())
                                                .collect();
                                            if song_match_ok(&c, &a, &refs) {
                                                if c.kind == lyrics::CoverKind::Single && !refs.is_empty() {
                                                    // 候选无专辑(vs 单曲封面)但播放中歌曲有专辑名可对照：
                                                    // 多半是不同版本，降级为兜底，优先按专辑名解析正确专辑
                                                    if debug { eprintln!("[lyrics] 候选为无专辑单曲(vs封面), 优先按专辑名解析"); }
                                                    single_fallback = Some(c);
                                                } else {
                                                    cover = Some(c);
                                                }
                                            } else if debug {
                                                eprintln!("[lyrics] 封面: 歌曲身份不符('{}' - '{}'), 尝试专辑名搜索", c.singer, c.name);
                                            }
                                        }
                                    }

                                    // 1b) 歌曲搜索失败或被校验拒绝 → 用专辑名搜专辑（候选：本地文件名
                                    //     专辑 + SMTC album，各自清洗去括号后作为额外候选）
                                    let mut album_variants: Vec<String> = Vec::new();
                                    for name in [local_album.clone(), album.clone()].into_iter().filter(|s| !s.is_empty()) {
                                        let cleaned = clean_album_name(&name);
                                        album_variants.push(cleaned.clone());
                                        if !cleaned.is_empty() && cleaned != name {
                                            album_variants.push(name);
                                        }
                                    }
                                    if cover.is_none() {
                                        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                                        for variant in album_variants.iter() {
                                            let v = variant.trim().to_string();
                                            if v.is_empty() || !seen.insert(v.clone()) { continue; }
                                            if let Some(am) = fetcher.search_album_mid_by_name(&v, &a).await {
                                                cover = Some(lyrics::AlbumCover { mid: am, name: String::new(), singer: String::new(), kind: lyrics::CoverKind::Album });
                                                break;
                                            }
                                        }
                                    }

                                    // 1c) SmartBox 专辑搜索索引不到的专辑（实测 "The Eminem Show" 返回 0 条）：
                                    //     legacy 歌曲搜索按专辑名找候选歌（结果自带专辑名，可与参考专辑
                                    //     校验），命中后走歌曲详情取封面，同样过身份校验
                                    if cover.is_none() && !album_variants.is_empty() {
                                        let ref_name = if !local_album.is_empty() { local_album.as_str() } else { album.as_str() };
                                        if let Some(songmid) = fetcher.find_song_by_album_name(&album_variants, &t, &a, ref_name).await {
                                            if let Ok(c) = fetcher.get_album_cover(&songmid).await {
                                                let refs: Vec<&str> = [local_album.as_str(), album.as_str()]
                                                    .into_iter()
                                                    .filter(|s| !s.is_empty())
                                                    .collect();
                                                if song_match_ok(&c, &a, &refs) {
                                                    if debug { eprintln!("[lyrics] 封面: legacy 专辑搜索命中 '{}'", c.name); }
                                                    cover = Some(c);
                                                }
                                            }
                                        }
                                    }

                                    // 1d) 专辑名解析均未命中 → 回退 1a 的无专辑单曲(vs)封面兜底
                                    if cover.is_none() {
                                        cover = single_fallback;
                                    }

                                    // 2) 封面 mid → 本地图片优先（T002 专辑 / T062 单曲），否则在线 URL。
                                    //    返回 (主图, 高清升级地址)：本地仅 150x150 时主图用低清 data URI
                                    //    即时显示，高清 800x800 在线地址交给前端预加载后平滑升级；
                                    //    本地 500x500 或直接在线 URL 时主图即高清，无升级地址。
                                    if let Some(c) = cover {
                                        if let Some(pic_dir) = get_cache_root().map(|r| r.join("QQMusicPicture")) {
                                            if let Some(local_pic) = local_qrc::find_album_pic(&pic_dir, &c.mid) {
                                                if let Ok(data) = std::fs::read(&local_pic) {
                                                    use base64::{Engine as _, engine::general_purpose::STANDARD};
                                                    let data_uri = format!("data:image/jpeg;base64,{}", STANDARD.encode(&data));
                                                    let is_hd = album_pic_is_hd(&local_pic);
                                                    if debug { eprintln!("[lyrics] 封面: ✓ 本地{}", if is_hd { "" } else { " (低清,将升级高清)" }); }
                                                    let hd_url = if is_hd {
                                                        String::new()
                                                    } else {
                                                        cover_cdn_url(c.kind, &c.mid)
                                                    };
                                                    return Some((data_uri, hd_url));
                                                }
                                            }
                                        }
                                        let pic_url = cover_cdn_url(c.kind, &c.mid);
                                        if debug { eprintln!("[lyrics] 封面: ✓ 在线"); }
                                        return Some((pic_url, String::new()));
                                    }
                                    None
                                };

                                if let Some((pic_url, hd_url)) = get_pic.await {
                                    let art_label = if pic_url.starts_with("data:") { "本地" } else { "在线" };
                                    let mut w = cache.write().await;
                                    w.update_album_pic_url(&t, &a, pic_url, hd_url);
                                    if !quiet {
                                        eprintln!("[歌词] {} - {} | 歌词: 本地 | 封面: {}", a, t, art_label);
                                    }
                                } else if !quiet {
                                    eprintln!("[歌词] {} - {} | 歌词: 本地 | 封面: 无", a, t);
                                }
                            }
                        });
                    } else {
                        if !quiet && song_key != last_logged_lyric_key {
                            let (lyric_label, art_label) = {
                                let cached = lyrics_cache.read().await;
                                match cached.peek_entry(&info.title, &info.artist) {
                                    Some(e) => {
                                        let l = match e.source {
                                            LyricsSource::Local => "本地",
                                            LyricsSource::Online => "在线",
                                            LyricsSource::None => "无",
                                        };
                                        let a = if e.album_pic_url.starts_with("data:") {
                                            "本地"
                                        } else if !e.album_pic_url.is_empty() {
                                            "在线"
                                        } else {
                                            "无"
                                        };
                                        (l, a)
                                    }
                                    None => ("无", "无"),
                                }
                            };
                            eprintln!("[歌词] {} - {} | 歌词: {} | 封面: {} (缓存)",
                                info.artist, info.title, lyric_label, art_label);
                            last_logged_lyric_key = song_key;
                        }
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
                            if !entry.album_pic_hd_url.is_empty() {
                                info.album_pic_hd_url = entry.album_pic_hd_url.clone();
                            }
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
                        album_pic_hd_url: String::new(),
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
                    album_pic_hd_url: String::new(),
                    server_ts: 0,
                }
            }
        };

        // 检查歌曲是否有变化 (用于切歌修正与文件输出)
        let song_changed = match &last_song_state {
            Some(last) => last.title != current_song_info.title || last.artist != current_song_info.artist,
            None => true,
        };

        // SMTC 切歌瞬间修正：media properties（title/artist）先更新，
        // timeline（Position/EndTime）可能滞后 1~2 秒仍报旧歌数据。
        //
        // 只靠 song_changed 一帧归零不够：后续帧 title 未变但 timeline 仍旧，
        // 前端会收到"新歌名 + 旧进度"错配——旧歌词继续渲染，进度来回跳。
        //
        // 解法：用上一首歌的 total_time_ms 做锚点——若当前 total 与旧歌
        // 相同（±500ms），判定 timeline 未更新，强制归零。total 变化后清除追踪。
        if song_changed {
            if let Some(old) = &last_song_state {
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

        // —— WASAPI 音频会话门控：用「是否真实出声」修正 SMTC 声称的状态 ——
        // - 播放中持续静音：判定停滞，冻结在当前位置（QQ 恢复后锚定修正）
        // - 会话停止流但 SMTC 未报：提前按暂停显示
        // 恢复播放的位置冻结在 smtc.rs 的 resume hold 里处理（感知锚定状态）。
        // 找不到 QQ 音频会话时 AudioGate 内部回退为透传。
        if song_changed {
            audio_gate.reset();
        }
        // 暂停淡出窗口内由 smtc 的淡出状态机全权处理 is_playing，
        // 门控的 inactive 判定会在淡出收尾前抢先把状态翻成暂停，
        // 与仍在前进的淡出报进打架（前端「已暂停但位置还在走」）
        if current_song_info.is_valid() && !smtc::pause_fade_active().await {
            match audio_gate.update(current_song_info.is_playing) {
                audio_session::GateDecision::Pass => {}
                audio_session::GateDecision::ForcePause => {
                    current_song_info.is_playing = false;
                }
            }
        }

        // SMTC positions are drift-corrected via LastUpdatedTime in smtc.rs.
        // Apply the user-configurable offset for fine-tuning.
        // 注意：playing 和 pause 都加 offset，避免 play→pause 切换瞬间 display 因
        // offset 消失而回退（~200ms），造成歌词行瞬间变灰再回正的闪烁。
        // 例外：切歌后 timeline 归零窗口（total==0）内不套偏移——占位值保持 0，
        // 否则前端插值基线从 200ms 起跑，timeline 就绪时被拉回真实位置
        // （回跳近 500ms，叠加前端"淡出尾音"钉回逻辑 = 歌词开头疯狂闪烁）。
        let smtc_offset_ms = config.settings.smtc_offset_ms;
        let adjusted = if current_song_info.total_time_ms > 0 {
            apply_signed_offset(current_song_info.current_time_ms, smtc_offset_ms)
        } else {
            current_song_info.current_time_ms
        };
        let display_time_ms = if current_song_info.total_time_ms > 0 {
            adjusted.min(current_song_info.total_time_ms)
        } else {
            adjusted
        };

        current_song_info.current_time_ms = display_time_ms;
        current_song_info.current_time = display_time_ms / 1000;
        if current_song_info.total_time_ms > 0 {
            current_song_info.progress_percent =
                (display_time_ms as f64 / current_song_info.total_time_ms as f64 * 100.0) as f32;
        }

        let current_song_info = Arc::new(current_song_info);

        if let Some(latest) = LATEST_SONG_INFO.get() {
            *latest.write().await = Arc::clone(&current_song_info);
        }

        // WebSocket/API 保持完整 SongInfo 协议；watch 通道仅克隆 Arc，
        // 避免每个轮询周期深拷贝整份逐字歌词。
        let _ = tx.send(Arc::clone(&current_song_info));

        // Tauri 主通道只在静态内容变化时发送完整快照；普通轮询仅发送
        // 数十字节的进度事件，前端仍按原轮询频率接收并进行 60fps 插值。
        if let Some(ref handle) = app_handle {
            use tauri::Emitter;
            let static_changed = last_gui_snapshot
                .as_ref()
                .map(|previous| !same_gui_payload(previous, current_song_info.as_ref()))
                .unwrap_or(true);

            if static_changed {
                if handle.emit("song-info", current_song_info.as_ref()).is_ok() {
                    last_gui_snapshot = Some(current_song_info.as_ref().clone());
                }
            } else {
                let progress = PlaybackProgress::from(current_song_info.as_ref());
                let _ = handle.emit("playback-progress", progress);
            }
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
            let json_due = song_changed
                || last_json_write_at
                    .map(|last_write| {
                        last_write.elapsed()
                            >= Duration::from_millis(FILE_OUTPUT_MIN_INTERVAL_MS)
                    })
                    .unwrap_or(true);

            if json_due {
                match write_info_to_json(&current_song_info, &config.settings.json_filename) {
                    Ok(()) => last_json_write_at = Some(tokio::time::Instant::now()),
                    Err(e) => {
                        if config.settings.debug_mode && !args.quiet {
                            eprintln!("Error writing to json file: {}", e);
                        }
                    }
                }
            }
        } else {
            last_json_write_at = None;
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

            if last_written_lyric.as_deref() != Some(display_lyric.as_str()) {
                match write_info_to_lyric_txt(&display_lyric, &config.settings.lyric_filename) {
                    Ok(()) => last_written_lyric = Some(display_lyric),
                    Err(e) => {
                        if config.settings.debug_mode && !args.quiet {
                            eprintln!("Error writing lyric to txt file: {}", e);
                        }
                    }
                }
            }
        } else {
            last_written_lyric = None;
        }

        last_song_state = Some(LastSongState::from(current_song_info.as_ref()));

            Ok(())
        }.await;

        if let Err(e) = loop_result {
            if config.settings.debug_mode && !args.quiet {
                eprintln!("Critical loop error: {}", e);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        } else {
            let elapsed = loop_start.elapsed();
            // 固定按 update_interval_ms 轮询，不再随窗口可见性降频。
            // 本程序的核心职责是实时向外提供数据（WebSocket/HTTP/文件），
            // 窗口最小化/隐藏时若把轮询降到 2s，所有消费方（如壁纸应用）
            // 的歌词/进度更新会滞后 ~2s——而 SMTC 事件通知已让空闲期的
            // 实际唤醒次数很低，降频省下的 CPU 与体验损失不成比例。
            let current_interval = Duration::from_millis(config.settings.update_interval_ms);
            if elapsed < current_interval {
                // 除周期 tick 外，SMTC 事件（暂停/恢复/seek/切歌）随时
                // 唤醒主循环做下一轮，把状态切换延迟从轮询周期压到 ~10ms 级
                let state_notify = smtc::state_changed_notify();
                tokio::select! {
                    _ = tokio::time::sleep(current_interval - elapsed) => {}
                    _ = state_notify.notified() => {}
                }
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
        if current_time_ms >= line.start_time_ms && line.start_time_ms >= max_start_time {
            max_start_time = line.start_time_ms;
            current_line = Some(line);
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
    for (ci, (i, _)) in s.char_indices().enumerate() {
        if ci == char_idx {
            return (&s[..i], &s[i..]);
        }
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
                    if time_val <= current_time_sec as f64 && time_val > max_time {
                        max_time = time_val;
                        current_lyric = line[end_bracket+1..].trim().to_string();
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
    LATEST_SONG_INFO
        .set(RwLock::new(Arc::new(SongInfo::default())))
        .map_err(|_| anyhow::anyhow!("Failed to initialize latest song state"))?;

    let monitor_args = args.clone();
    let monitor_config = config.clone();

    // 启动 Tauri 窗口
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_app_config, save_app_config, get_current_song_info, media_toggle_play_pause, media_next, media_previous])
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

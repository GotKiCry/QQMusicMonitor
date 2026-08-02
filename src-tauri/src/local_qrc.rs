use std::path::{Path, PathBuf};

/// 本地文件匹配最低接受分数。
///
/// 分数构成：标题完全一致 +100 / 标题子串 +60；歌手完全一致 +50 / 歌手子串 +30。
/// 只有「标题完全一致」（≥100）或「标题子串 + 歌手完全一致」（≥110）才接受，
/// 拒绝「标题子串 + 歌手零分」（60）的弱匹配。
///
/// 背景：QQ 音乐在歌曲开始播放后才写入歌词缓存文件（滞后数秒）。若程序在
/// 窗口期内扫描目录，会把"张杰 - 这，就是爱"错误匹配到同目录下的
/// "蔡依林 - 就是爱"（标题"就是爱"是"这 就是爱"的子串，歌手完全不匹配），
/// 且错误匹配会被内存 LyricsCache 以 Ready 状态锁死，永远显示错歌歌词。
const MIN_ACCEPT_SCORE: u32 = 100;

/// 探测 QQ 音乐缓存根目录（如 `D:\QQMusicCache`）。
/// 歌词在 `QQMusicLyricNew` 子目录，封面图在 `QQMusicPicture` 子目录。
pub fn auto_detect_cache_root() -> Option<PathBuf> {
    // Step 1: Try reading cache path from QQ Music config file
    if let Some(root) = detect_cache_root_from_config() {
        return Some(root);
    }

    // Step 2: Fallback - scan all drive letters for QQMusicCache
    for letter in b'C'..=b'Z' {
        let path = PathBuf::from(format!("{}:\\QQMusicCache", letter as char));
        if path.exists() && path.is_dir() {
            return Some(path);
        }
    }

    None
}

/// 从 WebkitCachePath.ini 推断缓存根目录
fn detect_cache_root_from_config() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    let ini_path = PathBuf::from(&appdata)
        .join("Tencent")
        .join("QQMusic")
        .join("WebkitCachePath.ini");

    let content = std::fs::read_to_string(&ini_path).ok()?;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(raw_path) = trimmed.strip_prefix("Path=") {
            // raw_path 形如 D:\QQMusicCache\WebkitCache，parent 即缓存根
            let cache_root = PathBuf::from(raw_path).parent()?.to_path_buf();
            if cache_root.is_dir() {
                return Some(cache_root);
            }
        }
    }

    None
}

// Function to find the main QRC lyric file for a given song
pub fn find_qrc_file(cache_dir: &Path, title: &str, artist: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(cache_dir).ok()?;

    // Normalize search terms for fuzzy matching
    let norm_title = normalize(title);
    let norm_artist = normalize(artist);

    let mut best_match: Option<PathBuf> = None;
    let mut best_score: u32 = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Only match _qm.qrc files (main lyrics, not translation)
        if !file_name.ends_with("_qm.qrc") {
            continue;
        }

        // Parse filename: "Artist - Title - Duration - Album_qm.qrc"
        let base = file_name.trim_end_matches("_qm.qrc");
        let parts: Vec<&str> = base.splitn(4, " - ").collect();
        if parts.len() < 2 {
            continue;
        }

        let file_artist = normalize(parts[0]);
        let file_title = normalize(parts[1]);

        // Score matching using contains-based logic for robustness
        let mut score: u32 = 0;

        // Exact title match is highest priority
        if file_title == norm_title {
            score += 100;
        } else if file_title.contains(&norm_title) || norm_title.contains(&file_title) {
            score += 60;
        } else {
            continue; // title must at least partially match
        }

        // Artist matching
        if file_artist == norm_artist {
            score += 50;
        } else if file_artist.contains(&norm_artist) || norm_artist.contains(&file_artist) {
            score += 30;
        }

        // 弱匹配拒绝：标题仅子串命中且歌手零分（60 分）会匹配到错歌，
        // 例如"这，就是爱"命中"就是爱"的本地文件。低于阈值的候选一律跳过。
        if score < MIN_ACCEPT_SCORE {
            continue;
        }

        if score > best_score {
            best_score = score;
            best_match = Some(path);
        }
    }

    best_match
}

// Function to find the translation QRC file corresponding to a main QRC file
pub fn find_qrc_trans_file(qrc_file: &Path) -> Option<PathBuf> {
    let file_name = qrc_file.file_name()?.to_str()?;
    if !file_name.ends_with("_qm.qrc") {
        return None;
    }
    let trans_name = file_name.replace("_qm.qrc", "_qmts.qrc");
    let trans_path = qrc_file.with_file_name(trans_name);
    if trans_path.exists() {
        Some(trans_path)
    } else {
        None
    }
}

// Function to find the main LRC lyric file for a given song
pub fn find_lrc_file(cache_dir: &Path, title: &str, artist: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(cache_dir).ok()?;

    // Normalize search terms for fuzzy matching
    let norm_title = normalize(title);
    let norm_artist = normalize(artist);

    let mut best_match: Option<PathBuf> = None;
    let mut best_score: u32 = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Only match _qm.lrc files (main lyrics, not translation)
        if !file_name.ends_with("_qm.lrc") {
            continue;
        }

        // Parse filename: "Artist - Title - Duration - Album_qm.lrc"
        let base = file_name.trim_end_matches("_qm.lrc");
        let parts: Vec<&str> = base.splitn(4, " - ").collect();
        if parts.len() < 2 {
            continue;
        }

        let file_artist = normalize(parts[0]);
        let file_title = normalize(parts[1]);

        // Score matching using contains-based logic for robustness
        let mut score: u32 = 0;

        // Exact title match is highest priority
        if file_title == norm_title {
            score += 100;
        } else if file_title.contains(&norm_title) || norm_title.contains(&file_title) {
            score += 60;
        } else {
            continue; // title must at least partially match
        }

        // Artist matching
        if file_artist == norm_artist {
            score += 50;
        } else if file_artist.contains(&norm_artist) || norm_artist.contains(&file_artist) {
            score += 30;
        }

        // 弱匹配拒绝：与 find_qrc_file 相同的阈值，防止子串标题 + 歌手零分匹配错歌
        if score < MIN_ACCEPT_SCORE {
            continue;
        }

        if score > best_score {
            best_score = score;
            best_match = Some(path);
        }
    }

    best_match
}

// Function to find the translation LRC file corresponding to a main LRC file
pub fn find_lrc_trans_file(lrc_file: &Path) -> Option<PathBuf> {
    let file_name = lrc_file.file_name()?.to_str()?;
    if !file_name.ends_with("_qm.lrc") {
        return None;
    }
    let trans_name = file_name.replace("_qm.lrc", "_qmts.lrc");
    let trans_path = lrc_file.with_file_name(trans_name);
    if trans_path.exists() {
        Some(trans_path)
    } else {
        None
    }
}

// Helper to normalize strings for fuzzy comparison.
//
// 将所有非字母数字字符（含标点、括号、`_`/`/`/`\\`、`(feat.` 的点和括号等）
// 折叠为单个空格，解决 SMTC 报告 `Go Again (feat. ELYSA)` 与本地缓存文件名
// `Go Again (feat_ ELYSA)` 因标点差异导致 contains/equal 匹配双双失败的问题。
// CJK 字符属于 `is_alphanumeric`，故中日韩标题不受影响。
pub fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 从 QRC/LRC 文件名中解析出各字段。
/// 文件名格式：`Artist - Title - Duration - Album_qm.{qrc,lrc}`
/// 返回 `(artist, title, album)`；字段缺失时为空串。
pub fn parse_lyric_filename(file_name: &str) -> (String, String, String) {
    let base = file_name
        .trim_end_matches("_qm.qrc")
        .trim_end_matches("_qm.lrc");
    let parts: Vec<&str> = base.splitn(4, " - ").collect();
    let artist = parts.first().copied().unwrap_or("").to_string();
    let title = parts.get(1).copied().unwrap_or("").to_string();
    let album = parts.get(3).copied().unwrap_or("").to_string();
    (artist, title, album)
}

/// 在 QQ 音乐本地缓存目录中查找专辑封面图。
///
/// 本地封面文件名格式：`T002R{W}x{H}M000{album_mid}_{seq}.jpg`（专辑封面）
/// 或 `T062R{W}x{H}M000{mid}_{seq}.jpg`（无专辑歌曲的单曲封面，vs 字段 mid）。
/// 优先返回 `R500x500`（本地最高清），回退 `R150x150`。
///
/// @param picture_dir - `QQMusicPicture` 目录路径
/// @param album_mid   - 从在线 API 获取的专辑 mid
/// @return 本地封面文件的绝对路径，或 None
pub fn find_album_pic(picture_dir: &Path, album_mid: &str) -> Option<PathBuf> {
    if album_mid.is_empty() {
        return None;
    }

    let entries = std::fs::read_dir(picture_dir).ok()?;
    let prefixes_500 = [
        format!("T002R500x500M000{}", album_mid),
        format!("T062R500x500M000{}", album_mid),
    ];
    let prefixes_150 = [
        format!("T002R150x150M000{}", album_mid),
        format!("T062R150x150M000{}", album_mid),
    ];

    let mut pic_500: Option<PathBuf> = None;
    let mut pic_150: Option<PathBuf> = None;

    for entry in entries.flatten() {
        let file_name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !file_name.ends_with(".jpg") {
            continue;
        }
        if pic_500.is_none() && prefixes_500.iter().any(|p| file_name.starts_with(p)) {
            pic_500 = Some(entry.path());
        } else if pic_150.is_none() && prefixes_150.iter().any(|p| file_name.starts_with(p)) {
            pic_150 = Some(entry.path());
        }
    }

    // 同一 mid 可能有多张（不同 seq），取第一张即可
    pic_500.or(pic_150)
}

/// 从在线专辑图 URL 中提取 album_mid。
///
/// URL 格式：`https://y.gtimg.cn/music/photo_new/T002R800x800M000{album_mid}.jpg?max_age=...`
/// 提取 `M000` 和 `.jpg` 之间的部分。
pub fn extract_album_mid_from_url(url: &str) -> Option<String> {
    let marker = "M000";
    let mid_start = url.find(marker)?;
    let after_marker = &url[mid_start + marker.len()..];
    let end = after_marker.find(".jpg")?;
    Some(after_marker[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试：SMTC 报告 `Go Again (feat. ELYSA)` 与本地缓存文件名
    /// `Go Again (feat_ ELYSA)` 因标点差异（`.` vs `_`、括号）此前
    /// 在 `find_qrc_file` 的 title 比较中双双失败而被跳过。改进后的
    /// `normalize` 应将两者折叠为同一结果，从而命中本地 QRC。
    #[test]
    fn test_normalize_punctuation_insensitive() {
        assert_eq!(normalize("Go Again (feat. ELYSA)"), "go again feat elysa");
        assert_eq!(normalize("Go Again (feat_ ELYSA)"), "go again feat elysa");
        assert_eq!(
            normalize("Go Again (feat. ELYSA)"),
            normalize("Go Again (feat_ ELYSA)"),
            "SMTC title and cached filename must normalize equal"
        );

        // 多歌手分隔符 / Explicit 后缀
        assert_eq!(normalize("King CAAN/ELYSA"), "king caan elysa");
        assert_eq!(normalize("King CAAN_ELYSA"), "king caan elysa");
        assert_eq!(normalize("Normal No More (Explicit)"), "normal no more explicit");

        // CJK 标题不受影响
        assert_eq!(normalize("修炼爱情"), "修炼爱情");
        assert_eq!(normalize("삐딱하게 (Crooked) (狂放)"), "삐딱하게 crooked 狂放");
    }

    /// 文件名解析：`Artist - Title - Duration - Album_qm.{qrc,lrc}` 各字段正确切分。
    #[test]
    fn test_parse_lyric_filename() {
        let (artist, title, album) = parse_lyric_filename(
            "Glass Animals - The Other Side Of Paradise (Explicit) - 320 - How To Be A Human Being (Explicit)_qm.qrc"
        );
        assert_eq!(artist, "Glass Animals");
        assert_eq!(title, "The Other Side Of Paradise (Explicit)");
        assert_eq!(album, "How To Be A Human Being (Explicit)");

        // LRC 后缀
        let (a, t, al) = parse_lyric_filename("徐良 - 那时雨 - 200 - 不写完_qm.lrc");
        assert_eq!(a, "徐良");
        assert_eq!(t, "那时雨");
        assert_eq!(al, "不写完");

        // 缺少 album 段：返回空 album
        let (a, t, al) = parse_lyric_filename("TYSM - Normal No More_qm.qrc");
        assert_eq!(a, "TYSM");
        assert_eq!(t, "Normal No More");
        assert_eq!(al, "");
    }

    /// 回归测试：'张杰 - 这，就是爱' 不得匹配到 '蔡依林 - 就是爱'。
    ///
    /// 两者的标题 normalize 后呈子串关系（"这 就是爱".contains("就是爱")），
    /// 歌手完全不匹配。旧逻辑接受该 60 分弱匹配，导致在 QQ 音乐尚未写入
    /// 正确歌词缓存文件（滞后数秒）的窗口期内，把错歌歌词锁进内存缓存。
    /// 临时歌词目录：每次调用分配唯一目录名，避免并行测试互相冲突
    fn temp_lyric_dir(files: &[&str]) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "qqmusic_monitor_test_{}_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            files.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        dir
    }

    #[test]
    fn qrc_substring_title_with_wrong_artist_is_rejected() {
        let dir = temp_lyric_dir(&["蔡依林 - 就是爱 - 256 - 城堡_qm.qrc"]);
        assert_eq!(
            find_qrc_file(&dir, "这，就是爱", "张杰"),
            None,
            "歌手零分 + 标题子串的弱匹配必须被拒绝"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn qrc_exact_title_preferred_over_substring_wrong_artist() {
        let dir = temp_lyric_dir(&[
            "蔡依林 - 就是爱 - 256 - 城堡_qm.qrc",
            "张杰 - 这，就是爱 - 292 - 听！我们的歌_qm.qrc",
        ]);
        let matched = find_qrc_file(&dir, "这，就是爱", "张杰");
        assert!(matched.is_some());
        let name = matched.unwrap().file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("张杰"), "应命中正确文件，实际: {}", name);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lrc_substring_title_with_wrong_artist_is_rejected() {
        let dir = temp_lyric_dir(&["蔡依林 - 就是爱 - 256 - 城堡_qm.lrc"]);
        assert_eq!(
            find_lrc_file(&dir, "这，就是爱", "张杰"),
            None,
            "LRC 同样拒绝歌手零分的子串弱匹配"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_title_with_exact_artist_still_matches() {
        // 合法场景：标题子串 + 歌手完全一致（110 分）仍应命中
        let dir = temp_lyric_dir(&["Selena Gomez - Sober (feat_ J Balvin) - 200 - Revive_qm.qrc"]);
        let matched = find_qrc_file(&dir, "Sober", "Selena Gomez");
        assert!(matched.is_some(), "标题子串 + 歌手一致的合法匹配不应被拒绝");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn punctuation_folded_exact_title_matches() {
        // normalize 标点折叠后标题完全一致：仍应命中（如 Kaiju No. vs No_）
        let dir = temp_lyric_dir(&["OneRepublic - Nobody (from Kaiju No_ 8) - 153 - Nobody_qm.qrc"]);
        let matched = find_qrc_file(&dir, "Nobody (from Kaiju No. 8)", "OneRepublic");
        assert!(matched.is_some(), "标点折叠后的完全一致匹配不应被拒绝");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 无专辑歌曲的单曲封面：本地文件名以 T062 前缀存在时也应命中
    #[test]
    fn find_album_pic_matches_single_cover_t062() {
        let dir = std::env::temp_dir().join(format!("qqmusic_monitor_pic_t062_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("T062R500x500M000002PE0rX0FxNck.jpg"), b"x").unwrap();
        std::fs::write(dir.join("T062R150x150M000002PE0rX0FxNck_1.jpg"), b"x").unwrap();

        let matched = find_album_pic(&dir, "002PE0rX0FxNck");
        let name = matched.expect("T062 单曲封面应被找到").file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("T062R500x500"), "应优先 R500x500，实际: {}", name);

        // 专辑封面路径仍正常工作
        assert!(find_album_pic(&dir, "002JsVIJ31j7qR").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 仅 150x150 时回退返回 150（调用方应附带高清升级地址）
    #[test]
    fn find_album_pic_falls_back_to_150_only() {
        let dir = std::env::temp_dir().join(format!("qqmusic_monitor_pic_150_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("T002R150x150M000001rWGUJ3Gvypx_2.jpg"), b"x").unwrap();

        let matched = find_album_pic(&dir, "001rWGUJ3Gvypx")
            .expect("仅有 150x150 时应回退返回它（低清，由调用方附加高清地址）");
        let name = matched.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.contains("R150x150"), "应返回 150x150 文件，实际: {}", name);
        std::fs::remove_dir_all(&dir).ok();
    }
}

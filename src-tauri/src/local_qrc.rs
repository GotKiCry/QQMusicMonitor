use std::path::{Path, PathBuf};

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
/// 本地封面文件名格式：`T002R{W}x{H}M000{album_mid}_{seq}.jpg`
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
    let prefix_500 = format!("T002R500x500M000{}", album_mid);
    let prefix_150 = format!("T002R150x150M000{}", album_mid);

    let mut pic_500: Option<PathBuf> = None;
    let mut pic_150: Option<PathBuf> = None;

    for entry in entries.flatten() {
        let file_name = match entry.file_name().to_str() {
            Some(n) => n.to_string(),
            None => continue,
        };
        if file_name.starts_with(&prefix_500) && file_name.ends_with(".jpg") {
            pic_500 = Some(entry.path());
        } else if file_name.starts_with(&prefix_150) && file_name.ends_with(".jpg") {
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
}

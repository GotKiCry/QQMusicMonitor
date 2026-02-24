use std::path::{Path, PathBuf};

// Function to auto-detect QQ Music lyric cache directory
pub fn auto_detect_cache_dir() -> Option<PathBuf> {
    // Step 1: Try reading cache path from QQ Music config file
    if let Some(path) = detect_from_qq_music_config() {
        return Some(path);
    }

    // Step 2: Fallback - scan all drive letters for QQMusicCache
    for letter in b'C'..=b'Z' {
        let path = PathBuf::from(format!("{}:\\QQMusicCache\\QQMusicLyricNew", letter as char));
        if path.exists() && path.is_dir() {
            return Some(path);
        }
    }

    None
}

// Function to detect lyric cache dir from QQ Music's WebkitCachePath.ini config
fn detect_from_qq_music_config() -> Option<PathBuf> {
    // QQ Music stores cache path in %APPDATA%\Tencent\QQMusic\WebkitCachePath.ini
    // Format: [WebkitCache]
    //         Path=D:\QQMusicCache\WebkitCache
    // The parent of this path is the cache root, and lyrics are in QQMusicLyricNew subdir
    let appdata = std::env::var("APPDATA").ok()?;
    let ini_path = PathBuf::from(&appdata)
        .join("Tencent")
        .join("QQMusic")
        .join("WebkitCachePath.ini");

    let content = std::fs::read_to_string(&ini_path).ok()?;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(raw_path) = trimmed.strip_prefix("Path=") {
            let cache_root = PathBuf::from(raw_path).parent()?.to_path_buf();
            let lyric_dir = cache_root.join("QQMusicLyricNew");
            if lyric_dir.exists() && lyric_dir.is_dir() {
                return Some(lyric_dir);
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

// Helper to normalize strings for fuzzy comparison
fn normalize(s: &str) -> String {
    s.to_lowercase()
        .replace('_', " ")
        .replace('/', " ")
        .replace('\\', " ")
        .trim()
        .to_string()
}

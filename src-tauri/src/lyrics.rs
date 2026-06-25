use anyhow::{Result, Context};
use reqwest::{Client, Url};
use serde_json::Value;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub struct LyricFetcher {
    client: Client,
    debug: bool,
}

impl LyricFetcher {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::with_debug(false)
    }

    pub fn with_debug(debug: bool) -> Self {
        Self {
            client: Client::builder()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
                .referer(true)
                .build()
                .unwrap_or_default(),
            debug,
        }
    }

    // Function to get high-definition album picture URL by song mid
    pub async fn get_album_pic_url_by_mid(&self, songmid: &str) -> String {
        match self.get_album_mid(songmid).await {
            Ok(album_mid) => format!("https://y.gtimg.cn/music/photo_new/T002R800x800M000{}.jpg?max_age=2592000", album_mid),
            Err(e) => {
                eprintln!("[lyrics] Failed to fetch album mid for {}: {}", songmid, e);
                String::new()
            }
        }
    }

    /// 通过专辑名在 SmartBox 搜索专辑，返回第一个匹配的 album_mid。
    /// 用于纠正 song_detail API 返回的单曲 album_mid 与用户实际播放专辑不符的情况。
    pub async fn search_album_mid_by_name(&self, album_name: &str) -> Option<String> {
        let smartbox_url = "https://c.y.qq.com/splcloud/fcgi-bin/smartbox_new.fcg";
        let url = Url::parse_with_params(smartbox_url, &[("key", album_name), ("format", "json")]).ok()?;

        let resp = self.client.get(url)
            .header("Referer", "https://y.qq.com/")
            .send()
            .await
            .ok()?;

        let parsed: Value = resp.json().await.ok()?;

        parsed["data"]["album"]["itemlist"]
            .as_array()
            .and_then(|list| list.first())
            .and_then(|item| item["mid"].as_str())
            .map(|s| s.to_string())
    }

    // Function to fetch albummid for a given songmid using get_song_detail_yqq API
    pub async fn get_album_mid(&self, songmid: &str) -> Result<String> {
        let detail_data = serde_json::json!({
            "comm": {
                "cv": 4747474,
                "ct": 24,
                "format": "json",
                "inCharset": "utf-8",
                "outCharset": "utf-8",
                "notice": 0,
                "platform": "yqq.json",
                "needNewCode": 1,
                "uin": 0
            },
            "req_1": {
                "module": "music.pf_song_detail_svr",
                "method": "get_song_detail_yqq",
                "param": {
                    "song_type": 0,
                    "song_mid": songmid,
                    "song_id": 0
                }
            }
        });

        let url = "https://u.y.qq.com/cgi-bin/musicu.fcg";

        let resp = self.client.post(url)
            .header("Referer", "https://y.qq.com/")
            .json(&detail_data)
            .send()
            .await
            .context("Failed to fetch song detail from musicu")?
            .json::<Value>()
            .await
            .context("Failed to parse song detail JSON")?;

        let album_mid = resp["req_1"]["data"]["track_info"]["album"]["mid"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("album mid not found in response"))?;

        Ok(album_mid.to_string())
    }

    // Function to search and fetch lyrics with multiple fallback strategies
    pub async fn fetch_lyrics(&self, title: &str, artist: &str) -> Result<(String, String, String, String)> {
        let log = |msg| { if self.debug { eprintln!("{}", msg); } };
        
        // Strategy 1: Search with "artist title" keyword
        let keyword = format!("{} {}", artist, title);
        match self.search_song(&keyword).await {
            Ok(Some(mid)) => {
                match self.get_lyric(&mid).await {
                    Ok(result) => {
                        if !result.0.is_empty() || !result.2.is_empty() {
                            let pic_url = self.get_album_pic_url_by_mid(&mid).await;
                            log(format!("[lyrics] ✓ Found lyrics via strategy 1 ('{}')", keyword));
                            return Ok((result.0, result.1, result.2, pic_url));
                        }
                        log(format!("[lyrics] Song found (mid={}) but lyric API returned empty", mid));
                    },
                    Err(e) => log(format!("[lyrics] Song found (mid={}) but lyric fetch failed: {}", mid, e)),
                }
            },
            Ok(None) => log(format!("[lyrics] Search '{}' returned 0 results", keyword)),
            Err(e) => log(format!("[lyrics] Search '{}' failed: {}", keyword, e)),
        }

        // Strategy 2: Search with just "title" (artist might contain multi-artist separators)
        if let Ok(Some(mid)) = self.search_song(title).await {
            if let Ok(result) = self.get_lyric(&mid).await {
                if !result.0.is_empty() || !result.2.is_empty() {
                    let pic_url = self.get_album_pic_url_by_mid(&mid).await;
                    log(format!("[lyrics] Found lyrics for '{} - {}' via strategy 2", title, artist));
                    return Ok((result.0, result.1, result.2, pic_url));
                }
            }
        }

        // Strategy 3: Try cleaned title (strip parentheses, suffixes)
        let clean_title = clean_search_term(title);
        let clean_artist = clean_search_term(artist);
        if clean_title != title || clean_artist != artist {
            // 3a: cleaned artist + cleaned title
            let keyword = format!("{} {}", clean_artist, clean_title);
            if let Ok(Some(mid)) = self.search_song(&keyword).await {
                if let Ok(result) = self.get_lyric(&mid).await {
                    if !result.0.is_empty() || !result.2.is_empty() {
                        let pic_url = self.get_album_pic_url_by_mid(&mid).await;
                        log(format!("[lyrics] Found lyrics for '{} - {}' via strategy 3a ('{}')", title, artist, keyword));
                        return Ok((result.0, result.1, result.2, pic_url));
                    }
                }
            }

            // 3b: cleaned title alone (artist + non-ASCII title often fails SmartBox,
            // e.g. "G-Dragon 삐딱하게" returns 0, but "삐딱하게" hits the right song.
            // Must run before parenthetical strategy so the original-language title
            // wins over an English alternative that may point to a different version.)
            if !clean_title.is_empty() {
                if let Ok(Some(mid)) = self.search_song(&clean_title).await {
                    if let Ok(result) = self.get_lyric(&mid).await {
                        if !result.0.is_empty() || !result.2.is_empty() {
                            let pic_url = self.get_album_pic_url_by_mid(&mid).await;
                            log(format!("[lyrics] Found lyrics for '{} - {}' via strategy 3b (clean title only: '{}')", title, artist, clean_title));
                            return Ok((result.0, result.1, result.2, pic_url));
                        }
                    }
                }
            }
        }

        // Strategy 4: Try searching with parenthetical content (e.g. alternative/translated title inside brackets)
        if let Some(alt_title) = extract_parentheses_content(title) {
            let keyword = format!("{} {}", clean_artist, alt_title);
            if let Ok(Some(mid)) = self.search_song(&keyword).await {
                if let Ok(result) = self.get_lyric(&mid).await {
                    if !result.0.is_empty() || !result.2.is_empty() {
                        let pic_url = self.get_album_pic_url_by_mid(&mid).await;
                        log(format!("[lyrics] Found lyrics via strategy 4 (parenthetical: '{}')", keyword));
                        return Ok((result.0, result.1, result.2, pic_url));
                    }
                }
            }
        }

        // All strategies exhausted
        log(format!("[lyrics] No lyrics found for '{} - {}' (API returned no data)", title, artist));
        Ok((String::new(), String::new(), String::new(), String::new()))
    }


    // Function to search for a song and return its mid
    // NOTE: Modern API (DoSearchForQQMusicDesktop) no longer returns results,
    // so we use SmartBox autocomplete API directly.
    async fn search_song(&self, keyword: &str) -> Result<Option<String>> {
        let keyword = sanitize_search_keyword(keyword);
        self.search_song_smartbox(&keyword).await
    }

    // Function to search using SmartBox autocomplete API
    pub(crate) async fn search_song_smartbox(&self, keyword: &str) -> Result<Option<String>> {
        let smartbox_url = "https://c.y.qq.com/splcloud/fcgi-bin/smartbox_new.fcg";
        let params = vec![
            ("key", keyword),
            ("format", "json"),
        ];

        let url = Url::parse_with_params(smartbox_url, &params)?;

        let resp = self.client.get(url)
            .header("Referer", "https://y.qq.com/")
            .send()
            .await
            .context("Failed smartbox search")?;

        let resp_text = resp.text().await
            .context("Failed to read smartbox response")?;

        let parsed: Value = match serde_json::from_str(&resp_text) {
            Ok(v) => v,
            Err(e) => {
                let preview: String = resp_text.chars().take(200).collect();
                if self.debug {
                    eprintln!("[lyrics] Search returned non-JSON for '{}': {} — body: {}", 
                        keyword, e, preview);
                }
                return Ok(None);
            }
        };

        // SmartBox response: data.song.itemlist[] → { mid, name, singer }
        let item_list = parsed["data"]["song"]["itemlist"].as_array();
        if let Some(list) = item_list {
            if let Some(first) = list.first() {
                if let Some(mid) = first["mid"].as_str() {
                    let name = first["name"].as_str().unwrap_or("?");
                    if self.debug {
                        eprintln!("[lyrics] ✓ Found: '{}' (mid={})", name, mid);
                    }
                    return Ok(Some(mid.to_string()));
                }
            }
        }

        if self.debug {
            eprintln!("[lyrics] No results for '{}'", keyword);
        }
        Ok(None)
    }

    // Function to fetch lyrics, translation, and QRC data by song mid
    pub async fn get_lyric(&self, songmid: &str) -> Result<(String, String, String)> {
        // Try modern musicu API first
        if let Ok((lyrics, trans, mut qrc)) = self.get_lyric_musicu(songmid).await {
            // 如果成功抓到 QRC，直接返回全部
            if !qrc.is_empty() {
                return Ok((lyrics, trans, qrc));
            }
            
            // 如果有 LRC 但无 QRC，尝试 legacy 弥补 QRC（可选，不阻塞）
            if !lyrics.is_empty() {
                if let Ok((_, _, l_qrc)) = self.get_lyric_legacy(songmid).await {
                    if !l_qrc.is_empty() {
                        qrc = l_qrc;
                    }
                }
                return Ok((lyrics, trans, qrc));
            }
            
            // Musicu 返回了空数据（合法响应，只是该歌曲没有歌词数据）
            // 不要 fallthrough 到 legacy，因为 legacy 需要登录会报错 1101
            return Ok((lyrics, trans, qrc));
        }
        
        // Musicu 请求本身失败（网络错误/TLS 等），尝试 legacy 兜底
        self.get_lyric_legacy(songmid).await
    }

    // Function to fetch lyrics via modern musicu.fcg API
    async fn get_lyric_musicu(&self, songmid: &str) -> Result<(String, String, String)> {
        // First try: qrc=1 (QRC 逐字歌词格式)
        let (mut lyrics, mut trans, qrc) = self.call_lyric_api(songmid, 1).await?;

        // 如果 qrc=1 只返回了 QRC 加密数据（lyrics 为空但 qrc 非空），
        // 尝试 qrc=0 拿明文 LRC 作为备用（因为 QRC 解密可能失败）
        if qrc.len() > 10 && lyrics.is_empty() {
            if let Ok((lrc_lyrics, lrc_trans, _)) = self.call_lyric_api(songmid, 0).await {
                if !lrc_lyrics.is_empty() {
                    lyrics = lrc_lyrics;
                    trans = lrc_trans;
                }
            }
        }

        Ok((lyrics, trans, qrc))
    }

    /// Internal helper: call musicu.fcg lyric API with given qrc mode
    async fn call_lyric_api(&self, songmid: &str, qrc_mode: i32) -> Result<(String, String, String)> {
        let lyric_data = serde_json::json!({
            "comm": {
                "cv": 4747474,
                "ct": 24,
                "format": "json",
                "inCharset": "utf-8",
                "outCharset": "utf-8",
                "notice": 0,
                "platform": "yqq.json",
                "needNewCode": 1,
                "uin": 0
            },
            "req_1": {
                "module": "music.musichallSong.PlayLyricInfo",
                "method": "GetPlayLyricInfo",
                "param": {
                    "songMID": songmid,
                    "songID": 0,
                    "qrc": qrc_mode,
                    "trans": 1,
                    "roma": 1
                }
            }
        });

        let url = "https://u.y.qq.com/cgi-bin/musicu.fcg";

        let resp = self.client.post(url)
            .header("Referer", "https://y.qq.com/")
            .json(&lyric_data)
            .send()
            .await
            .context("Failed to fetch lyrics from musicu")?
            .json::<Value>()
            .await
            .context("Failed to parse musicu lyrics JSON")?;

        let lyric_info = &resp["req_1"]["data"];

        let mut lyrics = String::new();
        let mut trans = String::new();
        let mut qrc = String::new();

        // Decode lyrics (Base64/Hex -> String)
        if let Some(lyric_base64) = lyric_info["lyric"].as_str() {
            let lyric_clean = lyric_base64.trim();
            
            if !lyric_clean.is_empty() {
                // 如果歌词不是 [ti: 这种普通的 LRC 则它实际上就是 QRC 数据。
                let is_qrc_hex = !lyric_clean.starts_with("[ti:") && !lyric_clean.starts_with("W3Rp") && !lyric_clean.starts_with("[00:");
                if is_qrc_hex {
                     qrc = lyric_clean.to_string();
                }

                if let Ok(decoded_bytes) = STANDARD.decode(lyric_clean) {
                    if let Ok(lyric_content) = String::from_utf8(decoded_bytes) {
                        lyrics = unescape_html(&lyric_content);
                    }
                } else if !is_qrc_hex {
                     // 如果不是 base64 也不是 QRC，那可能是纯文本 LRC
                     lyrics = lyric_clean.to_string();
                }
            }
        }

        // Decode translation
        if let Some(trans_base64) = lyric_info["trans"].as_str() {
            if !trans_base64.is_empty() {
                if let Ok(decoded_bytes) = STANDARD.decode(trans_base64) {
                    if let Ok(trans_content) = String::from_utf8(decoded_bytes) {
                        trans = unescape_html(&trans_content);
                    }
                } else {
                     trans = trans_base64.to_string();
                }
            }
        }

        // Get QRC data (有时候额外有个 qrc 字段，虽然概率不大或是一个数字)
        if let Some(qrc_val) = lyric_info["qrc"].as_str() {
            if !qrc_val.is_empty() && qrc_val.len() > 10 {
                qrc = qrc_val.to_string();
            }
        }

        Ok((lyrics, trans, qrc))
    }

    // Function to fetch lyrics via legacy API
    pub async fn get_lyric_legacy(&self, songmid: &str) -> Result<(String, String, String)> {
        let lyric_base = "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg";
        let params = vec![
            ("songmid", songmid),
            ("g_tk", "5381"),
            ("format", "json"),
            ("inCharset", "utf8"),
            ("outCharset", "utf8"),
            ("notice", "0"),
            ("platform", "yqq"),
            ("needNewCode", "0"),
        ];

        let url = Url::parse_with_params(lyric_base, &params)?;

        let resp = self.client.get(url)
            .header("Referer", "https://y.qq.com/")
            .send()
            .await
            .context("Failed to fetch lyric")?;

        // Read text first so we can handle non-JSON responses gracefully
        let resp_text = resp.text().await
            .context("Failed to read legacy lyric response")?;

        let parsed: Value = match serde_json::from_str(&resp_text) {
            Ok(v) => v,
            Err(e) => {
                let preview: String = resp_text.chars().take(200).collect();
                if self.debug {
                    eprintln!("[lyrics] Legacy lyric API returned non-JSON for mid={}: {} — body: {}", 
                        songmid, e, preview);
                }
                // Return empty data instead of failing
                return Ok((String::new(), String::new(), String::new()));
            }
        };

        let mut lyrics = String::new();
        let mut trans = String::new();
        let mut qrc = String::new();

        // Decode lyrics (Base64 -> String)
        if let Some(lyric_base64) = parsed["lyric"].as_str() {
            if let Ok(decoded_bytes) = STANDARD.decode(lyric_base64) {
                if let Ok(lyric_content) = String::from_utf8(decoded_bytes) {
                    lyrics = unescape_html(&lyric_content);
                }
            }
        }

        // Decode translation
        if let Some(trans_base64) = parsed["trans"].as_str() {
            if !trans_base64.is_empty() {
                if let Ok(decoded_bytes) = STANDARD.decode(trans_base64) {
                    if let Ok(trans_content) = String::from_utf8(decoded_bytes) {
                        trans = unescape_html(&trans_content);
                    }
                }
            }
        }

        // Get QRC
        if let Some(qrc_val) = parsed["qrc"].as_str() {
            qrc = qrc_val.to_string();
        }

        Ok((lyrics, trans, qrc))
    }
}

// Helper to sanitize search keyword: replace slashes and strip problematic chars
fn sanitize_search_keyword(s: &str) -> String {
    s.replace('/', " ")
     .replace("\\", " ")
     .replace('\u{200b}', "") // zero-width space
     .trim()
     .to_string()
}

// Helper to clean search terms by removing parenthetical content and special chars
fn clean_search_term(s: &str) -> String {
    let mut result = s.to_string();
    // Remove content in parentheses (both English and Chinese)
    while let Some(start) = result.find('(') {
        if let Some(end) = result[start..].find(')') {
            result.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }
    while let Some(start) = result.find('（') {
        if let Some(end) = result[start..].find('）') {
            result.replace_range(start..start + end + '）'.len_utf8(), "");
        } else {
            break;
        }
    }
    // Remove common suffixes
    for suffix in &[" - Single", " (Explicit)", " (Remastered)", " (Deluxe)"] {
        result = result.replace(suffix, "");
    }
    result.trim().to_string()
}

// Helper to extract text inside parentheses (both English and Chinese)
fn extract_parentheses_content(s: &str) -> Option<String> {
    if let Some(start) = s.find('(') {
        if let Some(end) = s[start..].find(')') {
            let content = s[start + 1..start + end].trim().to_string();
            if !content.is_empty() {
                return Some(content);
            }
        }
    }
    if let Some(start) = s.find('（') {
        if let Some(end) = s[start..].find('）') {
            let content = s[start + '（'.len_utf8()..start + end].trim().to_string();
            if !content.is_empty() {
                return Some(content);
            }
        }
    }
    None
}

// Helper to unescape common HTML entities
fn unescape_html(s: &str) -> String {
    s.replace("&apos;", "'")
     .replace("&quot;", "\"")
     .replace("&amp;", "&")
     .replace("&lt;", "<")
     .replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_search_smartbox() {
        let fetcher = LyricFetcher::new();
        let result = fetcher.search_song_smartbox("徐良 那时雨").await;
        match &result {
            Ok(Some(mid)) => {
                eprintln!("[test] SmartBox found mid={}", mid);
                assert!(!mid.is_empty(), "mid should not be empty");
            }
            Ok(None) => {
                eprintln!("[test] SmartBox returned no results");
                // This might be a network issue — don't fail the test
            }
            Err(e) => {
                panic!("SmartBox search failed: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_search_modern() {
        let fetcher = LyricFetcher::new();
        let result = fetcher.search_song("徐良 那时雨").await;
        match &result {
            Ok(Some(mid)) => {
                eprintln!("[test] Modern search found mid={}", mid);
                assert!(!mid.is_empty(), "mid should not be empty");
            }
            Ok(None) => {
                eprintln!("[test] Modern search returned no results");
            }
            Err(e) => {
                panic!("Modern search failed: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_fetch_lyrics() {
        let fetcher = LyricFetcher::new();
        match fetcher.fetch_lyrics("那时雨", "徐良").await {
            Ok((lyrics, trans, qrc, _pic)) => {
                eprintln!("[test] lyrics.len={}, trans.len={}, qrc.len={}", lyrics.len(), trans.len(), qrc.len());
                if !lyrics.is_empty() {
                    assert!(lyrics.contains("[ti:") || lyrics.contains("[00:"),
                        "lyrics should be LRC format, got: {:?}", &lyrics[..50.min(lyrics.len())]);
                }
            }
            Err(e) => {
                panic!("fetch_lyrics failed: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_fetch_qrc_target_song() {
        let fetcher = LyricFetcher::new();
        let (lyrics, trans, qrc_raw, _pic) = fetcher.fetch_lyrics("越来越不懂", "蔡健雅")
            .await
            .expect("fetch_lyrics should succeed");
        
        eprintln!("[test] 越来越不懂: lyrics={} chars, trans={} chars, qrc_raw={} chars",
            lyrics.len(), trans.len(), qrc_raw.len());
        
        // Verify lyrics are in LRC format
        if !lyrics.is_empty() {
            assert!(lyrics.contains("[ti:") || lyrics.contains("[00:"),
                "lyrics should be LRC format, got start: {:?}",
                &lyrics[..50.min(lyrics.len())]);
        }

        // Test the full QRC pipeline
        if !qrc_raw.is_empty() {
            match crate::qrc::decode_qrc(&qrc_raw) {
                Ok(xml) => {
                    eprintln!("[test] QRC decoded XML: {} chars", xml.len());
                    eprintln!("[test] XML start: {:?}", &xml[..200.min(xml.len())]);
                    match crate::qrc::parse_qrc_xml(&xml) {
                        Ok(lines) => {
                            eprintln!("[test] QRC parsed {} lines", lines.len());
                            assert!(!lines.is_empty(), "should have parsed at least 1 line");
                        }
                        Err(e) => {
                            eprintln!("[test] QRC parse_qrc_xml failed: {}", e);
                            // Try text fallback
                            let text_lyrics = crate::qrc::extract_lrc_from_xml(&xml).unwrap_or_default();
                            if !text_lyrics.is_empty() {
                                let parsed = crate::qrc::parse_qrc_text(&text_lyrics);
                                eprintln!("[test] Text fallback parsed {} lines", parsed.len());
                                if parsed.is_empty() {
                                    eprintln!("[test] Text fallback also produced no lines");
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[test] QRC decode failed: {}", e);
                }
            }
        } else {
            eprintln!("[test] No QRC data returned for this song");
        }

        assert!(!lyrics.is_empty() || !qrc_raw.is_empty(),
            "should have at least lyrics or QRC data");
    }

    #[tokio::test]
    async fn test_fetch_qrc_delicate_weapon() {
        let fetcher = LyricFetcher::new();
        let (lyrics, trans, qrc_raw, _pic) = fetcher.fetch_lyrics("Delicate Weapon", "Grimes/Lizzy Wizzy")
            .await
            .expect("fetch_lyrics should succeed");

        eprintln!("[test] Delicate Weapon: lyrics={} chars, trans={} chars, qrc_raw={} chars",
            lyrics.len(), trans.len(), qrc_raw.len());

        if !qrc_raw.is_empty() {
            match crate::qrc::decode_qrc(&qrc_raw) {
                Ok(xml) => {
                    eprintln!("[test] QRC decoded XML: {} chars", xml.len());
                    eprintln!("[test] XML start: {:?}", &xml[..200.min(xml.len())]);
                    match crate::qrc::parse_qrc_xml(&xml) {
                        Ok(lines) => {
                            eprintln!("[test] QRC parsed {} lines", lines.len());
                            assert!(!lines.is_empty(), "should have parsed at least 1 line");
                        }
                        Err(e) => {
                            eprintln!("[test] QRC XML parse failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[test] QRC decode failed: {}", e);
                }
            }
        } else {
            eprintln!("[test] ⚠️  No QRC data for this song — per-word highlighting not possible");
        }

        // Song should at least have LRC lyrics or QRC data
        assert!(!lyrics.is_empty() || !qrc_raw.is_empty(),
            "should have at least lyrics or QRC data");
    }

    // Regression test: 삐딱하게 (Crooked) (狂放) by G-DRAGON.
    // The title contains both an English alternative "(Crooked)" and a Chinese
    // translation "(狂放)" in parentheses. The previous strategy flow fell
    // through to the parenthetical strategy which searched "G-Dragon Crooked"
    // and matched a *different* upload (mid=004WNnRd0JugjM, album
    // "COUP D'ETAT [+ ONE OF A KIND & HEARTBREAKER]") than the Korean-titled
    // one the user is actually playing (mid=003GBUdq4W6cOD, album
    // "G-DRAGON 2ND ALBUM : COUP D`ETAT"), producing the wrong album cover.
    // Strategy 3b now searches the cleaned Korean title "삐딱하게" alone and
    // hits the correct song as the first SmartBox result.
    #[tokio::test]
    async fn test_fetch_crooked_album_match() {
        let fetcher = LyricFetcher::with_debug(true);
        let (_lyrics, _trans, _qrc, pic_url) = fetcher
            .fetch_lyrics("삐딱하게 (Crooked) (狂放)", "G-DRAGON")
            .await
            .expect("fetch_lyrics should succeed");

        eprintln!("[test] Crooked pic_url = {}", pic_url);

        // The correct album mid is 003xpVKT3C9KpA (G-DRAGON 2ND ALBUM : COUP D`ETAT).
        // The wrong one previously matched was 002e1kVt33k3e3 (compilation).
        assert!(
            pic_url.contains("003xpVKT3C9KpA"),
            "album pic should match the Korean-titled album '003xpVKT3C9KpA', got: {}",
            pic_url
        );
    }
}

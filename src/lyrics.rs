use anyhow::{Result, Context};
use reqwest::{Client, Url};
use serde_json::Value;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub struct LyricFetcher {
    client: Client,
}

impl LyricFetcher {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
                .referer(true)
                .build()
                .unwrap_or_default(),
        }
    }

    // Function to search and fetch lyrics with multiple fallback strategies
    pub async fn fetch_lyrics(&self, title: &str, artist: &str) -> Result<(String, String, String)> {
        // Strategy 1: Search with "artist title" keyword
        let keyword = format!("{} {}", artist, title);
        if let Ok(Some(mid)) = self.search_song(&keyword).await {
            let result = self.get_lyric(&mid).await?;
            // 有时 API 只返回 QRC (result.2) 而不返回普通 LRC (result.0)，两者任一非空都算成功
            if !result.0.is_empty() || !result.2.is_empty() {
                return Ok(result);
            }
        }

        // Strategy 2: Search with just "title" (artist might contain multi-artist separators)
        if let Ok(Some(mid)) = self.search_song(title).await {
            let result = self.get_lyric(&mid).await?;
            if !result.0.is_empty() || !result.2.is_empty() {
                return Ok(result);
            }
        }

        // Strategy 3: Try cleaned title (strip parentheses, suffixes)
        let clean_title = clean_search_term(title);
        let clean_artist = clean_search_term(artist);
        if clean_title != title || clean_artist != artist {
            let keyword = format!("{} {}", clean_artist, clean_title);
            if let Ok(Some(mid)) = self.search_song(&keyword).await {
                let result = self.get_lyric(&mid).await?;
                if !result.0.is_empty() || !result.2.is_empty() {
                    return Ok(result);
                }
            }
        }

        // All strategies exhausted
        Ok((String::new(), String::new(), String::new()))
    }

    // Function to search for a song and return its mid
    async fn search_song(&self, keyword: &str) -> Result<Option<String>> {
        let search_data = serde_json::json!({
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
                "method": "DoSearchForQQMusicDesktop",
                "module": "music.search.SearchCgiService",
                "param": {
                    "search_type": 0,
                    "query": keyword,
                    "page_num": 1,
                    "num_per_page": 5
                }
            }
        });

        let url = "https://u.y.qq.com/cgi-bin/musicu.fcg";

        let resp = self.client.post(url)
            .json(&search_data)
            .send()
            .await
            .context("Failed to search song")?
            .json::<Value>()
            .await
            .context("Failed to parse search JSON")?;

        // Extract song list from response
        let song_list = resp["req_1"]["data"]["body"]["song"]["list"].as_array();
        
        if let Some(list) = song_list {
            if let Some(first_song) = list.first() {
                if let Some(mid) = first_song["mid"].as_str() {
                    return Ok(Some(mid.to_string()));
                }
            }
        }

        // Fallback: try legacy search API
        self.search_song_legacy(keyword).await
    }

    // Function to search using the legacy API as fallback
    async fn search_song_legacy(&self, keyword: &str) -> Result<Option<String>> {
        let search_base = "https://c.y.qq.com/soso/fcgi-bin/client_search_cp";
        let search_params = vec![
            ("w", keyword),
            ("p", "1"),
            ("n", "5"),
            ("format", "json"),
        ];

        let url = Url::parse_with_params(search_base, &search_params)?;

        let resp = self.client.get(url)
            .send()
            .await
            .context("Failed legacy search")?
            .json::<Value>()
            .await
            .context("Failed to parse legacy search JSON")?;

        let song_list = resp["data"]["song"]["list"].as_array();
        
        if let Some(list) = song_list {
            if let Some(first_song) = list.first() {
                if let Some(songmid) = first_song["songmid"].as_str() {
                    return Ok(Some(songmid.to_string()));
                }
            }
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
            
            // 如果没有获取到 QRC 但有 LRC，尝试使用 legacy api 弥补 QRC
            if !lyrics.is_empty() && qrc.is_empty() {
                if let Ok((_l_lyrics, _l_trans, l_qrc)) = self.get_lyric_legacy(songmid).await {
                    if !l_qrc.is_empty() {
                        qrc = l_qrc;
                    }
                }
                // 返回 (哪怕 QRC 还是空的，也要把 LRC 返回)
                return Ok((lyrics, trans, qrc));
            }
        }
        
        // If MusicU fails entirely, try legacy endpoint
        self.get_lyric_legacy(songmid).await
    }

    // Function to fetch lyrics via modern musicu.fcg API
    async fn get_lyric_musicu(&self, songmid: &str) -> Result<(String, String, String)> {
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
                    "qrc": 1,
                    "trans": 1,
                    "roma": 1
                }
            }
        });

        let url = "https://u.y.qq.com/cgi-bin/musicu.fcg";

        let resp = self.client.post(url)
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
                // 如果歌词解开后不是 [ti: 这种普通的 LRC 则它实际上就是 QRC 数据。
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
            .context("Failed to fetch lyric")?
            .json::<Value>()
            .await
            .context("Failed to parse lyric JSON")?;

        let mut lyrics = String::new();
        let mut trans = String::new();
        let mut qrc = String::new();

        // Decode lyrics (Base64 -> String)
        if let Some(lyric_base64) = resp["lyric"].as_str() {
            if let Ok(decoded_bytes) = STANDARD.decode(lyric_base64) {
                if let Ok(lyric_content) = String::from_utf8(decoded_bytes) {
                    lyrics = unescape_html(&lyric_content);
                }
            }
        }

        // Decode translation
        if let Some(trans_base64) = resp["trans"].as_str() {
            if !trans_base64.is_empty() {
                if let Ok(decoded_bytes) = STANDARD.decode(trans_base64) {
                    if let Ok(trans_content) = String::from_utf8(decoded_bytes) {
                        trans = unescape_html(&trans_content);
                    }
                }
            }
        }

        // Get QRC
        if let Some(qrc_val) = resp["qrc"].as_str() {
            qrc = qrc_val.to_string();
        }

        Ok((lyrics, trans, qrc))
    }
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

// Helper to unescape common HTML entities
fn unescape_html(s: &str) -> String {
    s.replace("&apos;", "'")
     .replace("&quot;", "\"")
     .replace("&amp;", "&")
     .replace("&lt;", "<")
     .replace("&gt;", ">")
}

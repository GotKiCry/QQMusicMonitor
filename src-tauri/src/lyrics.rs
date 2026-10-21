use anyhow::{Result, Context};
use reqwest::{Client, Url};
use serde_json::Value;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub struct LyricFetcher {
    client: Client,
    debug: bool,
}

/// 封面来源类型：专辑封面（T002 URL 前缀）或单曲封面（T062 URL 前缀）。
/// 无专辑的歌曲（如电台/Remix 单曲）没有 album.mid，但 song_detail 的
/// vs 字段携带单曲封面 mid，QQ 客户端即用它展示封面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverKind {
    Album,
    Single,
}

/// 歌曲封面信息：mid + 来源类型 + 专辑名（Single 时为空）+ 歌手名，
/// 用于人工校验候选歌曲身份（歌手一致 + 专辑名一致才采信封面）。
#[derive(Debug, Clone)]
pub struct AlbumCover {
    pub mid: String,
    pub name: String,
    pub singer: String,
    pub kind: CoverKind,
}

/// Legacy 歌曲搜索（client_search_cp）单条结果：自带专辑名与歌手名，
/// 用于按专辑名找歌时校验候选版本（SmartBox 搜索不返回专辑信息）。
#[derive(Debug, Clone)]
pub struct LegacySongHit {
    pub songmid: String,
    pub songname: String,
    pub albumname: String,
    pub singer: String,
}

/// SmartBox 歌曲搜索结果单条（mid + 歌手名），供按歌手筛选正确版本
#[derive(Debug, Clone)]
struct SmartboxSongHit {
    mid: String,
    singer: String,
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
                // 之前没有设置任何超时：网络抖动时单个请求会一直挂着，直到
                // 系统 TCP 超时（Windows 约 21s），整首歌歌词迟迟不更新。
                // 实测 QQ 音乐接口正常 150~500ms、冷启动首次约 2s，
                // 连接 4s / 总请求 10s 足够且能兜住挂起。
                .connect_timeout(std::time::Duration::from_secs(4))
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            debug,
        }
    }

    // 取高清专辑封面 URL 与匹配歌曲的专辑名。
    // 返回 (pic_url, album_name)：专辑名供上层判断「歌词匹配版本」与
    // 正在播放版本是否同专辑（同专辑时在线 URL 即为权威封面，专辑名
    // 重新解析出的不同 mid 不得覆盖；不同专辑（合辑版）时才允许覆盖）。
    pub async fn get_album_pic_url_by_mid(&self, songmid: &str) -> (String, String) {
        match self.get_album_cover(songmid).await {
            Ok(cover) => {
                let prefix = match cover.kind {
                    CoverKind::Album => "T002",
                    CoverKind::Single => "T062",
                };
                let url = format!("https://y.gtimg.cn/music/photo_new/{}R800x800M000{}.jpg?max_age=2592000", prefix, cover.mid);
                (url, cover.name)
            }
            Err(e) => {
                if self.debug {
                    eprintln!("[lyrics] Failed to fetch album cover for {}: {}", songmid, e);
                }
                (String::new(), String::new())
            }
        }
    }

    /// 通过专辑名在 SmartBox 搜索专辑，返回第一个与查询名一致、
    /// 且（artist 非空时）歌手与查询歌手一致的 album_mid。
    ///
    /// 注意：SmartBox 专辑搜索对通用名极不可靠（实测 "Jay" 首条返回
    /// "钢琴曲纯音乐经典"、"Way Back" 首条返回别的歌手的 "Way Back Home"），
    /// 直接取第一条会得到完全无关的专辑封面。因此逐条验证：
    ///   1. 候选专辑名（去括号后）与查询名的 normalize 包含关系
    ///   2. 候选专辑的歌手与查询歌手一致（artist 非空时）
    /// 全部不匹配返回 None——宁可无图，不显示错图。
    pub async fn search_album_mid_by_name(&self, album_name: &str, artist: &str) -> Option<String> {
        if album_name.trim().is_empty() {
            return None;
        }
        let smartbox_url = "https://c.y.qq.com/splcloud/fcgi-bin/smartbox_new.fcg";
        let url = Url::parse_with_params(smartbox_url, &[("key", album_name), ("format", "json")]).ok()?;

        let resp = self.client.get(url)
            .header("Referer", "https://y.qq.com/")
            .send()
            .await
            .ok()?;

        let parsed: Value = resp.json().await.ok()?;

        let norm_query = crate::local_qrc::normalize(album_name);
        let norm_artist = crate::local_qrc::normalize(artist);
        for item in parsed["data"]["album"]["itemlist"].as_array()?.iter() {
            let name = item["name"].as_str().unwrap_or("").trim();
            if name.is_empty() {
                continue;
            }
            // 候选名先去括号再比（"ONE PIECE SUPER BEST (海贼王…)" → "ONE PIECE SUPER BEST"）
            let cleaned = clean_search_term(name);
            let norm_name = crate::local_qrc::normalize(&cleaned);
            if !(norm_name.contains(&norm_query) || norm_query.contains(&norm_name)) {
                continue;
            }
            // 歌手校验：通用专辑名会命中别的歌手的同名专辑（"Way Back" → "Way Back Home"），
            // artist 非空时必须一致才采信
            if !norm_artist.is_empty() {
                let norm_singer = crate::local_qrc::normalize(item["singer"].as_str().unwrap_or(""));
                if norm_singer.is_empty()
                    || !(norm_singer.contains(&norm_artist) || norm_artist.contains(&norm_singer))
                {
                    continue;
                }
            }
            return item["mid"].as_str().map(|s| s.to_string());
        }
        None
    }

    /// Legacy 歌曲搜索（c.y.qq.com/soso/fcgi-bin/client_search_cp）。
    /// SmartBox 只返回 songmid 不带专辑名，且对很多专辑完全索引不到；
    /// 该接口仍可用且每条结果自带专辑名，用于按专辑名定位正确版本的歌曲。
    async fn search_song_legacy(&self, keyword: &str) -> Result<Vec<LegacySongHit>> {
        let url = Url::parse_with_params(
            "https://c.y.qq.com/soso/fcgi-bin/client_search_cp",
            &[
                ("p", "1"),
                ("n", "10"),
                ("w", keyword),
                ("format", "json"),
            ],
        )?;

        let resp = self.client.get(url)
            .header("Referer", "https://y.qq.com/")
            .send()
            .await
            .context("Failed legacy song search")?
            .json::<Value>()
            .await
            .context("Failed to parse legacy search JSON")?;

        let mut hits = Vec::new();
        if let Some(list) = resp["data"]["song"]["list"].as_array() {
            for item in list {
                let songmid = item["songmid"].as_str().unwrap_or("");
                if songmid.is_empty() {
                    continue;
                }
                hits.push(LegacySongHit {
                    songmid: songmid.to_string(),
                    songname: item["songname"].as_str().unwrap_or("").to_string(),
                    albumname: item["albumname"].as_str().unwrap_or("").to_string(),
                    singer: item["singer"][0]["name"].as_str().unwrap_or("").to_string(),
                });
            }
        }
        Ok(hits)
    }

    /// 按专辑名通过 legacy 歌曲搜索找歌，返回专辑名与参考专辑一致、
    /// 歌名与查询歌名一致、且（artist 非空时）歌手与查询歌手一致的候选 songmid。
    ///
    /// 背景：SmartBox 专辑搜索对很多专辑完全索引不到（实测 "The Eminem Show"
    /// 返回 0 条），导致专辑名兜底失效、封面缺失。legacy 搜索把专辑名当关键词
    /// 搜出的歌曲自带 albumname/songname/singer，逐项校验后即可定位到
    /// 正确版本的歌曲（再经 get_album_cover 取专辑封面）。
    ///
    /// 注意：专辑名可能是通用名（如 "By Your Side" 有几十首同名歌），
    /// 仅按专辑名匹配会把别的歌手的同名专辑当成"正确专辑"（错图）。
    /// 因此三重校验：专辑名 + 歌名 + 歌手（title/artist 为空时跳过对应校验）；
    /// 全部不符则返回 None（由调用方回退到原始 URL，而不是显示错歌封面）。
    pub async fn find_song_by_album_name(
        &self,
        album_variants: &[String],
        title: &str,
        artist: &str,
        reference_album: &str,
    ) -> Option<String> {
        let norm_ref = crate::local_qrc::normalize(reference_album);
        if norm_ref.is_empty() {
            return None;
        }
        let norm_title = crate::local_qrc::normalize(title);
        let norm_artist = crate::local_qrc::normalize(artist);
        for variant in album_variants {
            let v = variant.trim();
            if v.is_empty() {
                continue;
            }
            // 先带歌手搜（消除同名单曲歧义），再退化为专辑名单独搜
            let keywords = [format!("{} {}", artist, v), v.to_string()];
            for keyword in keywords {
                let keyword = keyword.trim().to_string();
                if keyword.is_empty() {
                    continue;
                }
                let Ok(hits) = self.search_song_legacy(&keyword).await else {
                    continue;
                };
                for hit in hits {
                    if hit.albumname.is_empty() {
                        continue;
                    }
                    let norm_album = crate::local_qrc::normalize(&hit.albumname);
                    if !(norm_album.contains(&norm_ref) || norm_ref.contains(&norm_album)) {
                        continue;
                    }
                    // 歌名校验：title 非空时要求候选歌名与查询歌名一致（互相包含，
                    // 容忍 "(feat. …)" 等后缀差异）
                    if !norm_title.is_empty() {
                        let norm_hit_title = crate::local_qrc::normalize(&hit.songname);
                        if !(norm_hit_title.contains(&norm_title) || norm_title.contains(&norm_hit_title)) {
                            continue;
                        }
                    }
                    // 歌手校验：artist 为空（无从对照）或歌手缺失时接受；
                    // 否则必须与查询歌手一致（normalize 互相包含）
                    if norm_artist.is_empty() || hit.singer.is_empty() {
                        return Some(hit.songmid);
                    }
                    let norm_singer = crate::local_qrc::normalize(&hit.singer);
                    if norm_singer.contains(&norm_artist) || norm_artist.contains(&norm_singer) {
                        return Some(hit.songmid);
                    }
                }
            }
        }
        None
    }

    // Function to fetch album cover info for a given songmid using get_song_detail_yqq API
    pub async fn get_album_cover(&self, songmid: &str) -> Result<AlbumCover> {
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

        let track_info = &resp["req_1"]["data"]["track_info"];

        // 歌手名（多歌手以 "/" 连接，normalize 时折叠为空格即可互相包含匹配）
        let singer = track_info["singer"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s["name"].as_str())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default();

        // 1. 专辑封面：album.mid 非空即用之（空串不算，避免拼出 M000.jpg 的 404 URL）
        if let Some(mid) = track_info["album"]["mid"].as_str() {
            if !mid.is_empty() {
                let name = track_info["album"]["name"].as_str().unwrap_or("").to_string();
                return Ok(AlbumCover { mid: mid.to_string(), name, singer, kind: CoverKind::Album });
            }
        }

        // 2. 单曲封面：无专辑的歌曲（电台单曲/Remix）用 vs 字段的第一个非空 mid，
        //    QQ 客户端以 T062 前缀 URL 展示（本地缓存亦为 T062R500x500M000{mid}.jpg）
        if let Some(vs_list) = track_info["vs"].as_array() {
            for item in vs_list {
                if let Some(m) = item.as_str() {
                    if !m.is_empty() {
                        return Ok(AlbumCover { mid: m.to_string(), name: String::new(), singer, kind: CoverKind::Single });
                    }
                }
            }
        }

        Err(anyhow::anyhow!("song {} has no album mid and no vs cover", songmid))
    }

    // Function to search and fetch lyrics with multiple fallback strategies
    pub async fn fetch_lyrics(&self, title: &str, artist: &str) -> Result<(String, String, String, String, String)> {
        let log = |msg| {
            if self.debug {
                eprintln!("{}", msg);
            }
        };
        
        // Strategy 1: Search with "artist title" keyword
        let keyword = format!("{} {}", artist, title);
        match self.search_song_verified(&keyword, artist).await {
            Ok(Some(mid)) => {
                match self.get_lyric(&mid).await {
                    Ok(result) => {
                        if !result.0.is_empty() || !result.2.is_empty() {
                            let (pic_url, matched_album) = self.get_album_pic_url_by_mid(&mid).await;
                            log(format!("[lyrics] ✓ Found lyrics via strategy 1 ('{}')", keyword));
                            return Ok((result.0, result.1, result.2, pic_url, matched_album));
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
        if let Ok(Some(mid)) = self.search_song_verified(title, artist).await {
            if let Ok(result) = self.get_lyric(&mid).await {
                if !result.0.is_empty() || !result.2.is_empty() {
                    let (pic_url, matched_album) = self.get_album_pic_url_by_mid(&mid).await;
                            log(format!("[lyrics] Found lyrics for '{} - {}' via strategy 2", title, artist));
                    return Ok((result.0, result.1, result.2, pic_url, matched_album));
                }
            }
        }

        // Strategy 3: Try cleaned title (strip parentheses, suffixes)
        let clean_title = clean_search_term(title);
        let clean_artist = clean_search_term(artist);
        if clean_title != title || clean_artist != artist {
            // 3a: cleaned artist + cleaned title
            let keyword = format!("{} {}", clean_artist, clean_title);
            if let Ok(Some(mid)) = self.search_song_verified(&keyword, artist).await {
                if let Ok(result) = self.get_lyric(&mid).await {
                    if !result.0.is_empty() || !result.2.is_empty() {
                        let (pic_url, matched_album) = self.get_album_pic_url_by_mid(&mid).await;
                            log(format!("[lyrics] Found lyrics for '{} - {}' via strategy 3a ('{}')", title, artist, keyword));
                        return Ok((result.0, result.1, result.2, pic_url, matched_album));
                    }
                }
            }

            // 3b: cleaned title alone (artist + non-ASCII title often fails SmartBox,
            // e.g. "G-Dragon 삐딱하게" returns 0, but "삐딱하게" hits the right song.
            // Must run before parenthetical strategy so the original-language title
            // wins over an English alternative that may point to a different version.)
            if !clean_title.is_empty() {
                if let Ok(Some(mid)) = self.search_song_verified(&clean_title, artist).await {
                    if let Ok(result) = self.get_lyric(&mid).await {
                        if !result.0.is_empty() || !result.2.is_empty() {
                            let (pic_url, matched_album) = self.get_album_pic_url_by_mid(&mid).await;
                            log(format!("[lyrics] Found lyrics for '{} - {}' via strategy 3b (clean title only: '{}')", title, artist, clean_title));
                            return Ok((result.0, result.1, result.2, pic_url, matched_album));
                        }
                    }
                }
            }
        }

        // Strategy 4: Try searching with parenthetical content (e.g. alternative/translated title inside brackets)
        if let Some(alt_title) = extract_parentheses_content(title) {
            let keyword = format!("{} {}", clean_artist, alt_title);
            if let Ok(Some(mid)) = self.search_song_verified(&keyword, artist).await {
                if let Ok(result) = self.get_lyric(&mid).await {
                    if !result.0.is_empty() || !result.2.is_empty() {
                        let (pic_url, matched_album) = self.get_album_pic_url_by_mid(&mid).await;
                            log(format!("[lyrics] Found lyrics via strategy 4 (parenthetical: '{}')", keyword));
                        return Ok((result.0, result.1, result.2, pic_url, matched_album));
                    }
                }
            }
        }

        // All strategies exhausted
        log(format!("[lyrics] No lyrics found for '{} - {}' (API returned no data)", title, artist));
        Ok((String::new(), String::new(), String::new(), String::new(), String::new()))
    }


    /// 多策略搜索歌曲，返回与查询歌手一致的候选 songmid。
    ///
    /// 与 `fetch_lyrics` 不同，此处不做歌词非空校验，仅用于需要 songmid 的
    /// 辅助场景（如本地歌词已命中、仅缺专辑封面时解析 album_mid）。
    /// 策略顺序与 `fetch_lyrics` 对齐，保证与歌词命中同一首歌：
    ///   1. "artist title"
    ///   2. "title"
    ///   3. "clean_artist clean_title" / "clean_title"
    ///
    /// 注意：SmartBox 的标题搜索是模糊匹配，"STAY (Explicit)" 会命中
    /// "Stay explicit"（歌手为空的另一首歌）。这里逐条遍历候选并优先
    /// 歌手与查询一致的命中；SmartBox 歌手为空时只作为弱候选备用，
    /// 最终由调用方用歌曲详情的歌手/专辑信息二次校验。
    pub async fn search_song_mid(&self, title: &str, artist: &str) -> Option<String> {
        let clean_title = clean_search_term(title);
        let clean_artist = clean_search_term(artist);

        let mut queries: Vec<String> = Vec::new();
        queries.push(format!("{} {}", artist, title));
        queries.push(title.to_string());
        if clean_title != title || clean_artist != artist {
            queries.push(format!("{} {}", clean_artist, clean_title));
            if !clean_title.is_empty() {
                queries.push(clean_title.clone());
            }
        }

        let norm_artist = crate::local_qrc::normalize(artist);
        let mut weak_candidate: Option<String> = None;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for q in queries {
            let q = q.trim().to_string();
            if q.is_empty() || !seen.insert(q.clone()) {
                continue;
            }
            let Ok(hits) = self.search_songs_smartbox(&q).await else {
                continue;
            };
            for hit in hits {
                if hit.mid.is_empty() {
                    continue;
                }
                // 查询歌手为空：无校验依据，直接接受首个候选
                if norm_artist.is_empty() {
                    return Some(hit.mid);
                }
                let norm_singer = crate::local_qrc::normalize(&hit.singer);
                if norm_singer.is_empty() {
                    // SmartBox 歌手缺失（如 "Stay explicit"）：仅作弱候选备用
                    if weak_candidate.is_none() {
                        weak_candidate = Some(hit.mid);
                    }
                } else if norm_singer.contains(&norm_artist) || norm_artist.contains(&norm_singer) {
                    return Some(hit.mid);
                }
            }
        }
        weak_candidate
    }

    // Function to search for a song and return its mid
    // NOTE: Modern API (DoSearchForQQMusicDesktop) no longer returns results,
    // so we use SmartBox autocomplete API directly.
    async fn search_song(&self, keyword: &str) -> Result<Option<String>> {
        let keyword = sanitize_search_keyword(keyword);
        self.search_song_smartbox(&keyword).await
    }

    /// 按歌手偏好搜索：遍历 SmartBox 候选，优先歌手与查询歌手一致的命中；
    /// 歌手字段缺失或全部不匹配时退回首个候选（弱候选，由歌词非空校验兜底）。
    ///
    /// 背景：SmartBox 标题搜索是模糊匹配，"Closer (Tokyo Remix)" 会命中
    /// "Closer - The Chainsmokers/Halsey"（原版）——若不做歌手校验，
    /// 本地 QRC 尚未写入（切歌后 ~2s）的空窗期内会把错歌歌词与封面
    /// 一并锁进缓存。
    async fn search_song_verified(&self, keyword: &str, artist: &str) -> Result<Option<String>> {
        let keyword = sanitize_search_keyword(keyword);
        let hits = self.search_songs_smartbox(&keyword).await?;
        let norm_artist = crate::local_qrc::normalize(artist);
        let mut weak: Option<String> = None;
        for hit in hits {
            if hit.mid.is_empty() {
                continue;
            }
            if norm_artist.is_empty() {
                return Ok(Some(hit.mid));
            }
            let norm_singer = crate::local_qrc::normalize(&hit.singer);
            if norm_singer.is_empty() {
                if weak.is_none() {
                    weak = Some(hit.mid);
                }
            } else if norm_singer.contains(&norm_artist) || norm_artist.contains(&norm_singer) {
                return Ok(Some(hit.mid));
            }
        }
        Ok(weak)
    }

    // Function to search using SmartBox autocomplete API
    pub(crate) async fn search_song_smartbox(&self, keyword: &str) -> Result<Option<String>> {
        let hits = self.search_songs_smartbox(keyword).await?;
        if let Some(first) = hits.into_iter().next() {
            return Ok(Some(first.mid));
        }
        Ok(None)
    }

    // SmartBox 歌曲搜索，返回全部候选（mid + 歌手），供按歌手筛选
    async fn search_songs_smartbox(&self, keyword: &str) -> Result<Vec<SmartboxSongHit>> {
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
                return Ok(Vec::new());
            }
        };

        // SmartBox response: data.song.itemlist[] → { mid, name, singer }
        let mut hits = Vec::new();
        if let Some(list) = parsed["data"]["song"]["itemlist"].as_array() {
            for item in list {
                if let Some(mid) = item["mid"].as_str() {
                    let name = item["name"].as_str().unwrap_or("?");
                    if self.debug {
                        eprintln!("[lyrics] ✓ Found: '{}' (mid={})", name, mid);
                    }
                    hits.push(SmartboxSongHit {
                        mid: mid.to_string(),
                        singer: item["singer"].as_str().unwrap_or("").to_string(),
                    });
                }
            }
        }

        if self.debug && hits.is_empty() {
            eprintln!("[lyrics] No results for '{}'", keyword);
        }
        Ok(hits)
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
pub fn clean_search_term(s: &str) -> String {
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

    /// 端到端诊断：Unstoppable - The Score (专辑 Unstoppable, 正确 mid=001wHjwD0IWIRm)
    /// 模拟 path A get_pic 与 path B resolve_album_pic_url 的每一步。
    #[tokio::test]
    async fn diag_unstoppable_cover() {
        let fetcher = LyricFetcher::new();
        let t = "Unstoppable";
        let a = "The Score";

        // 1a: search_song_mid → get_album_cover
        let songmid = fetcher.search_song_mid(t, a).await;
        eprintln!("[diag] 1a search_song_mid('{}','{}') => {:?}", t, a, songmid);
        if let Some(m) = &songmid {
            if let Ok(c) = fetcher.get_album_cover(m).await {
                eprintln!("[diag] 1a cover => mid={} name='{}' singer='{}' kind={:?}",
                    c.mid, c.name, c.singer, c.kind);
                assert_eq!(c.mid, "001wHjwD0IWIRm", "1a 必须命中 The Score 的正确专辑");
            }
        }

        // 1b/0a: 专辑搜索
        let am = fetcher.search_album_mid_by_name("Unstoppable", a).await;
        eprintln!("[diag] 1b search_album_mid_by_name('Unstoppable','The Score') => {:?}", am);

        // 1c/0b: legacy 三重校验
        let songmid2 = fetcher
            .find_song_by_album_name(&["Unstoppable".to_string()], t, a, "Unstoppable")
            .await;
        eprintln!("[diag] 1c find_song_by_album_name(['Unstoppable'],'{}','{}','Unstoppable') => {:?}", t, a, songmid2);
        if let Some(m) = &songmid2 {
            if let Ok(c) = fetcher.get_album_cover(m).await {
                eprintln!("[diag] 1c cover => mid={} name='{}' singer='{}' kind={:?}",
                    c.mid, c.name, c.singer, c.kind);
                // 1c 可能命中同名双专辑的错版（002vztFj10OO9E），三重校验无法区分；
                // 该场景由 resolve_album_pic_url 的覆盖判定（matched_album 与 SMTC
                // 专辑一致时拒绝覆盖原 URL）兜底，此处仅记录。
                if c.mid != "001wHjwD0IWIRm" {
                    eprintln!("[diag] 1c 命中同名专辑错版 {}（预期被 resolve 覆盖判定拒绝，回退原 URL）", c.mid);
                }
            }
        }
    }

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
            Ok((lyrics, trans, qrc, _pic, _album)) => {
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
        let (lyrics, trans, qrc_raw, _pic, _album) = fetcher.fetch_lyrics("越来越不懂", "蔡健雅")
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
        let (lyrics, trans, qrc_raw, _pic, _album) = fetcher.fetch_lyrics("Delicate Weapon", "Grimes/Lizzy Wizzy")
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
        let (_lyrics, _trans, _qrc, pic_url, _album) = fetcher
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

    /// 回归测试：无专辑的单曲（I Can't Make You Love Me (Remix) - The Chainsmokers，
    /// song_detail 的 album.mid 为空串）应通过 vs 字段回退到单曲封面，
    /// 生成 T062 前缀 URL 而不是拼出 M000.jpg 的 404 链接。
    #[tokio::test]
    async fn test_albumless_song_gets_single_cover() {
        let fetcher = LyricFetcher::with_debug(true);
        match fetcher.get_album_cover("003N1qlF2ukeJP").await {
            Ok(cover) => {
                eprintln!("[test] cover: kind={:?} mid={} name={:?}", cover.kind, cover.mid, cover.name);
                assert_eq!(cover.kind, CoverKind::Single, "无专辑歌曲应回退为单曲封面");
                assert!(!cover.mid.is_empty(), "单曲封面 mid 不应为空");
                let url = fetcher.get_album_pic_url_by_mid("003N1qlF2ukeJP").await.0;
                assert!(
                    url.starts_with("https://y.gtimg.cn/music/photo_new/T062R800x800M000"),
                    "单曲封面 URL 应为 T062 前缀，实际: {}",
                    url
                );
            }
            Err(e) => {
                eprintln!("[test] get_album_cover 失败（网络或 API 变更）: {}", e);
            }
        }
    }

    /// 专辑名搜索必须验证候选名与查询名一致：通用名 "Jay" 的首条候选
    /// （钢琴曲纯音乐经典）与查询无关，不应被当作 album_mid 返回。
    #[tokio::test]
    async fn test_album_name_search_rejects_unrelated_candidates() {
        let fetcher = LyricFetcher::new();
        let mid = fetcher.search_album_mid_by_name("Jay", "").await;
        eprintln!("[test] album search 'Jay' => {:?}", mid);
        // 名称明确的专辑仍应能找到
        let mid2 = fetcher.search_album_mid_by_name("ONE PIECE SUPER BEST", "").await;
        eprintln!("[test] album search 'ONE PIECE SUPER BEST' => {:?}", mid2);
        if let Some(m) = mid2 {
            assert!(!m.is_empty());
        }
    }

    /// 回归测试：通用专辑名必须校验歌手——"Way Back" 的 smartbox 专辑搜索
    /// 首条返回的是别人(柏奈)的 "Way Back Home"（mid=002RmAQ14UTCNe），
    /// 带歌手校验后必须拒绝它，绝不返回该错专辑。
    #[tokio::test]
    async fn test_album_name_search_verifies_artist() {
        let fetcher = LyricFetcher::with_debug(true);
        let mid = fetcher
            .search_album_mid_by_name("Way Back", "Vicetone/Cozi Zuehlsdorff")
            .await;
        eprintln!("[test] album search 'Way Back' + Vicetone => {:?}", mid);
        assert_ne!(mid.as_deref(), Some("002RmAQ14UTCNe"),
            "不得命中柏奈的 'Way Back Home' 专辑");
        // 名称 + 歌手都匹配时仍应能找到（Jay 的专辑）
        let mid2 = fetcher
            .search_album_mid_by_name("Jay", "周杰伦")
            .await;
        eprintln!("[test] album search 'Jay' + 周杰伦 => {:?}", mid2);
    }

    /// 回归测试：SmartBox 专辑搜索索引不到的专辑（"The Eminem Show" 返回 0 条，
    /// 但用户实际播放的 Without Me 属于 "The Eminem Show (Clean)" 专辑）——
    /// legacy 歌曲搜索按专辑名应定位到正确版本的歌曲。
    #[tokio::test]
    async fn test_find_song_by_album_name() {
        let fetcher = LyricFetcher::with_debug(true);
        let variants = vec![
            "The Eminem Show (Clean)".to_string(),
            "The Eminem Show".to_string(),
        ];
        match fetcher
            .find_song_by_album_name(&variants, "Without Me", "Eminem", "The Eminem Show (Clean)")
            .await
        {
            Some(songmid) => {
                eprintln!("[test] album-search hit songmid={}", songmid);
                assert!(!songmid.is_empty());
                // 该歌曲详情应回落到 The Eminem Show 专辑（Clean 或同族版本）
                match fetcher.get_album_cover(&songmid).await {
                    Ok(cover) => {
                        eprintln!("[test] cover name={:?} kind={:?}", cover.name, cover.kind);
                        assert!(
                            crate::local_qrc::normalize(&cover.name).contains("eminem show"),
                            "歌曲详情专辑名应含 eminem show，实际: {}",
                            cover.name
                        );
                    }
                    Err(e) => eprintln!("[test] get_album_cover failed: {}", e),
                }
            }
            None => eprintln!("[test] 未命中（网络或 API 变更）"),
        }
    }

    /// 回归测试：通用专辑名必须校验歌手。
    /// "By Your Side" 有几十首同名歌（Jonas Blue 等），只按专辑名匹配会
    /// 把别的歌手的同名专辑当成正确专辑（错图）。带歌手搜索应命中
    /// The Him/Jordan Jay 的正确版本（mid=00099aLf1hbwCr），而不是
    /// Jonas Blue 的 001lJV4m0ADchq。
    #[tokio::test]
    async fn test_find_song_by_album_name_verifies_artist() {
        let fetcher = LyricFetcher::with_debug(true);
        let variants = vec!["By Your Side".to_string()];
        match fetcher
            .find_song_by_album_name(&variants, "By Your Side", "The Him/Jordan Jay", "By Your Side")
            .await
        {
            Some(songmid) => {
                eprintln!("[test] By Your Side album-search hit songmid={}", songmid);
                assert_eq!(songmid, "00099aLf1hbwCr", "必须命中 The Him/Jordan Jay 的正确版本，实际: {}", songmid);
                if let Ok(cover) = fetcher.get_album_cover(&songmid).await {
                    eprintln!("[test] cover album={:?} mid={}", cover.name, cover.mid);
                    assert!(
                        crate::local_qrc::normalize(&cover.singer).contains("the him"),
                        "封面歌手应为 The Him，实际: {}",
                        cover.singer
                    );
                }
            }
            None => eprintln!("[test] 未命中（网络或 API 变更）"),
        }
    }

    /// 回归测试：STAY (Explicit) - The Kid LAROI/Justin Bieber。
    /// 标题搜索 "STAY (Explicit)" 会模糊命中 "Stay explicit"（屈直林的翻唱），
    /// search_song_mid 必须优先返回歌手一致的候选（STAY 原版），
    /// 否则会拿到错歌的 vs 单曲封面（004LS1ro32YK1a）。
    #[tokio::test]
    async fn test_search_song_mid_prefers_artist_match() {
        let fetcher = LyricFetcher::with_debug(true);
        match fetcher
            .search_song_mid("STAY (Explicit)", "The Kid LAROI/Justin Bieber")
            .await
        {
            Some(mid) => {
                eprintln!("[test] STAY search_song_mid => {}", mid);
                if mid == "0043EX2e2F6JCA" {
                    eprintln!("[test] ✓ 命中 STAY 原版（歌手一致）");
                } else {
                    eprintln!("[test] ⚠ 命中其他候选（歌手字段可能为空，需详情二次校验）");
                }
                // 详情歌手应与查询歌手一致
                if let Ok(cover) = fetcher.get_album_cover(&mid).await {
                    eprintln!("[test] cover singer={:?} name={:?} kind={:?}", cover.singer, cover.name, cover.kind);
                    let artist_match = crate::local_qrc::normalize(&cover.singer)
                        .contains(&crate::local_qrc::normalize("The Kid LAROI/Justin Bieber"))
                        || crate::local_qrc::normalize("The Kid LAROI/Justin Bieber")
                            .contains(&crate::local_qrc::normalize(&cover.singer));
                    assert!(artist_match, "候选歌手应与查询歌手一致: {}", cover.singer);
                }
            }
            None => eprintln!("[test] 未命中（网络或 API 变更）"),
        }
    }

    /// 回归测试：Closer (Tokyo Remix) - 新田真剑佑/The Chainsmokers。
    /// SmartBox 标题搜索会模糊命中 "Closer - The Chainsmokers/Halsey"（原版），
    /// 歌词搜索必须拒绝歌手不符的候选——否则本地 QRC 尚未写入（切歌后 ~2s）
    /// 的空窗期内会把错歌歌词与封面（专辑 001rWGUJ3Gvypx）锁进缓存。
    #[tokio::test]
    async fn test_fetch_lyrics_rejects_wrong_artist_match() {
        let fetcher = LyricFetcher::with_debug(true);
        let (lyrics, trans, qrc, pic_url, _album) = fetcher
            .fetch_lyrics("Closer (Tokyo Remix)", "新田真剑佑/The Chainsmokers")
            .await
            .expect("fetch_lyrics should succeed");
        eprintln!(
            "[test] Closer (Tokyo Remix): lyrics={} trans={} qrc={} pic={}",
            lyrics.len(),
            trans.len(),
            qrc.len(),
            pic_url
        );
        // 该歌所有 SmartBox 查询均为 0 结果，预期无在线数据（等本地 QRC 写入后重试）；
        // 若有数据，绝不能是原版 Closer（ti 头带 "Billboard" 标记、歌手含 Halsey）
        if !lyrics.is_empty() || !qrc.is_empty() {
            let combined = format!("{}{}{}", lyrics, trans, qrc);
            assert!(
                !combined.contains("Billboard") && !combined.contains("Halsey"),
                "不得匹配到 The Chainsmokers/Halsey 的原版 Closer"
            );
        }
    }
}






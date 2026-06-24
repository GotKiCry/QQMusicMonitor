use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
use windows::Media::Control::GlobalSystemMediaTransportControlsSession;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus;
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use crate::song_info::SongInfo;
use anyhow::{Result, Context};
use std::sync::OnceLock;
use tokio::sync::Mutex;

struct SmtcReaderInner {
    manager: GlobalSystemMediaTransportControlsSessionManager,
    current_session: Option<GlobalSystemMediaTransportControlsSession>,
    last_song_key: Option<String>,
    last_thumbnail_base64: String,
}

static SMTC_READER: OnceLock<Mutex<SmtcReaderInner>> = OnceLock::new();

// Function to check if the session is from QQ Music.
fn is_qqmusic_session(session: &GlobalSystemMediaTransportControlsSession) -> bool {
    if let Ok(id) = session.SourceAppUserModelId() {
        let app_id = id.to_string().to_lowercase();
        app_id.contains("qqmusic")
            || app_id.contains("xuanwo")
            || app_id == "qqmusic"
    } else {
        false
    }
}

// Helper to read thumbnail from IRandomAccessStreamReference and convert to base64 data URL
async fn read_thumbnail_base64(thumbnail_ref: IRandomAccessStreamReference) -> Result<String> {
    let stream = thumbnail_ref.OpenReadAsync()?.await?;
    let content_type = stream.ContentType()?.to_string();
    let size = stream.Size()? as u32;
    if size == 0 {
        return Ok(String::new());
    }
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.await?;
    let mut buffer = vec![0u8; size as usize];
    reader.ReadBytes(&mut buffer)?;
    let base64_str = STANDARD.encode(&buffer);
    Ok(format!("data:{};base64,{}", content_type, base64_str))
}

// Function to read media information from a session.
// Uses LastUpdatedTime to compute drift-corrected playback position,
// matching the same algorithm Windows uses for its volume flyout progress bar.
async fn read_session_info(session: &GlobalSystemMediaTransportControlsSession, fetch_thumbnail: bool) -> Result<SongInfo> {
    let media_properties = session.TryGetMediaPropertiesAsync()?.await.context("Failed to get media properties")?;
    let timeline_properties = session.GetTimelineProperties()?;

    let title = media_properties.Title()?.to_string();
    let artist = media_properties.Artist()?.to_string();
    let album = media_properties.AlbumTitle()?.to_string();

    let mut album_pic_url = String::new();
    if fetch_thumbnail {
        if let Ok(thumbnail_ref) = media_properties.Thumbnail() {
            tokio::task::spawn_local(async move {
                match read_thumbnail_base64(thumbnail_ref).await {
                    Ok(base64_img) => {
                        if let Some(reader_mutex) = SMTC_READER.get() {
                            let mut r = reader_mutex.lock().await;
                            r.last_thumbnail_base64 = base64_img;
                        }
                    }
                    Err(e) => {
                        eprintln!("[SMTC] Failed to read thumbnail: {:?}", e);
                    }
                }
            });
        }
    }

    if title.is_empty() {
        anyhow::bail!("empty title");
    }

    let position = timeline_properties.Position()?;
    let end_time = timeline_properties.EndTime()?;

    let raw_current_ms = (position.Duration / 10_000) as u64;
    let total_time_ms = (end_time.Duration / 10_000) as u64;
    let total_seconds = (end_time.Duration as f64) / 10_000_000.0;

    let is_playing = if let Ok(playback_info) = session.GetPlaybackInfo() {
        matches!(playback_info.PlaybackStatus(), Ok(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing))
    } else {
        false
    };

    // Correct the stale SMTC Position using LastUpdatedTime.
    // SMTC.Position is a snapshot captured at SMTC.LastUpdatedTime.
    // Real position = Position + (now - LastUpdatedTime) when playing.
    let corrected_current_ms = if is_playing {
        match timeline_properties.LastUpdatedTime() {
            Ok(last_updated) => {
                let last_updated_100ns = last_updated.UniversalTime;
                if last_updated_100ns > 0 {
                    // Convert current UTC to Windows FILETIME epoch (100-ns intervals since 1601-01-01)
                    let now_unix_ms = chrono::Utc::now().timestamp_millis();
                    // Offset between Windows FILETIME epoch and Unix epoch in milliseconds
                    const WINDOWS_EPOCH_OFFSET_MS: i64 = 11_644_473_600_000;
                    let now_windows_ms = now_unix_ms + WINDOWS_EPOCH_OFFSET_MS;
                    let last_updated_ms = last_updated_100ns / 10_000; // 100-ns units to ms

                    let elapsed_ms = (now_windows_ms - last_updated_ms).max(0) as u64;
                    // Clamp elapsed to 5 seconds to avoid huge jumps from stale timestamps
                    let clamped_elapsed = elapsed_ms.min(5000);
                    let corrected = raw_current_ms + clamped_elapsed;

                    // Clamp to total duration
                    if total_time_ms > 0 { corrected.min(total_time_ms) } else { corrected }
                } else {
                    raw_current_ms
                }
            }
            Err(_) => raw_current_ms,
        }
    } else {
        raw_current_ms
    };

    let corrected_seconds = corrected_current_ms as f64 / 1000.0;
    let progress = if total_seconds > 0.0 {
        (corrected_seconds / total_seconds) * 100.0
    } else {
        0.0
    };

    let server_ts = chrono::Utc::now().timestamp_millis() as u64;

    Ok(SongInfo {
        title,
        artist,
        album,
        lyrics: String::new(),
        trans: String::new(),
        qrc_raw: String::new(),
        qrc_data: Vec::new(),
        current_time: corrected_seconds as u64,
        total_time: total_seconds as u64,
        current_time_ms: corrected_current_ms,
        total_time_ms,
        progress_percent: progress as f32,
        is_playing,
        album_pic_url,
        server_ts,
    })
}

// Function to get current media info from Windows SMTC using cached session.
pub async fn get_current_media_info() -> Result<Option<SongInfo>> {
    // Lazily initialize the global SMTC reader
    let reader_mutex = match SMTC_READER.get() {
        Some(m) => m,
        None => {
            let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.await?;
            let inner = SmtcReaderInner {
                manager,
                current_session: None,
                last_song_key: None,
                last_thumbnail_base64: String::new(),
            };
            let _ = SMTC_READER.set(Mutex::new(inner));
            SMTC_READER.get().unwrap()
        }
    };

    let mut reader = reader_mutex.lock().await;

    // 1. Try reading from the cached session first (do NOT fetch thumbnail initially to save CPU)
    if let Some(session) = &reader.current_session {
        if let Ok(mut info) = read_session_info(session, false).await {
            let song_key = format!("{}|{}", info.title, info.artist);
            if let Some(last_key) = &reader.last_song_key {
                if song_key == *last_key {
                    // Cache hit: same song and session is valid.
                    // Populate with the cached thumbnail base64 and return immediately.
                    info.album_pic_url = reader.last_thumbnail_base64.clone();
                    return Ok(Some(info));
                }
            }
        }
        // Cache miss or read error: clear the cache and fall back to full scan
        reader.current_session = None;
        reader.last_song_key = None;
        reader.last_thumbnail_base64 = String::new();
    }

    // 2. Scan all active sessions for QQ Music
    let mut qqmusic_session = None;
    if let Ok(sessions) = reader.manager.GetSessions() {
        if let Ok(size) = sessions.Size() {
            for i in 0..size {
                if let Ok(session) = sessions.GetAt(i) {
                    if is_qqmusic_session(&session) {
                        qqmusic_session = Some(session);
                        break;
                    }
                }
            }
        }
    }

    let mut qqmusic_found = false;
    if let Some(session) = qqmusic_session {
        qqmusic_found = true;
        // Cache miss: must fetch thumbnail here exactly once
        if let Ok(info) = read_session_info(&session, true).await {
            reader.last_song_key = Some(format!("{}|{}", info.title, info.artist));
            reader.last_thumbnail_base64 = info.album_pic_url.clone();
            reader.current_session = Some(session);
            return Ok(Some(info));
        }
    }

    // If QQ Music session was found but read failed, do not downgrade to other media sources
    if qqmusic_found {
        return Ok(None);
    }

    // 3. Fallback: try default current session
    if let Ok(session) = reader.manager.GetCurrentSession() {
        // Cache miss: must fetch thumbnail here exactly once
        if let Ok(info) = read_session_info(&session, true).await {
            reader.last_song_key = Some(format!("{}|{}", info.title, info.artist));
            reader.last_thumbnail_base64 = info.album_pic_url.clone();
            reader.current_session = Some(session);
            return Ok(Some(info));
        }
    }

    Ok(None)
}

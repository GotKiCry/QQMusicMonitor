use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
use windows::Media::Control::GlobalSystemMediaTransportControlsSession;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus;
use crate::song_info::SongInfo;
use anyhow::{Result, Context};

/// 检查 SMTC 会话是否来自 QQ Music（通过 SourceAppUserModelId 识别）
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

/// 从 SMTC 会话读取媒体信息
async fn read_session_info(session: &GlobalSystemMediaTransportControlsSession) -> Result<SongInfo> {
    let media_properties = session.TryGetMediaPropertiesAsync()?.await.context("Failed to get media properties")?;
    let timeline_properties = session.GetTimelineProperties()?;

    let title = media_properties.Title()?.to_string();
    let artist = media_properties.Artist()?.to_string();
    let album = media_properties.AlbumTitle()?.to_string();

    if title.is_empty() {
        anyhow::bail!("empty title");
    }

    let position = timeline_properties.Position()?;
    let end_time = timeline_properties.EndTime()?;

    let current_time_ms = (position.Duration / 10_000) as u64;
    let total_time_ms = (end_time.Duration / 10_000) as u64;

    let current_seconds = (position.Duration as f64) / 10_000_000.0;
    let total_seconds = (end_time.Duration as f64) / 10_000_000.0;

    let progress = if total_seconds > 0.0 {
        (current_seconds / total_seconds) * 100.0
    } else {
        0.0
    };

    let is_playing = if let Ok(playback_info) = session.GetPlaybackInfo() {
        matches!(playback_info.PlaybackStatus(), Ok(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing))
    } else {
        false
    };

    Ok(SongInfo {
        title,
        artist,
        album,
        lyrics: String::new(),
        trans: String::new(),
        qrc_raw: String::new(),
        qrc_data: Vec::new(),
        current_time: current_seconds as u64,
        total_time: total_seconds as u64,
        current_time_ms,
        total_time_ms,
        progress_percent: progress as f32,
        is_playing,
    })
}

/// 从 Windows SMTC 获取 QQ Music 的媒体信息
///
/// 策略：
/// 1. 优先在所有会话中查找匹配 QQ Music AppUserModelId 的会话 → 独占使用（过滤其他音源干扰）
/// 2. 若未找到匹配（桌面版 QQ Music 可能无 AppUserModelId），降级到 GetCurrentSession()（原行为）
pub async fn get_current_media_info() -> Result<Option<SongInfo>> {
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.await?;

    let mut qqmusic_found = false;

    // 遍历所有活跃会话，优先找 QQ Music
    if let Ok(sessions) = manager.GetSessions() {
        if let Ok(size) = sessions.Size() {
            for i in 0..size {
                if let Ok(session) = sessions.GetAt(i) {
                    if is_qqmusic_session(&session) {
                        qqmusic_found = true;
                        if let Ok(info) = read_session_info(&session).await {
                            return Ok(Some(info));
                        }
                    }
                }
            }
        }
    }

    // 找到了 QQ Music 会话但读取出错 → 返回 None（不降级到其他音源）
    if qqmusic_found {
        return Ok(None);
    }

    // 未找到 QQ Music 标识的会话 → 降级到原行为（兼容桌面版 QQ Music）
    if let Ok(session) = manager.GetCurrentSession() {
        if let Ok(info) = read_session_info(&session).await {
            return Ok(Some(info));
        }
    }

    Ok(None)
}

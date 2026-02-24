use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
use crate::song_info::SongInfo;
use anyhow::{Result, Context};

/// 从 Windows SMTC 获取当前的媒体信息
pub async fn get_current_media_info() -> Result<Option<SongInfo>> {
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.await?;
    let session = manager.GetCurrentSession();

    if let Ok(session) = session {
        let media_properties = session.TryGetMediaPropertiesAsync()?.await.context("Failed to get media properties")?;
        let timeline_properties = session.GetTimelineProperties()?;

        let title = media_properties.Title()?.to_string();
        let artist = media_properties.Artist()?.to_string();
        let album = media_properties.AlbumTitle()?.to_string(); // 部分应用可能不提供 AlbumTitle

        // 如果没有标题，视为无效
        if title.is_empty() {
            return Ok(None);
        }

        let position = timeline_properties.Position()?;
        let end_time = timeline_properties.EndTime()?;
        
        // 生成 SongInfo
        // 注意：SMTC 的时间单位是 TimeSpan (100ns tick)，Windows crate 会自动转换为 std::time::Duration
        let current_seconds = (position.Duration as f64) / 10_000_000.0;
        let total_seconds = (end_time.Duration as f64) / 10_000_000.0;

        let progress = if total_seconds > 0.0 {
            (current_seconds / total_seconds) * 100.0
        } else {
            0.0
        };

        return Ok(Some(SongInfo {
            title,
            artist,
            album,
            lyrics: String::new(), // SMTC 不提供歌词
            trans: String::new(),
            qrc_raw: String::new(),
            qrc_data: Vec::new(),
            current_time: current_seconds as u64, // SongInfo 用的是 u64 秒
            total_time: total_seconds as u64,
            progress_percent: progress as f32,
        }));
    }

    Ok(None)
}

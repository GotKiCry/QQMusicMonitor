use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct QrcLine {
    pub content: String,
    pub start_time_ms: u64,
    pub duration_ms: u64,
    pub words: Vec<QrcWord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct QrcWord {
    pub content: String,
    pub start_time_ms: u64, // Absolute time relative to song start (XML and QRC-text parsers both output absolute values)
    pub duration_ms: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct SongInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub lyrics: String,
    pub trans: String, // 翻译歌词
    #[serde(skip)]
    pub qrc_raw: String, // 逐字歌词 (加密)
    pub qrc_data: Vec<QrcLine>, // 逐字歌词 (解析后)
    pub current_time: u64,        // 当前播放时间（秒）
    pub total_time: u64,          // 总时长（秒）
    pub current_time_ms: u64,     // 当前播放时间（毫秒），用于 QRC 精确逐字渲染
    pub total_time_ms: u64,       // 总时长（毫秒）
    pub progress_percent: f32,    // 进度百分比
    pub is_playing: bool,         // 当前是否正在播放
    pub album_pic_url: String,    // 专辑封面图片地址（data URI / 在线 URL / SMTC 缩略图）
    pub album_pic_hd_url: String, // 高清封面地址：当 album_pic_url 是低清本地图(150x150)时，
                                  // 此字段携带 800x800 在线 URL，前端预加载完成后平滑升级
    pub server_ts: u64,           // 后端采样时的绝对时间戳（毫秒）
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default, PartialEq)]
pub struct PlaybackProgress {
    pub current_time: u64,
    pub total_time: u64,
    pub current_time_ms: u64,
    pub total_time_ms: u64,
    pub progress_percent: f32,
    pub is_playing: bool,
    pub server_ts: u64,
}

impl From<&SongInfo> for PlaybackProgress {
    fn from(info: &SongInfo) -> Self {
        Self {
            current_time: info.current_time,
            total_time: info.total_time,
            current_time_ms: info.current_time_ms,
            total_time_ms: info.total_time_ms,
            progress_percent: info.progress_percent,
            is_playing: info.is_playing,
            server_ts: info.server_ts,
        }
    }
}

#[allow(dead_code)]
impl SongInfo {
    /// 检查歌曲信息是否有效（有标题）
    pub fn is_valid(&self) -> bool {
        !self.title.is_empty()
    }
    
    /// 格式化当前时间为 MM:SS 格式
    pub fn format_current_time(&self) -> String {
        format_time(self.current_time)
    }
    
    /// 格式化总时长为 MM:SS 格式
    pub fn format_total_time(&self) -> String {
        format_time(self.total_time)
    }
    
    /// 获取进度条字符串
    pub fn get_progress_bar(&self, width: usize) -> String {
        if self.total_time == 0 {
            " ".repeat(width)
        } else {
            let filled = (self.progress_percent / 100.0 * width as f32) as usize;
            let empty = width - filled;
            format!("{}{}", "█".repeat(filled), "░".repeat(empty))
        }
    }
}

/// 将秒数格式化为 MM:SS 格式
#[allow(dead_code)]
fn format_time(seconds: u64) -> String {
    let minutes = seconds / 60;
    let secs = seconds % 60;
    format!("{:02}:{:02}", minutes, secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_progress_contains_only_dynamic_state() {
        let info = SongInfo {
            title: "Song".to_string(),
            current_time: 12,
            total_time: 180,
            current_time_ms: 12_345,
            total_time_ms: 180_000,
            progress_percent: 6.858,
            is_playing: true,
            server_ts: 123,
            ..SongInfo::default()
        };

        let progress = PlaybackProgress::from(&info);
        assert_eq!(progress.current_time_ms, 12_345);
        assert_eq!(progress.total_time_ms, 180_000);
        assert!(progress.is_playing);
        assert_eq!(progress.server_ts, 123);
    }
}

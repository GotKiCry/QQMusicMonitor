use serde::Serialize;

#[derive(Debug, Serialize, Clone, Default, PartialEq)]
pub struct SongInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub lyrics: String,
    pub current_time: u32,        // 当前播放时间（秒）
    pub total_time: u32,          // 总时长（秒）
    pub progress_percent: f32,    // 进度百分比
}

impl SongInfo {
    /// 检查歌曲信息是否有效（有标题）
    pub fn is_valid(&self) -> bool {
        !self.title.is_empty()
    }
    
    /// 检查是否有完整的歌曲信息
    pub fn is_complete(&self) -> bool {
        !self.title.is_empty() && !self.artist.is_empty() && !self.album.is_empty()
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
fn format_time(seconds: u32) -> String {
    let minutes = seconds / 60;
    let secs = seconds % 60;
    format!("{:02}:{:02}", minutes, secs)
}

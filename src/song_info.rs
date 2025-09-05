use serde::Serialize;

#[derive(Debug, Serialize, Clone, Default, PartialEq)]
pub struct SongInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub lyrics: String,
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
}

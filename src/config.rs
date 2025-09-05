use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::fs;

/// 配置文件结构
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub memory_offsets: MemoryOffsets,
    pub settings: Settings,
}

/// 内存偏移量配置
#[derive(Debug, Deserialize, Clone)]
pub struct MemoryOffsets {
    pub song_name_offset: usize,
    pub song_singer_offset: usize,
    pub song_album_offset: usize,
    pub song_lyrics_offset: usize,
    pub current_time_offset: usize,
    pub total_time_offset: usize,
    pub song_name_chain: Vec<usize>,
    pub song_singer_chain: Vec<usize>,
    pub song_album_chain: Vec<usize>,
    pub song_lyrics_chain: Vec<usize>,
    pub current_time_chain: Vec<usize>,
    pub total_time_chain: Vec<usize>,
    pub title_offset: usize,
}

/// 程序设置配置
#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub update_interval_ms: u64,
    pub max_retries: u32,
    pub process_name: String,
    pub module_name: String,
    pub output_txt: bool,
    pub output_json: bool,
    pub txt_filename: String,
    pub json_filename: String,
    pub debug_mode: bool,
    pub max_string_length: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            memory_offsets: MemoryOffsets {
                song_name_offset: 0x0002B2C,
                song_singer_offset: 0x0002B2C,
                song_album_offset: 0x0002B2C,
                song_lyrics_offset: 0x0002B2C,
                current_time_offset: 0x0002B2C,
                total_time_offset: 0x0002B2C,
                song_name_chain: vec![0x0],
                song_singer_chain: vec![0x0],
                song_album_chain: vec![0x0],
                song_lyrics_chain: vec![0x0],
                current_time_chain: vec![0x0],
                total_time_chain: vec![0x0],
                title_offset: 0x90C,
            },
            settings: Settings {
                update_interval_ms: 500,
                max_retries: 3,
                process_name: "QQMusic.exe".to_string(),
                module_name: "QQMusic.dll".to_string(),
                output_txt: true,
                output_json: true,
                txt_filename: "now_playing.txt".to_string(),
                json_filename: "now_playing.json".to_string(),
                debug_mode: false,
                max_string_length: 4096,
            },
        }
    }
}

impl Config {
    /// 从文件加载配置
    pub fn load_from_file(path: &str) -> Result<Self> {
        let config_content = fs::read_to_string(path)
            .map_err(|e| anyhow!("无法读取配置文件 {}: {}", path, e))?;
        
        let config: Config = toml::from_str(&config_content)
            .map_err(|e| anyhow!("解析配置文件失败: {}", e))?;
        
        Ok(config)
    }

    /// 获取配置实例（优先从文件加载，失败则使用默认配置）
    pub fn get_config() -> Self {
        match Self::load_from_file("config.toml") {
            Ok(config) => {
                if config.settings.debug_mode {
                    println!("✅ 成功加载配置文件");
                }
                config
            }
            Err(e) => {
                if Self::default().settings.debug_mode {
                    println!("⚠️  无法加载配置文件: {}", e);
                    println!("⚠️  使用默认配置");
                }
                Self::default()
            }
        }
    }
}

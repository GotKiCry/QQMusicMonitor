use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::fs;

/// 配置文件结构
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub settings: Settings,
}

/// 程序设置配置
#[derive(Debug, Deserialize, Clone)]
pub struct Settings {
    pub update_interval_ms: u64,
    pub max_retries: u32,
    pub output_txt: bool,
    pub output_json: bool,
    pub txt_filename: String,
    pub json_filename: String,
    pub output_lyric: bool,
    pub lyric_filename: String,
    pub debug_mode: bool,
    #[serde(default)]
    pub qrc_cache_dir: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            settings: Settings {
                update_interval_ms: 1000,
                max_retries: 3,
                output_txt: false,
                output_json: true,
                txt_filename: "now_playing.txt".to_string(),
                json_filename: "now_playing.json".to_string(),
                output_lyric: true,
                lyric_filename: "current_lyric.txt".to_string(),
                debug_mode: false,
                qrc_cache_dir: None,
            },
        }
    }
}

impl Config {
    /// 从文件加载配置
    pub fn load_from_file(path: &str) -> Result<Self> {
        let config_content = fs::read_to_string(path)
            .map_err(|e| anyhow!("无法读取配置文件 {}: {}", path, e))?;
        
        // 兼容旧配置：如果包含 extra fields (如 memory_offsets)，toml crate 默认会忽略它们
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
                // 如果是默认配置模式且开启了debug（通常得看 args，这里简化处理）
                // 暂时只在加载失败时静默或报错，CLI 会再次覆盖 debug_mode
                if Self::default().settings.debug_mode {
                    println!("⚠️  无法加载配置文件: {}", e);
                    println!("⚠️  使用默认配置");
                }
                Self::default()
            }
        }
    }
}

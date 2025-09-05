use anyhow::Result;
use std::fs::File;
use std::io::{Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use widestring::U16String;
use crossterm::{execute, terminal, cursor};
use regex::Regex;

mod cli;
mod config;
mod memory;
mod process;
mod song_info;

use cli::Cli;
use config::Config;
use song_info::SongInfo;


fn main() -> Result<()> {
    // 解析命令行参数
    let args = Cli::parse_args();
    
    // 处理帮助和版本信息
    if args.help {
        Cli::show_help();
        return Ok(());
    }
    
    if args.version {
        Cli::show_version();
        return Ok(());
    }

    // 加载配置
    let config = Config::get_config();
    
    // 应用命令行参数覆盖配置
    let config = apply_cli_overrides(config, &args);

    // 设置Ctrl+C处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;
    
    if !args.quiet && config.settings.debug_mode {
        println!("🎵 QQMusic Reader v{}", env!("CARGO_PKG_VERSION"));
        println!("========================================");
        println!("⚠️  请确保以管理员权限运行！");
        println!("========================================\n");
    }

    if config.settings.debug_mode && !args.quiet {
        println!("🔍 Searching for {}...", config.settings.process_name);
    }

    // 1. 查找进程
    let pid = process::get_pid_by_name(&config.settings.process_name)?;

    if config.settings.debug_mode && !args.quiet {
        println!("{} found with PID: {}", config.settings.process_name, pid);
    }

    let handle = process::get_process_handle(pid)?;

    let dll_base_address = process::get_module_base_address(pid, &config.settings.module_name)?;

    if config.settings.debug_mode && !args.quiet {
        println!(
            "Module {} found at address: {:#X}",
            config.settings.module_name, dll_base_address
        );
        println!("Start reading song info...");
    }

    // 初始化终端
    if !args.quiet {
        execute!(stdout(), terminal::Clear(terminal::ClearType::All), cursor::MoveTo(0, 0))?;
        execute!(stdout(), cursor::Hide)?;
    }

    let mut last_song_info: Option<SongInfo> = None;
    let mut update_count = 0;

    // 主循环
    while running.load(Ordering::SeqCst) {
        // 读取内存
        let current_song_info = match memory::read_song_info(handle, dll_base_address, &config) {
            Ok(info) => {
                if info.is_valid() {
                    info
                } else {
                    SongInfo { 
                        title: "No music playing".to_string(),
                        artist: String::new(),
                        album: String::new(),
                        lyrics: String::new(),
                        current_time: 0,
                        total_time: 0,
                        progress_percent: 0.0,
                    }
                }
            },
            Err(e) => {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error reading song info: {}", e);
                }
                SongInfo { 
                    title: "ERROR".to_string(),
                    artist: String::new(),
                    album: String::new(),
                    lyrics: String::new(),
                    current_time: 0,
                    total_time: 0,
                    progress_percent: 0.0,
                }
            }
        };

        // 检查歌曲是否有变化
        let song_changed = match &last_song_info {
            Some(last) => last.title != current_song_info.title || last.artist != current_song_info.artist,
            None => true,
        };

        // 写入文件（仅在歌曲变化时）
        if song_changed {
            if config.settings.output_txt {
                if let Err(e) = write_info_to_txt(&current_song_info, &config.settings.txt_filename) {
                    if config.settings.debug_mode && !args.quiet {
                        eprintln!("Error writing to txt file: {}", e);
                    }
                }
            }
            
            if config.settings.output_json {
                if let Err(e) = write_info_to_json(&current_song_info, &config.settings.json_filename) {
                    if config.settings.debug_mode && !args.quiet {
                        eprintln!("Error writing to json file: {}", e);
                    }
                }
            }
        }

        // 更新控制台显示
        if !args.quiet {
            execute!(stdout(), cursor::MoveTo(0, 0))?;
            
            // 显示歌曲信息
            if current_song_info.is_valid() && current_song_info.title != "No music playing" {
                // 处理空数据，用<参数名>替代
                let title = if current_song_info.title.is_empty() { "<歌曲名>" } else { &current_song_info.title };
                let artist = if current_song_info.artist.is_empty() { "<歌手>" } else { &current_song_info.artist };
                let album = if current_song_info.album.is_empty() { "<专辑>" } else { &current_song_info.album };
                
                // 第一行：歌曲名-歌手
                let line1 = format!("{}-{}", title, artist);
                println!("{:<80}", line1);
                
                // 第二行：专辑信息
                let album_line = format!("专辑:{}", album);
                println!("{:<80}", album_line);
                
                // 第三行：进度条和时间信息
                if current_song_info.total_time > 0 {
                    let progress_bar = current_song_info.get_progress_bar(20);
                    let time_info = format!("{} / {} [{:.1}%]", 
                        current_song_info.format_current_time(),
                        current_song_info.format_total_time(),
                        current_song_info.progress_percent);
                    println!("{:<80}", format!("{} {}", progress_bar, time_info));
                } else {
                    println!("{:<80}", format!("{}", chrono::Local::now().format("%H:%M:%S")));
                }
                
                // 显示歌词（如果有，过滤网址）
                if !current_song_info.lyrics.is_empty() {
                    let filtered_lyrics = filter_lyrics(&current_song_info.lyrics);
                    if !filtered_lyrics.is_empty() {
                        // 歌词可能有多行，逐行处理
                        for line in filtered_lyrics.lines() {
                            if !line.trim().is_empty() {
                                println!("{:<80}", line); // 每行都填充到80字符
                            }
                        }
                    }
                }
            } else {
                println!("{:<80}", "<歌曲名>-<歌手>");
                println!("{:<80}", "<专辑>");
                println!("{:<80}", format!("{}", chrono::Local::now().format("%H:%M:%S")));
                println!("{:<80}", "<歌词>");
            }
            
            // 调试模式下显示详细信息
            if config.settings.debug_mode {
                println!("{}", "─".repeat(80));
                println!("进程: {} | PID: {}", config.settings.process_name, pid);
                println!("更新: {} | 间隔: {}ms", update_count, config.settings.update_interval_ms);
                println!("歌曲变化: {}", song_changed);
            }
            
            // 清除剩余的行（防止歌词变短时的残留）
            execute!(stdout(), terminal::Clear(terminal::ClearType::FromCursorDown))?;
        }

        last_song_info = Some(current_song_info);
        update_count += 1;
        
        // 等待下一次更新
        thread::sleep(Duration::from_millis(config.settings.update_interval_ms));
    }

    // 恢复终端状态
    if !args.quiet && config.settings.debug_mode {
        execute!(stdout(), cursor::Show)?;
        execute!(stdout(), terminal::Clear(terminal::ClearType::All))?;
        println!("程序已退出");
    }

    unsafe { winapi::um::handleapi::CloseHandle(handle) };
    Ok(())
}

/// 应用命令行参数覆盖配置
fn apply_cli_overrides(mut config: Config, args: &Cli) -> Config {
    if args.debug {
        config.settings.debug_mode = true;
    }
    
    if args.no_txt {
        config.settings.output_txt = false;
    }
    
    if args.no_json {
        config.settings.output_json = false;
    }
    
    if args.txt_file != "now_playing.txt" {
        config.settings.txt_filename = args.txt_file.clone();
    }
    
    if args.json_file != "now_playing.json" {
        config.settings.json_filename = args.json_file.clone();
    }
    
    if args.interval != 500 {
        config.settings.update_interval_ms = args.interval;
    }
    
    if args.retries != 3 {
        config.settings.max_retries = args.retries;
    }
    
    if args.process != "QQMusic.exe" {
        config.settings.process_name = args.process.clone();
    }
    
    if args.module != "QQMusic.dll" {
        config.settings.module_name = args.module.clone();
    }
    
    config
}

/// 将歌曲信息写入文本文件 (UTF-16 LE)
fn write_info_to_txt(info: &SongInfo, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let u16_string = U16String::from_str(&info.title);
    let bytes: Vec<u8> = u16_string.into_vec().into_iter().flat_map(|c| c.to_le_bytes().to_vec()).collect();
    file.write_all(&bytes)?;
    Ok(())
}

/// 过滤歌词，移除网址和无用信息
fn filter_lyrics(lyrics: &str) -> String {
    let url_regex = Regex::new(r"https?://[^\s]+").unwrap_or_else(|_| Regex::new(r"").unwrap());
    let wma_regex = Regex::new(r"\.wma").unwrap_or_else(|_| Regex::new(r"").unwrap());
    let qqmusic_regex = Regex::new(r"qqmusic\.qq\.com").unwrap_or_else(|_| Regex::new(r"").unwrap());
    
    let filtered = url_regex.replace_all(lyrics, "");
    let filtered = wma_regex.replace_all(&filtered, "");
    let filtered = qqmusic_regex.replace_all(&filtered, "");
    
    filtered.trim().to_string()
}

/// 将歌曲信息写入JSON文件 (UTF-16 LE)
fn write_info_to_json(info: &SongInfo, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let json_string = format!("{{ \"title\": \"{}\" }}", info.title);
    let u16_string = U16String::from_str(&json_string);
    let bytes: Vec<u8> = u16_string.into_vec().into_iter().flat_map(|c| c.to_le_bytes().to_vec()).collect();
    file.write_all(&bytes)?;
    Ok(())
}

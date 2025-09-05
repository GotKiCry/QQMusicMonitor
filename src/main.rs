use anyhow::Result;
use std::fs::File;
use std::io::Write;
use widestring::U16String;

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

    if !args.quiet {
        println!("🎵 QQMusic Reader v{}", env!("CARGO_PKG_VERSION"));
        println!("========================================");
        println!("⚠️  请确保以管理员权限运行！");
        println!("========================================\n");
    }

    if !args.quiet {
        println!("🔍 Searching for {}...", config.settings.process_name);
    }

    // 1. 查找进程
    let pid = process::get_pid_by_name(&config.settings.process_name)?;

    if !args.quiet {
        println!("{} found with PID: {}", config.settings.process_name, pid);
    }

    let handle = process::get_process_handle(pid)?;

    let dll_base_address = process::get_module_base_address(pid, &config.settings.module_name)?;

    if !args.quiet {
        println!(
            "Module {} found at address: {:#X}",
            config.settings.module_name, dll_base_address
        );
        println!("Start reading song info...");
    }

    // 读取内存
    let current_song_info = match memory::read_song_info(handle, dll_base_address, &config) {
        Ok(info) => {
            if info.is_valid() {
                if !args.quiet {
                    println!("🎵 当前播放: {}-{} | {}", info.title,info.artist,info.album);
                }
                info
            } else {
                if !args.quiet {
                    println!("⏸️  No music playing or song title not found.");
                }
                SongInfo { 
                    title: "ERROR".to_string(),
                    artist: String::new(),
                    album: String::new(),
                    lyrics: String::new(),
                }
            }
        },
        Err(e) => {
            if !args.quiet {
                eprintln!("❌ Error reading song info: {}", e);
            }
            SongInfo { 
                title: "ERROR".to_string(),
                artist: String::new(),
                album: String::new(),
                lyrics: String::new(),
            }
        }
    };

    // 写入文件
    if config.settings.output_txt {
        if let Err(e) = write_info_to_txt(&current_song_info, &config.settings.txt_filename) {
            if !args.quiet {
                eprintln!("\n❌ Error writing to txt file: {}", e);
            }
        }
    }
    
    if config.settings.output_json {
        if let Err(e) = write_info_to_json(&current_song_info, &config.settings.json_filename) {
            if !args.quiet {
                eprintln!("\n❌ Error writing to json file: {}", e);
            }
        }
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

/// 将歌曲信息写入JSON文件 (UTF-16 LE)
fn write_info_to_json(info: &SongInfo, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let json_string = format!("{{ \"title\": \"{}\" }}", info.title);
    let u16_string = U16String::from_str(&json_string);
    let bytes: Vec<u8> = u16_string.into_vec().into_iter().flat_map(|c| c.to_le_bytes().to_vec()).collect();
    file.write_all(&bytes)?;
    Ok(())
}

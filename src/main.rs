use anyhow::Result;
use std::fs::File;
use std::io::{Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use widestring::U16String;
use crossterm::{execute, terminal, cursor};

mod cli;
mod config;
mod smtc;
mod lyrics;
mod song_info;
mod qrc;
mod local_qrc; // Enable local QRC cache module

use cli::Cli;
use config::Config;
use song_info::{SongInfo, QrcLine};
use lyrics::LyricFetcher;

#[tokio::main]
async fn main() -> Result<()> {
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
        println!("🎵 QQMusic Reader v{} (SMTC Mode)", env!("CARGO_PKG_VERSION"));
        println!("========================================");
        println!("⚠️  正在使用 Windows 系统媒体控制接口");
        println!("========================================\n");
    }

    // 初始化歌词获取器
    let lyric_fetcher = LyricFetcher::new();



    // 初始化终端
    if !args.quiet {
        execute!(stdout(), terminal::Clear(terminal::ClearType::All), cursor::MoveTo(0, 0))?;
        execute!(stdout(), cursor::Hide)?;
    }



    let mut last_song_info: Option<SongInfo> = None;
    let mut update_count = 0;



    // 主循环

    while running.load(Ordering::SeqCst) {
        // 使用包装块捕获循环内所有可能导致崩溃的错误
        let loop_result: Result<()> = async {
            // 使用 SMTC 读取媒体信息
            let current_song_info = match smtc::get_current_media_info().await {
            Ok(info) => {
                if let Some(mut info) = info {
                    // 如果切歌了（或者尚无缓存），尝试获取歌词覆盖进去
                    let should_fetch_lyrics = match &last_song_info {
                        Some(last) => last.title != info.title || last.artist != info.artist,
                        None => true,
                    };

                    if should_fetch_lyrics {
                        // 1. 网络 API 获取 LRC + 翻译 + QRC
                        match lyric_fetcher.fetch_lyrics(&info.title, &info.artist).await {
                            Ok((l, t, q)) => {
                                info.lyrics = l;
                                info.trans = t;
                                if !q.is_empty() {
                                    info.qrc_raw = q;
                                    match qrc::decode_qrc(&info.qrc_raw) {
                                        Ok(xml) => {
                                            if let Ok(lines) = qrc::parse_qrc_xml(&xml) {
                                                info.qrc_data = lines;
                                            }
                                            // 如果普通的 LRC 爬取失败但 QRC 成功解密了，从 QRC 的 XML 提取备份歌词
                                            if info.lyrics.is_empty() {
                                                info.lyrics = qrc::extract_lrc_from_xml(&xml).unwrap_or_default();
                                            }
                                            // 如果 parse_qrc_xml 没有产出结构化数据但 lyrics 是 QRC 文本格式，用 parse_qrc_text 解析
                                            if info.qrc_data.is_empty() && !info.lyrics.is_empty() {
                                                let parsed = qrc::parse_qrc_text(&info.lyrics);
                                                if !parsed.is_empty() {
                                                    info.qrc_data = parsed;
                                                }
                                            }
                                        },
                                        Err(e) => {
                                            // Debug 强制输出错误以便定位 Hex 无法解密的原因
                                            let dbg_msg = format!("Failed to decode QRC: {:?}", e);
                                            std::fs::write("qrc_decode_error.log", &dbg_msg).unwrap_or_default();
                                        }
                                    }
                                }
                            },
                            Err(e) => {
                                if config.settings.debug_mode {
                                    eprintln!("Lyric fetch error from API: {}", e);
                                }
                            }
                        }

                        // 2. 如果网络请求失败或歌词为空，尝试从本地缓存读取
                        let cache_dir_opt = match &config.settings.qrc_cache_dir {
                            Some(dir) => Some(std::path::PathBuf::from(dir)),
                            None => local_qrc::auto_detect_cache_dir(),
                        };

                        if let Some(cache_dir) = cache_dir_opt {
                            // 2.1 尝试读取本地 QRC
                            if info.qrc_raw.is_empty() {
                                if let Some(qrc_file) = local_qrc::find_qrc_file(&cache_dir, &info.title, &info.artist) {
                                    if let Ok(xml) = qrc::decode_qrc_from_file(&qrc_file) {
                                        info.qrc_raw = "[local]".to_string(); // 用 [local] 标记来源
                                        if let Ok(lines) = qrc::parse_qrc_xml(&xml) {
                                            info.qrc_data = lines;
                                        }

                                        // 顺便读取翻译文件
                                        if let Some(trans_file) = local_qrc::find_qrc_trans_file(&qrc_file) {
                                            if let Ok(trans_xml) = qrc::decode_qrc_from_file(&trans_file) {
                                                info.trans = trans_xml; 
                                            }
                                        }
                                    }
                                }
                            }

                            // 2.2 如果连普通的 LRC 歌词都没有，尝试读取本地 LRC
                            if info.lyrics.is_empty() {
                                if let Some(lrc_file) = local_qrc::find_lrc_file(&cache_dir, &info.title, &info.artist) {
                                    // LRC 文件可能被以与 QRC 同样的方式加密，也可能是明文
                                    let lrc_raw = match qrc::decode_qrc_from_file(&lrc_file) {
                                        Ok(decrypted) => decrypted,
                                        Err(_) => std::fs::read_to_string(&lrc_file).unwrap_or_default(),
                                    };
                                    
                                    info.lyrics = qrc::extract_lrc_from_xml(&lrc_raw).unwrap_or(lrc_raw);
                                    
                                    // 寻找翻译 LRC
                                    if let Some(trans_lrc_file) = local_qrc::find_lrc_trans_file(&lrc_file) {
                                        let trans_raw = match qrc::decode_qrc_from_file(&trans_lrc_file) {
                                            Ok(decrypted) => decrypted,
                                            Err(_) => std::fs::read_to_string(&trans_lrc_file).unwrap_or_default(),
                                        };
                                        info.trans = qrc::extract_lrc_from_xml(&trans_raw).unwrap_or(trans_raw);
                                    }
                                }
                            }
                        }
                    } else {
                        // 没切歌，保留上一首歌的数据
                        if let Some(last) = &last_song_info {
                            info.lyrics = last.lyrics.clone();
                            info.trans = last.trans.clone();
                            info.qrc_raw = last.qrc_raw.clone();
                            info.qrc_data = last.qrc_data.clone();
                        }
                    }
                    info
                } else {
                    SongInfo { 
                        title: "No music playing".to_string(),
                        artist: String::new(),
                        album: String::new(),
                        lyrics: String::new(),
                        trans: String::new(),
                        qrc_raw: String::new(),
                        qrc_data: Vec::new(),
                        current_time: 0,
                        total_time: 0,
                        progress_percent: 0.0,
                    }
                }
            },
            Err(e) => {
                if !args.quiet {
                    eprintln!("Error reading SMTC info: {}", e);
                }
                SongInfo { 
                    title: "ERROR".to_string(),
                    artist: String::new(),
                    album: String::new(),
                    lyrics: format!("System Media Control Error: {}", e),
                    trans: String::new(),
                    qrc_raw: String::new(),
                    qrc_data: Vec::new(),
                    current_time: 0,
                    total_time: 0,
                    progress_percent: 0.0,
                }
            }
        };

        // 检查歌曲是否有变化 (用于文件输出)
        let song_changed = match &last_song_info {
            Some(last) => last.title != current_song_info.title || last.artist != current_song_info.artist,
            None => true,
        };

        // 写入文件（无论歌曲是否变化都可能需要更新进度等，但对于 TXT/JSON 根据需求可优化，这里单独处理歌词实时更新）
        // 对于 current_lyric.txt, 依赖于时间，所以每隔 interval 都要更新
        if config.settings.output_txt && song_changed {
            if let Err(e) = write_info_to_txt(&current_song_info, &config.settings.txt_filename) {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error writing to txt file: {}", e);
                }
            }
        }
        
        if config.settings.output_json {
            // JSON 包含进度，可以每隔一定时间更新一次，不过这里就每帧刷新了
            if let Err(e) = write_info_to_json(&current_song_info, &config.settings.json_filename) {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error writing to json file: {}", e);
                }
            }
        }

        if config.settings.output_lyric {
            let filtered_lyrics = filter_lyrics(&current_song_info.lyrics, &current_song_info.trans, current_song_info.current_time);
            if let Err(e) = write_info_to_lyric_txt(&filtered_lyrics, &config.settings.lyric_filename) {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error writing lyric to txt file: {}", e);
                }
            }
        }

        // 更新控制台显示
        if !args.quiet {
            use crossterm::style::Stylize;
            execute!(stdout(), cursor::MoveTo(0, 0))?;
            
            // 显示歌曲信息
            if current_song_info.is_valid() && current_song_info.title != "No music playing" {
                // 处理空数据，用<参数名>替代
                let title = if current_song_info.title.is_empty() { "<歌曲名>" } else { &current_song_info.title };
                let artist = if current_song_info.artist.is_empty() { "<歌手>" } else { &current_song_info.artist };
                let album = if current_song_info.album.is_empty() { "<专辑>" } else { &current_song_info.album };
                
                // 第一行：歌曲名-歌手 (绿色高亮)
                let line1 = format!("{}-{}", title, artist);
                print!("{:<80}", line1.green().bold());
                execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                println!();
                
                // 第二行：专辑信息 (灰色)
                let album_line = format!("专辑:{}", album);
                print!("{:<80}", album_line.dark_grey());
                execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                println!();
                
                // 第三行：进度条和时间信息 (青色)
                if current_song_info.total_time > 0 {
                    let progress_bar = current_song_info.get_progress_bar(20).cyan();
                    let time_info = format!("{} / {} [{:.1}%]", 
                        current_song_info.format_current_time(),
                        current_song_info.format_total_time(),
                        current_song_info.progress_percent).cyan();
                    print!("{} {}", progress_bar, time_info);
                    execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                    println!();
                } else {
                    print!("{}", chrono::Local::now().format("%H:%M:%S").to_string().dark_grey());
                    execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                    println!();
                }
                
                println!(); // 空一行

                // 显示歌词（如果有）
                if !current_song_info.qrc_data.is_empty() {
                    // To get more precise time than SMTC's integer seconds, use progress_percent:
                    let precise_time_ms = (current_song_info.total_time as f64 * current_song_info.progress_percent as f64 * 10.0) as u64;

                    let (qrc_line, trans_line) = get_current_qrc_line(&current_song_info.qrc_data, &current_song_info.trans, precise_time_ms);
                    if let Some(line) = qrc_line {
                        print!("{}", render_qrc_line(line, precise_time_ms));
                        execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                        println!();

                        if !trans_line.is_empty() {
                            print!("{}", trans_line.white());
                            execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                            println!();
                        }
                    } else {
                        print!("...");
                        execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                        println!();
                    }
                } else if !current_song_info.lyrics.is_empty() {
                    let filtered_lyrics = filter_lyrics(&current_song_info.lyrics, &current_song_info.trans, current_song_info.current_time);
                    if !filtered_lyrics.is_empty() {
                         // 歌词可能包含两行（原唱+翻译）
                         let mut lines = filtered_lyrics.lines();
                          if let Some(orig) = lines.next() {
                             print!("{}", orig.yellow().bold());
                             execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                             println!();
                         }
                         if let Some(trans) = lines.next() {
                             print!("{}", trans.white());
                             execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                             println!();
                         }
                    } else {
                        print!("...");
                        execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                        println!();
                    }
                } else {
                     print!("{}", "Lyrics not found".dark_red());
                     execute!(stdout(), terminal::Clear(terminal::ClearType::UntilNewLine))?;
                     println!();
                }

                // QRC 调试信息
                if config.settings.debug_mode {
                    if !current_song_info.qrc_data.is_empty() {
                        let source = if current_song_info.qrc_raw == "[local]" { "本地缓存" } else { "API" };
                        let qrc_info = format!("[QRC] {} | {} 行逐字数据", source, current_song_info.qrc_data.len());
                        println!("{}", qrc_info.cyan());
                    } else if !current_song_info.lyrics.is_empty() {
                         println!("{}", "[QRC] 无逐字歌词数据".dark_grey());
                    }
                }
            } else {
                println!("{:<80}", "No music playing...".dark_grey());
                println!("{:<80}", "");
                println!("{:<80}", format!("{}", chrono::Local::now().format("%H:%M:%S")).dark_grey());
                println!("{:<80}", "");
            }
            
            // 调试模式下显示详细信息
            if config.settings.debug_mode {
                println!("{}", "─".repeat(80).dark_grey());
                let debug_line = format!("SMTC Mode | 更新: {} | 间隔: {}ms", 
                    update_count, config.settings.update_interval_ms);
                println!("{}", debug_line.dark_grey());
            }
            
            // 清除剩余的行（防止歌词变短时的残留）
            execute!(stdout(), terminal::Clear(terminal::ClearType::FromCursorDown))?;
        }

        last_song_info = Some(current_song_info);
        update_count += 1;
        
        // 等待下一次更新
            Ok(())
        }.await;

        if let Err(e) = loop_result {
            if config.settings.debug_mode && !args.quiet {
                eprintln!("Critical loop error: {}", e);
            }
            // 发生严重错误时稍微多睡一会，防止死循环刷屏
            tokio::time::sleep(Duration::from_secs(2)).await;
        } else {
            tokio::time::sleep(Duration::from_millis(config.settings.update_interval_ms)).await;
        }
    }


    // 恢复终端状态
    if !args.quiet {
        execute!(stdout(), cursor::Show)?;
        execute!(stdout(), terminal::Clear(terminal::ClearType::All))?;
        println!("程序已退出");
    }

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

    if args.no_lyric {
        config.settings.output_lyric = false;
    }
    
    if args.txt_file != "now_playing.txt" {
        config.settings.txt_filename = args.txt_file.clone();
    }
    
    if args.json_file != "now_playing.json" {
        config.settings.json_filename = args.json_file.clone();
    }
    
    if args.lyric_file != "current_lyric.txt" {
        config.settings.lyric_filename = args.lyric_file.clone();
    }
    
    if args.interval != 1000 {
        config.settings.update_interval_ms = args.interval;
    }
    
    if args.retries != 3 {
        config.settings.max_retries = args.retries;
    }

    if let Some(ref dir) = args.qrc_dir {
        config.settings.qrc_cache_dir = Some(dir.clone());
    }
    
    config
}

/// 将歌曲信息写入文本文件 (UTF-16 LE)
fn write_info_to_txt(info: &SongInfo, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let u16_string = U16String::from_str(&info.title);
    let bytes: Vec<u8> = u16_string.into_vec().into_iter().flat_map(|c| c.to_le_bytes().to_vec()).collect();
    let mut final_bytes = vec![0xFF, 0xFE]; 
    final_bytes.extend(bytes);
    file.write_all(&final_bytes)?;
    Ok(())
}

/// 过滤并提取当前进度的歌词
/// 注意：这里的实现比较简化，SMTC不提供精确的歌词行时间戳，
/// 且从网络下载的LRC是整个文件。
/// 根据当前时间获取对应的 QRC 行和翻译
fn get_current_qrc_line<'a>(qrc_data: &'a [QrcLine], trans: &'a str, current_time_ms: u64) -> (Option<&'a QrcLine>, String) {
    let mut current_line: Option<&QrcLine> = None;
    let mut max_start_time = 0;

    for line in qrc_data {
        // 如果当前时间已经超过这行的开始时间
        if current_time_ms >= line.start_time_ms {
            if line.start_time_ms >= max_start_time {
                max_start_time = line.start_time_ms;
                current_line = Some(line);
            }
        }
    }

    let mut current_trans = String::new();
    if let Some(line) = current_line {
        // 尝试从翻译中找出对应时间戳的行
        if !trans.is_empty() {
            let line_time_sec = line.start_time_ms as f64 / 1000.0;
            for trans_line in trans.lines() {
                if let Some(start_bracket) = trans_line.find('[') {
                    if let Some(end_bracket) = trans_line.find(']') {
                        let time_str = &trans_line[start_bracket+1..end_bracket];
                        if let Ok(time_val) = parse_lrc_time(time_str) {
                            if (time_val - line_time_sec).abs() < 0.5 {
                                current_trans = trans_line[end_bracket+1..].trim().to_string();
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    (current_line, current_trans)
}

/// 渲染带高亮的 QRC 逐字歌词
fn render_qrc_line(line: &QrcLine, current_time_ms: u64) -> String {
    use crossterm::style::Stylize;
    
    // 如果还没唱到这行，全灰
    if current_time_ms < line.start_time_ms {
        return line.content.clone().dark_grey().to_string();
    }
    
    // 如果这行已经唱完了，全黄
    if current_time_ms >= line.start_time_ms + line.duration_ms {
        return line.content.clone().yellow().bold().to_string();
    }
    
    // 正在唱这一行：按字渲染
    let mut result = String::new();
    for word in &line.words {
        // QRC 文本格式中 word.start_time_ms 已经是绝对时间（相对于歌曲开头）
        let word_end_ms = word.start_time_ms + word.duration_ms;
        
        if current_time_ms >= word_end_ms {
            // 这个字已经唱完，显示黄色
            result.push_str(&word.content.clone().yellow().bold().to_string());
        } else if current_time_ms >= word.start_time_ms && current_time_ms < word_end_ms {
            // 正在唱的字，高亮青色
            result.push_str(&word.content.clone().cyan().bold().to_string());
        } else {
            // 还没唱到的字，显示暗色
            result.push_str(&word.content.clone().dark_grey().to_string());
        }
    }
    
    result
}

/// 过滤并提取当前进度的歌词（支持双语）
fn filter_lyrics(lyrics: &str, trans: &str, current_time_sec: u64) -> String {
    let mut current_lyric = String::new();
    let mut current_trans = String::new();
    let mut max_time = -1.0;

    // 查找原文
    for line in lyrics.lines() {
        if let Some(start_bracket) = line.find('[') {
            if let Some(end_bracket) = line.find(']') {
                let time_str = &line[start_bracket+1..end_bracket];
                if let Ok(time_val) = parse_lrc_time(time_str) {
                    if time_val <= current_time_sec as f64 {
                        if time_val > max_time {
                            max_time = time_val;
                            current_lyric = line[end_bracket+1..].trim().to_string();
                        }
                    }
                }
            }
        }
    }

    // 如果有翻译，查找对应的翻译行
    if !trans.is_empty() && max_time >= 0.0 {
        for line in trans.lines() {
            if let Some(start_bracket) = line.find('[') {
                if let Some(end_bracket) = line.find(']') {
                    let time_str = &line[start_bracket+1..end_bracket];
                    if let Ok(time_val) = parse_lrc_time(time_str) {
                        // 翻译的时间轴通常和原文一致，这里找最接近当前时间且不超过当前时间的
                        // 由于网络获取的 trans 可能微秒级差异，或者直接用相同的时间戳查找
                        if (time_val - max_time).abs() < 0.5 { // 容差 0.5 秒
                            current_trans = line[end_bracket+1..].trim().to_string();
                            break;
                        }
                    }
                }
            }
        }
    }
    
    // 如果没有带时间轴的原文，则退而求其次显示第一行
    if max_time == -1.0 && !lyrics.is_empty() {
        for line in lyrics.lines() {
            if !line.trim().is_empty() && !line.starts_with("[") {
                 current_lyric = line.trim().to_string();
                 break;
            }
        }
    }

    if current_trans.is_empty() {
        current_lyric
    } else {
        format!("{}\n{}", current_lyric, current_trans)
    }
}

fn parse_lrc_time(time_str: &str) -> Result<f64, ()> {
    // mm:ss.xx or mm:ss
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() == 2 {
        let min: f64 = parts[0].parse().map_err(|_| ())?;
        let sec: f64 = parts[1].parse().map_err(|_| ())?;
        return Ok(min * 60.0 + sec);
    }
    Err(())
}

/// 将歌曲信息完整写入JSON文件
fn write_info_to_json(info: &SongInfo, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let json_string = serde_json::to_string_pretty(info)?;
    file.write_all(json_string.as_bytes())?;
    Ok(())
}

/// 单独将当前歌词写入文本文件 (UTF-16 LE)
fn write_info_to_lyric_txt(lyric: &str, filename: &str) -> Result<()> {
    let mut file = File::create(filename)?;
    let u16_string = U16String::from_str(lyric);
    let bytes: Vec<u8> = u16_string.into_vec().into_iter().flat_map(|c| c.to_le_bytes().to_vec()).collect();
    // 添加 UTF-16 LE BOM 方便 Windows Notepad 和 OBS 等识别
    let mut final_bytes = vec![0xFF, 0xFE]; 
    final_bytes.extend(bytes);
    file.write_all(&final_bytes)?;
    Ok(())
}

use anyhow::Result;
use std::fs::File;
use std::io::{Write, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use widestring::U16String;
use crossterm::{execute, terminal, cursor};

mod cli;
mod config;
mod smtc;
mod lyrics;
mod song_info;
mod qrc;
mod local_qrc; // Enable local QRC cache module
mod server;

use cli::Cli;
use config::Config;
use song_info::{SongInfo, QrcLine};
use lyrics::LyricFetcher;

/// 后台歌词缓存 — 由主循环读取、后台任务写入
struct LyricsCache {
    /// 缓存的歌曲标识 "title|artist"
    song_key: String,
    lyrics: String,
    trans: String,
    qrc_raw: String,
    qrc_data: Vec<QrcLine>,
}

impl LyricsCache {
    fn new() -> Self {
        Self {
            song_key: String::new(),
            lyrics: String::new(),
            trans: String::new(),
            qrc_raw: String::new(),
            qrc_data: Vec::new(),
        }
    }

    /// 检查歌曲是否已缓存
    fn has_song(&self, title: &str, artist: &str) -> bool {
        self.song_key == format!("{}|{}", title, artist)
    }
}

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

    // 初始化歌词获取器和后台缓存
    let lyric_fetcher = Arc::new(LyricFetcher::with_debug(config.settings.debug_mode));
    let lyrics_cache = Arc::new(RwLock::new(LyricsCache::new()));

    // 初始化数据广播通道并启动服务
    let (tx, rx) = tokio::sync::watch::channel(SongInfo::default());
    if config.settings.enable_server {
        let port = config.settings.server_port;
        // 在 TUI 接管前打印服务器信息，避免与渲染竞态
        println!("🚀 本地同步服务已启动: http://127.0.0.1:{}", port);
        println!("📡 WebSocket 接口: ws://127.0.0.1:{}/ws", port);
        println!("📄 当前状态接口: http://127.0.0.1:{}/api/current", port);
        tokio::spawn(async move {
            server::start_server(port, rx).await;
        });
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
        let loop_start = tokio::time::Instant::now();
        // 使用包装块捕获循环内所有可能导致崩溃的错误
        let loop_result: Result<()> = async {
            // 使用 SMTC 读取媒体信息
            let current_song_info = match smtc::get_current_media_info().await {
            Ok(info) => {
                if let Some(mut info) = info {
                    // 后台歌词加载：切歌时后台请求，不阻塞 TUI
                    let song_key = format!("{}|{}", info.title, info.artist);
                    let cached_has_song = lyrics_cache.read().await.has_song(&info.title, &info.artist);
                    
                    if !cached_has_song {
                        // 歌曲不在缓存中 → 后台获取
                        let debug = config.settings.debug_mode;
                        if debug { eprintln!("[lyrics] Fetching lyrics for '{} - {}' in background", info.title, info.artist); }
                        let cache = lyrics_cache.clone();
                        let fetcher = lyric_fetcher.clone();
                        let key = song_key.clone();
                        let t = info.title.clone();
                        let a = info.artist.clone();
                        tokio::spawn(async move {
                            match fetcher.fetch_lyrics(&t, &a).await {
                                Ok((l, trans, q)) => {
                                    let mut w = cache.write().await;
                                    w.song_key = key;
                                    w.lyrics = l;
                                    w.trans = trans;
                                    w.qrc_raw = q;
                                    // QRC 解码（只做一次，不在主循环重复）
                                    if !w.qrc_raw.is_empty() {
                                        if debug { eprintln!("[QRC] Raw data: {} bytes", w.qrc_raw.len()); }
                                        match qrc::decode_qrc(&w.qrc_raw) {
                                            Ok(xml) => {
                                                if debug { eprintln!("[QRC] Decrypted XML: {} bytes", xml.len()); }
                                                match qrc::parse_qrc_xml(&xml) {
                                                    Ok(lines) => {
                                                        if debug { eprintln!("[QRC] Parsed {} lines from XML", lines.len()); }
                                                        w.qrc_data = lines;
                                                    }
                                                    Err(e) => {
                                                        if debug { let preview: String = xml.chars().take(300).collect(); eprintln!("[QRC] XML parse failed: {} — preview: {}", e, preview); }
                                                    }
                                                }
                                                if w.lyrics.is_empty() {
                                                    w.lyrics = qrc::extract_lrc_from_xml(&xml).unwrap_or_default();
                                                    if debug && !w.lyrics.is_empty() { eprintln!("[QRC] Extracted LRC from XML: {} chars", w.lyrics.len()); }
                                                }
                                                if w.qrc_data.is_empty() && !w.lyrics.is_empty() {
                                                    let parsed = qrc::parse_qrc_text(&w.lyrics);
                                                    if !parsed.is_empty() {
                                                        if debug { eprintln!("[QRC] Text fallback parsed {} lines", parsed.len()); }
                                                        w.qrc_data = parsed;
                                                    }
                                                }
                                            },
                                            Err(e) => {
                                                if debug { let preview: String = w.qrc_raw.chars().take(100).collect(); eprintln!("[QRC] Decode failed: {} — raw preview: {}", e, preview); }
                                            }
                                        }
                                    }
                                    if debug { eprintln!("[lyrics] ✓ Background fetch complete for '{} - {}'", t, a); }
                                }
                                Err(e) => {
                                    if debug { eprintln!("[lyrics] Background fetch failed for '{} - {}': {}", t, a, e); }
                                }
                            }
                        });
                    }

                    // 从缓存中获取歌词 + QRC 数据（QRC 解码已在后台完成）
                    {
                        let cached = lyrics_cache.read().await;
                        info.lyrics = cached.lyrics.clone();
                        info.trans = cached.trans.clone();
                        info.qrc_raw = cached.qrc_raw.clone();
                        info.qrc_data = cached.qrc_data.clone();
                    }

                    // 最终兜底: 如果 lyrics 仍为空但 trans 不为空，将翻译作为歌词展示
                    if info.lyrics.is_empty() && !info.trans.is_empty() {
                        info.lyrics = info.trans.clone();
                    }

                    // 如果网络歌词为空，尝试从本地缓存读取
                    let cache_dir_opt = local_qrc::auto_detect_cache_dir();
                    if let Some(cache_dir) = cache_dir_opt {
                        if info.qrc_raw.is_empty() {
                            if let Some(qrc_file) = local_qrc::find_qrc_file(&cache_dir, &info.title, &info.artist) {
                                if let Ok(xml) = qrc::decode_qrc_from_file(&qrc_file) {
                                    info.qrc_raw = "[local]".to_string();
                                    if let Ok(lines) = qrc::parse_qrc_xml(&xml) {
                                        info.qrc_data = lines;
                                    }
                                    if let Some(trans_file) = local_qrc::find_qrc_trans_file(&qrc_file) {
                                        if let Ok(trans_xml) = qrc::decode_qrc_from_file(&trans_file) {
                                            info.trans = trans_xml;
                                        }
                                    }
                                }
                            }
                        }
                        if info.lyrics.is_empty() {
                            if let Some(lrc_file) = local_qrc::find_lrc_file(&cache_dir, &info.title, &info.artist) {
                                let lrc_raw = match qrc::decode_qrc_from_file(&lrc_file) {
                                    Ok(decrypted) => decrypted,
                                    Err(_) => std::fs::read_to_string(&lrc_file).unwrap_or_default(),
                                };
                                info.lyrics = qrc::extract_lrc_from_xml(&lrc_raw).unwrap_or(lrc_raw);
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
                        current_time_ms: 0,
                        total_time_ms: 0,
                        progress_percent: 0.0,
                        is_playing: false,
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
                    current_time_ms: 0,
                    total_time_ms: 0,
                    progress_percent: 0.0,
                    is_playing: false,
                }
            }
        };

        // 检查歌曲是否有变化 (用于文件输出)
        let song_changed = match &last_song_info {
            Some(last) => last.title != current_song_info.title || last.artist != current_song_info.artist,
            None => true,
        };

        // 广播最新状态给所有 WebSocket 客户端
        let _ = tx.send(current_song_info.clone());

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
            let precise_time_ms = current_song_info.current_time_ms;
            let mut filtered_lyrics = String::new();
            
            if !current_song_info.qrc_data.is_empty() {
                let (qrc_line, trans_line) = get_current_qrc_line(&current_song_info.qrc_data, &current_song_info.trans, precise_time_ms);
                if let Some(line) = qrc_line {
                    if trans_line.is_empty() {
                        filtered_lyrics = line.content.clone();
                    } else {
                        filtered_lyrics = format!("{}\n{}", line.content, trans_line);
                    }
                }
            }
            
            if filtered_lyrics.is_empty() {
                filtered_lyrics = filter_lyrics(&current_song_info.lyrics, &current_song_info.trans, current_song_info.current_time);
            }
            
            let display_lyric = if filtered_lyrics.trim().is_empty() {
                if current_song_info.is_valid() && current_song_info.title != "No music playing" {
                    // 若有歌曲播放但是暂无对应时间的歌词（如间奏或还没开始），显示省略号填充
                    "...\n\n".to_string()
                } else {
                    String::new()
                }
            } else {
                filtered_lyrics
            };

            if let Err(e) = write_info_to_lyric_txt(&display_lyric, &config.settings.lyric_filename) {
                if config.settings.debug_mode && !args.quiet {
                    eprintln!("Error writing lyric to txt file: {}", e);
                }
            }
        }

        // 更新控制台显示 — 使用缓冲渲染消除闪烁
        if !args.quiet {
            use crossterm::style::Stylize;
            
            // 在内存中构建完整帧
            let mut frame = String::new();
            
            // 显示歌曲信息
            if current_song_info.is_valid() && current_song_info.title != "No music playing" {
                // 处理空数据，用<参数名>替代
                let title = if current_song_info.title.is_empty() { "<歌曲名>" } else { &current_song_info.title };
                let artist = if current_song_info.artist.is_empty() { "<歌手>" } else { &current_song_info.artist };
                let album = if current_song_info.album.is_empty() { "<专辑>" } else { &current_song_info.album };
                
                // 第一行：歌曲名-歌手 (绿色高亮)
                let title_text = format!("{}-{}", title, artist);
                frame.push_str(&pad_line(&title_text.clone(), format!("{}", title_text.green().bold()), 80));
                
                // 第二行：专辑信息 (灰色)
                let album_text = format!("专辑:{}", album);
                frame.push_str(&pad_line(&album_text.clone(), format!("{}", album_text.dark_grey()), 80));
                
                // 第三行：进度条和时间信息 (青色)
                if current_song_info.total_time > 0 {
                    let progress_bar = current_song_info.get_progress_bar(20).cyan();
                    let time_info = format!("{} / {} [{:.1}%]", 
                        current_song_info.format_current_time(),
                        current_song_info.format_total_time(),
                        current_song_info.progress_percent).cyan();
                    frame.push_str(&pad_styled(&format!("{} {}", progress_bar, time_info), 80));
                    frame.push('\n');
                } else {
                    frame.push_str(&pad_line(&chrono::Local::now().format("%H:%M:%S").to_string(), format!("{}", chrono::Local::now().format("%H:%M:%S").to_string().dark_grey()), 80));
                }
                
                frame.push('\n'); // 空一行

                // 显示歌词（如果有）
                if !current_song_info.qrc_data.is_empty() {
                    let precise_time_ms = current_song_info.current_time_ms;

                    let (qrc_line, trans_line) = get_current_qrc_line(&current_song_info.qrc_data, &current_song_info.trans, precise_time_ms);
                    if let Some(line) = qrc_line {
                        frame.push_str(&pad_styled(&render_qrc_line(line, precise_time_ms), 80));
                        frame.push('\n');
                        if !trans_line.is_empty() {
                            frame.push_str(&pad_line(&trans_line.clone(), format!("{}", trans_line.white()), 80));
                        }
                    } else {
                        frame.push_str(&pad_line("...", format!("{}", "...".dark_grey()), 80));
                    }
                } else if !current_song_info.lyrics.is_empty() {
                    let filtered_lyrics = filter_lyrics(&current_song_info.lyrics, &current_song_info.trans, current_song_info.current_time);
                    if !filtered_lyrics.is_empty() {
                         // 歌词可能包含两行（原唱+翻译）
                         let mut lines = filtered_lyrics.lines();
                          if let Some(orig) = lines.next() {
                             frame.push_str(&pad_line(&orig, format!("{}", orig.yellow().bold()), 80));
                         }
                         if let Some(trans) = lines.next() {
                             frame.push_str(&pad_line(&trans, format!("{}", trans.white()), 80));
                         }
                    } else {
                        frame.push_str(&pad_line("...", format!("{}", "...".dark_grey()), 80));
                    }
                } else {
                     frame.push_str(&pad_line("Lyrics not found", format!("{}", "Lyrics not found".dark_red()), 80));
                }

                // QRC 调试信息
                if config.settings.debug_mode {
                    if !current_song_info.qrc_data.is_empty() {
                        let source = if current_song_info.qrc_raw == "[local]" { "本地缓存" } else { "API" };
                        let qrc_debug = format!("[QRC] {} | {} 行逐字数据", source, current_song_info.qrc_data.len());
                        frame.push_str(&pad_line(&qrc_debug.clone(), format!("{}", qrc_debug.cyan()), 80));
                    } else if !current_song_info.lyrics.is_empty() {
                         frame.push_str(&pad_line("[QRC] 无逐字歌词数据", format!("{}", "[QRC] 无逐字歌词数据".dark_grey()), 80));
                    }
                }
            } else {
                frame.push_str(&pad_line("No music playing...", format!("{}", "No music playing...".dark_grey()), 80));
                frame.push_str(&pad_line("", format!(""), 80));
                frame.push_str(&pad_line(&chrono::Local::now().format("%H:%M:%S").to_string(), format!("{}", chrono::Local::now().format("%H:%M:%S").to_string().dark_grey()), 80));
                frame.push_str(&pad_line("", format!(""), 80));
            }
            
            // 调试模式下显示详细信息
            if config.settings.debug_mode {
                frame.push_str(&pad_line(&"─".repeat(80), format!("{}", "─".repeat(80).dark_grey()), 80));
                let debug_text = format!("SMTC Mode | 更新: {} | 间隔: {}ms", update_count, config.settings.update_interval_ms);
                frame.push_str(&pad_line(&debug_text.clone(), format!("{}", debug_text.dark_grey()), 80));
            }
            
            // 一次性输出到终端：移到左上角 → 写入帧 → 清除残留行
            execute!(stdout(), cursor::MoveTo(0, 0))?;
            print!("{}", frame);
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
            // 时间补偿：减去本次循环已耗时，保证实际间隔接近 update_interval_ms
            let elapsed = loop_start.elapsed();
            let interval = Duration::from_millis(config.settings.update_interval_ms);
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
            // 如果循环耗时已经超过 interval，不休眠直接进入下一次
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
    
    // CLI --interval 始终覆盖配置文件
    config.settings.update_interval_ms = args.interval;
    
    if args.no_server {
        config.settings.enable_server = false;
    }

    if args.port != 3000 {
        config.settings.server_port = args.port;
    }

    if args.retries != 3 {
        config.settings.max_retries = args.retries;
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
        // 尝试从翻译中找出对应时间戳的行（最近邻匹配）
        if !trans.is_empty() {
            let line_time_sec = line.start_time_ms as f64 / 1000.0;
            let mut best_diff = f64::MAX;
            for trans_line in trans.lines() {
                if let Some(start_bracket) = trans_line.find('[') {
                    if let Some(end_bracket) = trans_line.find(']') {
                        let text = trans_line[end_bracket+1..].trim();
                        // Skip empty lines and "//" placeholders
                        if text.is_empty() || text == "//" {
                            continue;
                        }
                        let time_str = &trans_line[start_bracket+1..end_bracket];
                        if let Ok(time_val) = parse_lrc_time(time_str) {
                            let diff = (time_val - line_time_sec).abs();
                            if diff < 0.1 && diff < best_diff {
                                best_diff = diff;
                                current_trans = text.to_string();
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
    // QRC 文本格式中 word 的 start_time_ms 是相对于行起始的偏移，
    // 需要加上 line.start_time_ms 得到绝对时间
    let mut result = String::new();
    for word in &line.words {
        let word_start_abs = line.start_time_ms + word.start_time_ms;
        let word_end_abs = word_start_abs + word.duration_ms;
        
        if current_time_ms >= word_end_abs {
            // 这个字已经唱完，显示黄色
            result.push_str(&word.content.clone().yellow().bold().to_string());
        } else if current_time_ms >= word_start_abs && current_time_ms < word_end_abs {
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
                        let text = line[end_bracket+1..].trim();
                        // Skip empty lines and "//" placeholders
                        if text.is_empty() || text == "//" {
                            continue;
                        }
                        if (time_val - max_time).abs() < 0.1 {
                            current_trans = text.to_string();
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

/// 渲染内容并补空格到指定可见宽度
/// 解决 Stylize 的 ANSI 转义码被 `{:<w}` 计入宽度的问题
fn pad_line(text: &str, styled: String, width: usize) -> String {
    let visible = text.chars().count();
    if visible < width {
        format!("{}{}\n", styled, " ".repeat(width - visible))
    } else {
        format!("{}\n", styled)
    }
}

/// 对已包含 ANSI 转义码的字符串按可见宽度补空格
fn pad_styled(styled: &str, width: usize) -> String {
    // 粗略计算可见宽度：去掉 \x1b[... 转义序列后数可见字符
    // 不引入 regex 依赖，通过简单状态机跳过 ANSI 序列
    let mut visible = 0;
    let mut in_escape = false;
    for c in styled.chars() {
        if in_escape {
            if c == 'm' || c == 'H' { in_escape = false; }
            continue;
        }
        if c == '\x1b' { in_escape = true; continue; }
        visible += 1;
    }
    if visible < width {
        format!("{}{}", styled, " ".repeat(width - visible))
    } else {
        styled.to_string()
    }
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

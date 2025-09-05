use crate::{config::Config, song_info::SongInfo};
use anyhow::{anyhow, Context, Result};
use winapi::ctypes::c_void;
use winapi::um::memoryapi::ReadProcessMemory;
use winapi::um::winnt::HANDLE;

/// 读取指定内存地址的指针 (针对32位应用程序，读取4字节指针)
pub fn read_pointer(handle: HANDLE, address: usize) -> Result<usize> {
    let mut buffer: u32 = 0;
    let mut bytes_read: usize = 0;
    let result = unsafe {
        ReadProcessMemory(
            handle,
            address as *const c_void,
            &mut buffer as *mut _ as *mut c_void,
            std::mem::size_of::<u32>(),
            &mut bytes_read,
        )
    };
    if result == 0 || bytes_read != std::mem::size_of::<u32>() {
        Err(anyhow!("Failed to read pointer"))
    } else {
        Ok(buffer as usize)
    }
}

/// 通过基地址和偏移量链解析最终地址
fn resolve_pointer_chain(handle: HANDLE, base_address: usize, offsets: &[usize]) -> Result<usize> {
    let mut current_ptr_value = base_address;
    for (i, &offset) in offsets.iter().enumerate() {
        current_ptr_value = read_pointer(handle, current_ptr_value)
            .with_context(|| format!("Failed at pointer chain index {}", i))?;
        
        if current_ptr_value == 0 {
            return Err(anyhow!("Pointer at chain index {} was null", i));
        }
        
        current_ptr_value += offset;
    }
    Ok(current_ptr_value)
}

/// 读取内存中的宽字符字符串 (UTF-16 LE)
pub fn read_wstring(handle: HANDLE, address: usize, max_length: Option<usize>, config: &Config) -> Result<String> {
    if address == 0 {
        return Ok(String::new());
    }

    let max_chars = max_length.unwrap_or(4096);
    let mut buffer = Vec::<u16>::with_capacity(max_chars.min(64));
    let mut current_address = address;
    let mut bytes_read: usize = 0;

    // 首先尝试读取前4个字节，看看是否是字符串长度
    let mut length_buffer: u32 = 0;
    let length_result = unsafe {
        ReadProcessMemory(
            handle,
            current_address as *const c_void,
            &mut length_buffer as *mut _ as *mut c_void,
            4,
            &mut bytes_read,
        )
    };

    // 检查是否可能是长度前缀的字符串
    if length_result != 0 && bytes_read == 4 && length_buffer > 0 && length_buffer <= max_chars as u32 {
        // 可能是长度前缀的字符串，跳过长度字段
        current_address += 4;
        
        for _ in 0..length_buffer {
            let mut char_buffer: u16 = 0;
            let result = unsafe {
                ReadProcessMemory(
                    handle,
                    current_address as *const c_void,
                    &mut char_buffer as *mut _ as *mut c_void,
                    2, // 2 bytes for a u16
                    &mut bytes_read,
                )
            };

            // 如果读取失败、读不到2字节或遇到空字符，则停止
            if result == 0 || bytes_read != 2 || char_buffer == 0 {
                break;
            }
            buffer.push(char_buffer);
            current_address += 2;
        }
    } else {
        // 尝试直接读取为null结尾的字符串
        for _ in 0..max_chars {
            let mut char_buffer: u16 = 0;
            let result = unsafe {
                ReadProcessMemory(
                    handle,
                    current_address as *const c_void,
                    &mut char_buffer as *mut _ as *mut c_void,
                    2, // 2 bytes for a u16
                    &mut bytes_read,
                )
            };

            // 如果读取失败、读不到2字节或遇到空字符，则停止
            if result == 0 || bytes_read != 2 || char_buffer == 0 {
                break;
            }
            buffer.push(char_buffer);
            current_address += 2;
        }
    }

    if buffer.is_empty() {
        // 调试输出原始u16值，即使缓冲区为空
        if config.settings.debug_mode {
            println!("DEBUG: Raw u16 buffer (empty): {:?}", buffer);
        }
        return Err(anyhow!("Read 0 chars from wstring pointer"));
    }

    // 调试输出原始u16值
    if config.settings.debug_mode {
        println!("DEBUG: Raw u16 buffer: {:?}", buffer);
        println!("DEBUG: Trying to decode as UTF-16...");
        
        // 尝试不同的解码方式
        if let Ok(utf8_str) = String::from_utf16(&buffer) {
            println!("DEBUG: UTF-16 decode successful: '{}'", utf8_str);
        } else {
            println!("DEBUG: UTF-16 decode failed, trying lossy");
            let lossy_str = String::from_utf16_lossy(&buffer);
            println!("DEBUG: UTF-16 lossy decode: '{}'", lossy_str);
        }
    }

    Ok(String::from_utf16_lossy(&buffer))
}

/// 从QQ音乐内存中读取歌曲信息
///
/// # 注意
/// 这些指针和偏移量是针对特定版本的QQ音乐的，随时可能失效。
/// 如果失效，你需要使用类似Cheat Engine的工具重新寻找这些地址。
pub fn read_song_info(handle: HANDLE, base_address: usize, config: &Config) -> Result<SongInfo> {
    let mut song_info = SongInfo::default();
    
    // 读取歌曲标题
    if let Ok(title) = read_string_field(handle, base_address, config, &config.memory_offsets.song_name_offset, &config.memory_offsets.song_name_chain, config.memory_offsets.title_offset, "标题") {
        song_info.title = title;
    }
    
    // 读取歌手信息
    if let Ok(artist) = read_string_field(handle, base_address, config, &config.memory_offsets.song_singer_offset, &config.memory_offsets.song_singer_chain, config.memory_offsets.title_offset, "歌手") {
        song_info.artist = artist;
    }
    
    // 读取专辑信息
    if let Ok(album) = read_string_field(handle, base_address, config, &config.memory_offsets.song_album_offset, &config.memory_offsets.song_album_chain, config.memory_offsets.title_offset, "专辑") {
        song_info.album = album;
    }
    
    // 读取歌词信息
    if let Ok(lyrics) = read_string_field(handle, base_address, config, &config.memory_offsets.song_lyrics_offset, &config.memory_offsets.song_lyrics_chain, config.memory_offsets.title_offset, "歌词") {
        song_info.lyrics = lyrics;
    }
    
    // 读取当前播放时间
    if let Ok(current_time) = read_int_field(handle, base_address, config, &config.memory_offsets.current_time_offset, &config.memory_offsets.current_time_chain, "当前时间") {
        song_info.current_time = current_time;
    }
    
    // 读取总时长
    if let Ok(total_time) = read_int_field(handle, base_address, config, &config.memory_offsets.total_time_offset, &config.memory_offsets.total_time_chain, "总时长") {
        song_info.total_time = total_time;
    }
    
    // 计算进度百分比
    if song_info.total_time > 0 {
        song_info.progress_percent = (song_info.current_time as f32 / song_info.total_time as f32) * 100.0;
    } else {
        song_info.progress_percent = 0.0;
    }
    
    // 检查是否至少读取到了标题
    if song_info.is_valid() {
        if config.settings.debug_mode {
            println!("🎵 成功读取歌曲数据:");
            println!("   标题: '{}'", song_info.title);
            println!("   歌手: '{}'", song_info.artist);
            println!("   专辑: '{}'", song_info.album);
            println!("   歌词: '{}{}'", song_info.lyrics.chars().take(50).collect::<String>(), if song_info.lyrics.len() > 50 { "..." } else { "" });
        }
        Ok(song_info)
    } else {
        Err(anyhow!("无法读取歌曲信息。请检查QQ音乐版本或更新配置文件。"))
    }
}

/// 读取字符串字段的辅助函数
fn read_string_field(handle: HANDLE, base_address: usize, config: &Config, field_offset: &usize, chain: &[usize], title_offset: usize, field_name: &str) -> Result<String> {
    let test_base = base_address + field_offset;
    
    if config.settings.debug_mode {
        println!("🔧 读取{} - 使用配置偏移量: {:#X}", field_name, field_offset);
    }

    // 解析指针链获取字段基地址
    if let Ok(final_ptr) = resolve_pointer_chain(handle, test_base, chain) {
        // 检查指针是否合理
        if final_ptr > 0x1000 && final_ptr < 0x7FFFFFFFFFFFFFFF {
            if config.settings.debug_mode {
                println!("✅ {}指针链解析成功", field_name);
                println!("🔧 {}_offset: {:#X}", field_name, title_offset);
            }
            
            // 直接使用配置中的偏移量读取字符串信息
            let string_address = final_ptr + title_offset;
            
            if config.settings.debug_mode {
                println!("🔍 {}使用配置中的偏移量: {:#X}", field_name, title_offset);
            }
            
            let result = read_wstring(handle, string_address, Some(config.settings.max_string_length), config)
                .unwrap_or_default();
            
            if !result.is_empty() {
                if config.settings.debug_mode {
                    println!("✅ 成功读取{}: '{}'", field_name, result);
                }
                return Ok(result);
            } else {
                if config.settings.debug_mode {
                    println!("⚠️ {}读取结果为空", field_name);
                }
            }
        } else {
            if config.settings.debug_mode {
                println!("⚠️ {}指针地址无效: {:#X}", field_name, final_ptr);
            }
        }
    } else {
        if config.settings.debug_mode {
            println!("⚠️ {}指针链解析失败", field_name);
        }
    }
    
    Err(anyhow!("无法读取{}信息", field_name))
}

/// 读取32位整数值（用于播放时间等）
fn read_dword(handle: HANDLE, address: usize) -> Result<u32> {
    let mut buffer: u32 = 0;
    let mut bytes_read: usize = 0;
    let result = unsafe {
        ReadProcessMemory(
            handle,
            address as *const c_void,
            &mut buffer as *mut _ as *mut c_void,
            4,
            &mut bytes_read,
        )
    };
    if result == 0 || bytes_read != 4 {
        Err(anyhow!("Failed to read dword"))
    } else {
        Ok(buffer)
    }
}

/// 读取整数字段的辅助函数
fn read_int_field(handle: HANDLE, base_address: usize, config: &Config, field_offset: &usize, chain: &[usize], field_name: &str) -> Result<u32> {
    let test_base = base_address + field_offset;
    
    if config.settings.debug_mode {
        println!("🔧 读取{} - 使用配置偏移量: {:#X}", field_name, field_offset);
    }

    // 解析指针链获取字段基地址
    if let Ok(final_ptr) = resolve_pointer_chain(handle, test_base, chain) {
        // 检查指针是否合理
        if final_ptr > 0x1000 && final_ptr < 0x7FFFFFFFFFFFFFFF {
            if config.settings.debug_mode {
                println!("✅ {}指针链解析成功", field_name);
            }
            
            let result = read_dword(handle, final_ptr)
                .unwrap_or(0);
            
            if config.settings.debug_mode {
                println!("🔍 {}读取结果: {}", field_name, result);
            }
            
            return Ok(result);
        } else {
            if config.settings.debug_mode {
                println!("⚠️ {}指针地址无效", field_name);
            }
        }
    } else {
        if config.settings.debug_mode {
            println!("⚠️ {}指针链解析失败", field_name);
        }
    }
    
    Ok(0) // 返回默认值0而不是错误
}

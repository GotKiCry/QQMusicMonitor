use anyhow::{Result, Context};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use des::Des;
use des::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
#[cfg_attr(not(test), allow(unused_imports))]
use flate2::read::{ZlibDecoder, DeflateDecoder};
use std::io::Read;
use std::path::Path;
use xmltree::Element;
use crate::song_info::{QrcLine, QrcWord};
use std::ffi::c_int;

extern "C" {
    // 导入从 C 代码暴露出的 DES 加密/解密函数
    fn Ddes(buff: *mut u8, key: *mut u8, len: c_int) -> c_int;
    fn des(buff: *mut u8, key: *mut u8, len: c_int) -> c_int;
}

// Function to decrypt and decompress QRC lyrics
pub fn decode_qrc(qrc_base64: &str) -> Result<String> {
    if qrc_base64.is_empty() {
        return Ok(String::new());
    }

    // 1. Decode Encoded String
    let is_hex = qrc_base64.chars().all(|c| c.is_ascii_hexdigit());
    let encrypted_data = if is_hex {
        hex::decode(qrc_base64).context("Failed to decode hex QRC data")?
    } else {
        STANDARD.decode(qrc_base64).context("Failed to decode base64 QRC data")?
    };

    // 2. DES Decrypt
    // QQ Music 的 API Hex 和本地文件使用相同的魔改 DES 算法 (C FFI Ddes)
    // 而 Base64 编码的数据则使用标准 DES ECB
    let mut decrypted_data = encrypted_data;

    if is_hex {
        // API Hex QRC 使用三重 DES (Decrypt-Encrypt-Decrypt) 三把不同的 key
        // 参考: lib_qrc_decoder.cpp from xmcp/QRCD
        let mut key1 = *b"!@#)(NHLiuy*$%^&";
        let mut key2 = *b"123ZXC!@#)(*$%^&";
        let mut key3 = *b"!@#)(*$%^&abcDEF";
        let data_len = decrypted_data.len() as c_int;
        unsafe {
            Ddes(decrypted_data.as_mut_ptr(), key1.as_mut_ptr(), data_len);
            des(decrypted_data.as_mut_ptr(), key2.as_mut_ptr(), data_len);
            Ddes(decrypted_data.as_mut_ptr(), key3.as_mut_ptr(), data_len);
        }
    } else {
        // 标准 DES ECB 模式解密 (用于 Base64 编码的数据)
        let key_bytes = b"!@#)(*$^";
        let key = GenericArray::from_slice(key_bytes);
        let cipher = Des::new(key);
        for chunk in decrypted_data.chunks_mut(8) {
            if chunk.len() == 8 {
                let block = GenericArray::from_mut_slice(chunk);
                cipher.decrypt_block(block);
            }
        }
    }

    // 3. Zlib Decompress
    let mut zlib_decoder = ZlibDecoder::new(&decrypted_data[..]);
    let mut decompressed_data = String::new();
    
    match zlib_decoder.read_to_string(&mut decompressed_data) {
        Ok(_) => Ok(decompressed_data),
        Err(e) => {
            Err(anyhow::anyhow!("Zlib decompression failed: {}", e))
        }
    }
}


/// QMCv1 固定 128 字节密钥表（QQ 音乐本地文件 QMC XOR 层）
const QMC1_KEY: [u8; 128] = [
    0xc3,0x4a,0xd6,0xca,0x90,0x67,0xf7,0x52,0xd8,0xa1,0x66,0x62,0x9f,0x5b,0x09,0x00,
    0xc3,0x5e,0x95,0x23,0x9f,0x13,0x11,0x7e,0xd8,0x92,0x3f,0xbc,0x90,0xbb,0x74,0x0e,
    0xc3,0x47,0x74,0x3d,0x90,0xaa,0x3f,0x51,0xd8,0xf4,0x11,0x84,0x9f,0xde,0x95,0x1d,
    0xc3,0xc6,0x09,0xd5,0x9f,0xfa,0x66,0xf9,0xd8,0xf0,0xf7,0xa0,0x90,0xa1,0xd6,0xf3,
    0xc3,0xf3,0xd6,0xa1,0x90,0xa0,0xf7,0xf0,0xd8,0xf9,0x66,0xfa,0x9f,0xd5,0x09,0xc6,
    0xc3,0x1d,0x95,0xde,0x9f,0x84,0x11,0xf4,0xd8,0x51,0x3f,0xaa,0x90,0x3d,0x74,0x47,
    0xc3,0x0e,0x74,0xbb,0x90,0xbc,0x3f,0x92,0xd8,0x7e,0x11,0x13,0x9f,0x23,0x95,0x5e,
    0xc3,0x00,0x09,0x5b,0x9f,0x62,0x66,0xa1,0xd8,0x52,0xf7,0x67,0x90,0xca,0xd6,0x4a,
];

/// QQ 音乐本地 QRC 文件 11 字节魔法头
const QMC_MAGIC: [u8; 11] = [0x98,0x25,0xB0,0xAC,0xE3,0x02,0x83,0x68,0xE8,0xFC,0x6C];

/// 本地 QRC 文件的 DES 密钥（8字节，不同于在线 API 的16字节）
const LOCAL_KEY1: [u8; 8] = *b"!@#)(NHL";
const LOCAL_KEY2: [u8; 8] = *b"123ZXC!@";
const LOCAL_KEY3: [u8; 8] = *b"!@#)(*$%";

/// QMC XOR 解密 + 剥离魔法头
fn qmc_xor_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for (i, &b) in data.iter().enumerate() {
        let idx = if i <= 0x7FFF { i & 0x7F } else { (i % 0x7FFF) & 0x7F };
        out.push(b ^ QMC1_KEY[idx]);
    }
    out[11..].to_vec()
}

/// 本地文件的三重 DES（buggy DES，C FFI）
fn local_triple_des(data: &[u8]) -> Vec<u8> {
    let mut buf = data.to_vec();
    let len = buf.len() as c_int;
    let mut k1 = LOCAL_KEY1.to_vec();
    let mut k2 = LOCAL_KEY2.to_vec();
    let mut k3 = LOCAL_KEY3.to_vec();
    unsafe {
        Ddes(buf.as_mut_ptr(), k1.as_mut_ptr(), len);
        des(buf.as_mut_ptr(), k2.as_mut_ptr(), len);
        Ddes(buf.as_mut_ptr(), k3.as_mut_ptr(), len);
    }
    buf
}

// Function to decrypt QRC from raw binary file (no Base64 wrapping)
pub fn decode_qrc_from_file(path: &Path) -> Result<String> {
    let raw = std::fs::read(path)
        .context(format!("Failed to read QRC file: {:?}", path))?;

    if raw.is_empty() {
        return Ok(String::new());
    }

    // 新版本地文件：QMC XOR + 三重 DES + zlib
    if raw.len() >= 11 && raw[..11] == QMC_MAGIC {
        let xored = qmc_xor_decode(&raw);
        let decrypted = local_triple_des(&xored);
        return zlib_decompress(&decrypted)
            .context(format!("QMC+zlib decompression failed for file {:?}", path));
    }

    // 旧版本地文件回退（单 Ddes，可能已废弃）
    let mut data = raw;
    let mut key = *b"!@#)(*$^";
    let data_len = data.len() as c_int;
    unsafe {
        Ddes(data.as_mut_ptr(), key.as_mut_ptr(), data_len);
    }
    zlib_decompress(&data)
        .context(format!("Legacy zlib decompression failed for file {:?}", path))
}

fn zlib_decompress(data: &[u8]) -> Result<String> {
    let mut zlib_decoder = ZlibDecoder::new(data);
    let mut decompressed_data = String::new();
    zlib_decoder.read_to_string(&mut decompressed_data)?;
    Ok(decompressed_data)
}

// Parse the decoded XML string into structured QRC data
pub fn parse_qrc_xml(xml_content: &str) -> Result<Vec<QrcLine>> {
    // First try standard XML parsing
    if let Ok(root) = Element::parse(xml_content.as_bytes()) {
        let mut lines = Vec::new();

        // Strategy 1: Direct LyricLine + LyricWord structure
        for child in &root.children {
            if let Some(element) = child.as_element() {
                if element.name == "LyricLine" {
                    let content = element.attributes.get("LyricContent").cloned().unwrap_or_default();
                    let start_time_ms = element.attributes.get("StartTime").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let duration_ms = element.attributes.get("Duration").and_then(|s| s.parse().ok()).unwrap_or(0);
                    
                    let mut words = Vec::new();
                    for word_child in &element.children {
                        if let Some(word_elem) = word_child.as_element() {
                            if word_elem.name == "LyricWord" {
                                let word_content = word_elem.attributes.get("LyricContent").cloned().unwrap_or_default();
                                let word_start: u64 = word_elem.attributes.get("StartTime").and_then(|s| s.parse().ok()).unwrap_or(0);
                                let word_duration = word_elem.attributes.get("Duration").and_then(|s| s.parse().ok()).unwrap_or(0);
                                
                                // LyricWord.StartTime 是绝对时间（相对于歌曲开头）
                                words.push(QrcWord {
                                    content: word_content,
                                    start_time_ms: word_start,
                                    duration_ms: word_duration,
                                });
                            }
                        }
                    }
                    
                    lines.push(QrcLine {
                        content,
                        start_time_ms,
                        duration_ms,
                        words,
                    });
                }
            }
        }

        // Strategy 2: Lyric_* nodes with LyricContent as QRC text
        if lines.is_empty() {
            fn find_lyric_content_recursive(elem: &Element) -> Option<String> {
                if elem.name.starts_with("Lyric_") || elem.name.starts_with("Lyric") {
                    if let Some(content) = elem.attributes.get("LyricContent") {
                        if !content.is_empty() {
                            return Some(content.clone());
                        }
                    }
                }
                for child in &elem.children {
                    if let Some(child_elem) = child.as_element() {
                        if let Some(found) = find_lyric_content_recursive(child_elem) {
                            return Some(found);
                        }
                    }
                }
                None
            }

            if let Some(qrc_text) = find_lyric_content_recursive(&root) {
                lines = parse_qrc_text(&qrc_text);
            }
        }

        if !lines.is_empty() {
            return Ok(lines);
        }
    }

    // Strategy 3 (Fallback): XML parsing failed (e.g. unescaped quotes in LyricContent).
    // Extract LyricContent value directly from raw string.
    let marker = "LyricContent=\"";
    if let Some(start_idx) = xml_content.find(marker) {
        let content_start = start_idx + marker.len();
        // Find the closing pattern: either `"/>` or `">\n</` at the end of the attribute
        // Since the content itself may contain `"`, we search for `"/>` from the end
        if let Some(end_offset) = xml_content[content_start..].rfind("\"/>") {
            let qrc_text = &xml_content[content_start..content_start + end_offset];
            let lines = parse_qrc_text(qrc_text);
            if !lines.is_empty() {
                return Ok(lines);
            }
        }
    }

    Err(anyhow::anyhow!("No QRC data could be parsed from XML"))
}


// Parse QRC text format: [start_ms,duration_ms]字(word_start,word_dur)字(word_start,word_dur)...
// This format is used by QRC lyrics embedded in API XML's LyricContent attribute
pub fn parse_qrc_text(qrc_text: &str) -> Vec<QrcLine> {
    let mut lines = Vec::new();

    for raw_line in qrc_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() { continue; }

        // Match line header: [start_ms,duration_ms]
        if !line.starts_with('[') { continue; }
        let bracket_end = match line.find(']') {
            Some(pos) => pos,
            None => continue,
        };
        let header = &line[1..bracket_end];
        let parts: Vec<&str> = header.split(',').collect();
        if parts.len() != 2 { continue; }

        let start_time_ms: u64 = match parts[0].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let duration_ms: u64 = match parts[1].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let body = &line[bracket_end + 1..];
        let mut words = Vec::new();
        let mut content = String::new();
        let mut i = 0;
        let chars: Vec<char> = body.chars().collect();

        while i < chars.len() {
            // Look for word text followed by (word_start,word_dur)
            let mut word_text = String::new();

            // Collect characters until we think we found a timing block start '('
            while i < chars.len() {
                if chars[i] == '(' {
                    // Check if this is a valid timing block: (start,duration)
                    let mut j = i + 1;
                    let mut timing_str = String::new();
                    while j < chars.len() && chars[j] != ')' {
                        timing_str.push(chars[j]);
                        j += 1;
                    }

                    if j < chars.len() && chars[j] == ')' {
                        let timing_parts: Vec<&str> = timing_str.split(',').collect();
                        if timing_parts.len() == 2 && timing_parts.iter().all(|s| s.trim().chars().all(|c| c.is_ascii_digit())) {
                            // Valid timing block found, stop collecting word text
                            break;
                        }
                    }
                }
                word_text.push(chars[i]);
                i += 1;
            }

            if i < chars.len() && chars[i] == '(' {
                // Parse (start_ms,duration_ms)
                i += 1; // skip '('
                let mut timing = String::new();
                while i < chars.len() && chars[i] != ')' {
                    timing.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() { i += 1; } // skip ')'

                let timing_parts: Vec<&str> = timing.split(',').collect();
                if timing_parts.len() == 2 {
                    let word_start: u64 = timing_parts[0].trim().parse().unwrap_or(0);
                    let word_duration: u64 = timing_parts[1].trim().parse().unwrap_or(0);

                    content.push_str(&word_text);
                    // 文本格式字时间是绝对时间（相对于歌曲开头），与 XML 格式一致
                    words.push(QrcWord {
                        content: word_text,
                        start_time_ms: word_start,
                        duration_ms: word_duration,
                    });
                }
            } else if !word_text.is_empty() {
                // Trailing text without timing (or we hit end of line)
                content.push_str(&word_text);
            }
        }

        if !words.is_empty() {
            lines.push(QrcLine {
                content,
                start_time_ms,
                duration_ms,
                words,
            });
        }
    }

    lines
}

// Extract plain LRC text from XML (<Lyric_n LyricContent="...">) if it's an XML
pub fn extract_lrc_from_xml(xml_content: &str) -> Option<String> {
    let root = Element::parse(xml_content.as_bytes()).ok()?;
    
    // Collect all Lyric_* elements by traversing the tree recursively
    fn find_lyric_elements<'a>(elem: &'a Element) -> Vec<&'a Element> {
        let mut results = Vec::new();
        if elem.name.starts_with("Lyric_") || elem.name.starts_with("Lyric") {
            results.push(elem);
        }
        for child in &elem.children {
            if let Some(child_elem) = child.as_element() {
                results.extend(find_lyric_elements(child_elem));
            }
        }
        results
    }

    let lyric_elements = find_lyric_elements(&root);

    for element in lyric_elements {
        if let Some(content_b64) = element.attributes.get("LyricContent") {
            if let Ok(decoded_bytes) = STANDARD.decode(content_b64) {
                if let Ok(text) = String::from_utf8(decoded_bytes) {
                    return Some(text);
                }
            }
            // Fallback: return raw content if base64 decode fails
            return Some(content_b64.clone());
        }
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_qrc_text_with_brackets() {
        let qrc_text = "[0,5420]大(0,361)笨(361,361)钟(722,361) (1083,361)-(1444,361) (1805,361)周(2166,361)杰(2527,361)伦(2888,361) (3249,361)((3610,361)Jay(3971,361) (4332,361)Chou(4693,361))(5054,361)";
        let lines = parse_qrc_text(qrc_text);
        
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        
        // 期望内容包含括号
        assert_eq!(line.content, "大笨钟 - 周杰伦 (Jay Chou)");
        
        // 检查 word 级别的内容
        assert_eq!(line.words[line.words.len() - 5].content, "(");
        assert_eq!(line.words[line.words.len() - 4].content, "Jay");
        assert_eq!(line.words[line.words.len() - 1].content, ")");
    }

    #[test]
    fn test_parse_qrc_text_normal() {
        let qrc_text = "[100,200]Hello(100,50)World(150,50)";
        let lines = parse_qrc_text(qrc_text);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].content, "HelloWorld");
    }

    #[test]
    fn test_decode_local_qrc_bruteforce() {
        let test_paths = [
            r"D:\QQMusicCache\QQMusicLyricNew\林俊杰 - 修炼爱情 - 287 - 因你 而在_qm.qrc",
            r"D:\QQMusicCache\QQMusicLyricNew\The Chainsmokers_Amy Shark - The Reaper - 182 - World War Joy (Explicit)_qm.qrc",
        ];

        // 多种 key
        let keys_8: [(&str, &[u8]); 6] = [
            ("!@#)(*$^", b"!@#)(*$^"),
            ("!@#)(NHL", b"!@#)(NHL"),
            ("123ZXC!@", b"123ZXC!@"),
            ("!@#)(*$%", b"!@#)(*$%"),
            ("!@#)(NHLiuy*$%^&", b"!@#)(NHLiuy*$%^&"), // 16字节，取前8
            ("qqmusic!", b"qqmusic!"),
        ];

        for test_path in &test_paths {
            let path = std::path::Path::new(test_path);
            if !path.exists() {
                eprintln!("\n[test] 跳过: 文件不存在 — {}", test_path);
                continue;
            }
            let raw = match std::fs::read(path) {
                Ok(d) => d,
                Err(e) => { eprintln!("[test] 读取失败: {} — {}", test_path, e); continue; }
            };

            eprintln!("\n[test] 文件: {} ({} bytes)", test_path, raw.len());
            eprintln!("  head(32): {}", raw.iter().take(32).map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));

            for (kname, key) in &keys_8 {
                // 1. 单次 Ddes（C FFI 解密）
                {
                    let mut data = raw.clone();
                    let mut k = key.to_vec();
                    unsafe { Ddes(data.as_mut_ptr(), k.as_mut_ptr(), data.len() as c_int); }
                    check_decompress(&format!("Ddes_{}", kname), &data);
                }
                // 2. 单次 des（C FFI 加密）
                {
                    let mut data = raw.clone();
                    let mut k = key.to_vec();
                    unsafe { des(data.as_mut_ptr(), k.as_mut_ptr(), data.len() as c_int); }
                    check_decompress(&format!("des_{}", kname), &data);
                }
                // 3. Rust 标准 DES 解密（ECB）
                {
                    let mut data = raw.clone();
                    let generic_key = GenericArray::from_slice(&key[..8.min(key.len())]);
                    let cipher = Des::new(generic_key);
                    for chunk in data.chunks_mut(8) {
                        if chunk.len() == 8 {
                            cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
                        }
                    }
                    check_decompress(&format!("RustDES_{}", kname), &data);
                }
            }

            // 5. 三重 DES 的各种排列（用最常见的三把 key）
            for (k1n, k1) in &keys_8[..3] {
                for (k2n, k2) in &keys_8[..3] {
                    for (k3n, k3) in &keys_8[..3] {
                        // 6种操作排列: D=decrypt, E=encrypt
                        let ops = [
                            (vec!["Ddes", "des", "Ddes"], vec![k1, k2, k3]),
                            (vec!["des", "Ddes", "des"], vec![k1, k2, k3]),
                            (vec!["Ddes", "Ddes", "Ddes"], vec![k1, k2, k3]),
                            (vec!["des", "des", "des"], vec![k1, k2, k3]),
                        ];
                        for (op_names, op_keys) in &ops {
                            let mut data = raw.clone();
                            for (op, ok) in op_names.iter().zip(op_keys.iter()) {
                                let mut k = ok.to_vec();
                                let dlen = data.len() as c_int;
                                unsafe {
                                    if *op == "Ddes" {
                                        Ddes(data.as_mut_ptr(), k.as_mut_ptr(), dlen);
                                    } else {
                                        des(data.as_mut_ptr(), k.as_mut_ptr(), dlen);
                                    }
                                }
                            }
                            let label = format!("{}({}){}({}){}({})", op_names[0], k1n, op_names[1], k2n, op_names[2], k3n);
                            check_decompress(&label, &data);
                        }
                    }
                }
            }

            // 6. 跳过文件头再解压
            for skip in &[4usize, 8, 11, 12, 16, 20, 24, 32] {
                if raw.len() > *skip {
                    check_decompress(&format!("raw_skip{}", skip), &raw[*skip..]);
                }
            }
        }
    }

    /// 对比同一首歌的在线 QRC 原始数据与本地文件，找出格式差异
    #[test]
    fn test_compare_online_vs_local_qrc() {
        let test_path = r"D:\QQMusicCache\QQMusicLyricNew\林俊杰 - 修炼爱情 - 287 - 因你 而在_qm.qrc";
        let song_title = "修炼爱情";
        let song_artist = "林俊杰";

        // 1. 读取本地文件
        let path = std::path::Path::new(test_path);
        let local_bytes = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => { eprintln!("[test] 本地文件读取失败: {}", e); return; }
        };

        eprintln!("[test] === 本地文件 ===");
        eprintln!("  大小: {} bytes", local_bytes.len());
        eprintln!("  head(64): {}", local_bytes.iter().take(64).map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));
        eprintln!("  tail(32): {}", local_bytes.iter().rev().take(32).collect::<Vec<_>>().iter().rev().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));

        // 2. 从在线 API 获取 QRC 原始 hex 字符串
        let rt = tokio::runtime::Runtime::new().unwrap();
        let fetcher = crate::lyrics::LyricFetcher::with_debug(true);
        let (lyrics, _trans, qrc_hex, _pic_url) = match rt.block_on(fetcher.fetch_lyrics(song_title, song_artist)) {
            Ok(r) => r,
            Err(e) => { eprintln!("[test] 在线获取失败: {}", e); return; }
        };

        eprintln!("\n[test] === 在线 API ===");
        eprintln!("  QRC hex长度: {} chars", qrc_hex.len());
        eprintln!("  LRC lyrics长度: {} chars", lyrics.len());
        if !lyrics.is_empty() {
            eprintln!("  LRC 前120字: {}", &lyrics[..120.min(lyrics.len())]);
        }

        // 3. 在线 QRC hex → 二进制
        let online_bytes = if !qrc_hex.is_empty() {
            let is_hex = qrc_hex.chars().all(|c| c.is_ascii_hexdigit());
            if is_hex {
                match hex::decode(&qrc_hex) {
                    Ok(b) => {
                        eprintln!("\n[test] === 在线 QRC hex→二进制 ===");
                        eprintln!("  大小: {} bytes", b.len());
                        eprintln!("  head(64): {}", b.iter().take(64).map(|x| format!("{:02X}", x)).collect::<Vec<_>>().join(" "));
                        eprintln!("  tail(32): {}", b.iter().rev().take(32).collect::<Vec<_>>().iter().rev().map(|x| format!("{:02X}", x)).collect::<Vec<_>>().join(" "));
                        Some(b)
                    }
                    Err(e) => { eprintln!("  hex decode失败: {}", e); None }
                }
            } else {
                eprintln!("  非hex格式: {}...", &qrc_hex[..40.min(qrc_hex.len())]);
                None
            }
        } else {
            eprintln!("\n[test] 在线 QRC 为空");
            None
        };

        // 4. 对比
        if let Some(ref online) = online_bytes {
            eprintln!("\n[test] === 对比 ===");
            eprintln!("  在线大小: {} bytes  本地大小: {} bytes", online.len(), local_bytes.len());
            let min_len = online.len().min(local_bytes.len());
            let xor_head: Vec<u8> = online.iter().zip(local_bytes.iter()).take(min_len.min(64)).map(|(a,b)| a^b).collect();
            eprintln!("  XOR head(64): {}", xor_head.iter().map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));
            // 统计相同率
            let same = online.iter().zip(local_bytes.iter()).take(min_len).filter(|(a,b)| a==b).count();
            eprintln!("  前{}字节相同率: {:.1}%", min_len, same as f64 / min_len as f64 * 100.0);
        }

        // 5. 在线 QRC 解密（验证 3DES 对在线数据有效）
        if let Some(ref online) = online_bytes {
            eprintln!("\n[test] === 在线 QRC 解密验证 ===");
            let mut data = online.clone();
            let mut key1 = *b"!@#)(NHLiuy*$%^&";
            let mut key2 = *b"123ZXC!@#)(*$%^&";
            let mut key3 = *b"!@#)(*$%^&abcDEF";
            let dlen = data.len() as i32;
            unsafe {
                Ddes(data.as_mut_ptr(), key1.as_mut_ptr(), dlen);
                des(data.as_mut_ptr(), key2.as_mut_ptr(), dlen);
                Ddes(data.as_mut_ptr(), key3.as_mut_ptr(), dlen);
            }
            eprintln!("  解密后 head(32): {}", data.iter().take(32).map(|b| format!("{:02X}", b)).collect::<Vec<_>>().join(" "));
            if let Ok(xml) = try_decompress(&data, false) {
                eprintln!("  ✅ zlib解压成功! 前200字: {}", &xml[..200.min(xml.len())]);
            } else if let Ok(xml) = try_decompress(&data, true) {
                eprintln!("  ✅ deflate解压成功! 前200字: {}", &xml[..200.min(xml.len())]);
            } else {
                eprintln!("  ❌ zlib/deflate均失败");
            }
        }
    }
}

fn check_decompress(label: &str, data: &[u8]) {
    if data.len() < 4 { return; }
    if let Ok(s) = try_decompress(data, false) {
        eprintln!("  ✅ {} → zlib 成功! 前100字: {}", label, &s[..100.min(s.len())]);
    }
    if let Ok(s) = try_decompress(data, true) {
        eprintln!("  ✅ {} → deflate 成功! 前100字: {}", label, &s[..100.min(s.len())]);
    }
}

fn try_decompress(data: &[u8], raw_deflate: bool) -> Result<String> {
    if raw_deflate {
        let mut d = DeflateDecoder::new(data);
        let mut s = String::new();
        d.read_to_string(&mut s)?;
        Ok(s)
    } else {
        let mut d = ZlibDecoder::new(data);
        let mut s = String::new();
        d.read_to_string(&mut s)?;
        Ok(s)
    }
}


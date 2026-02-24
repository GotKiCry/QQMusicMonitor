use anyhow::{Result, Context};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use des::Des;
use des::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use flate2::read::ZlibDecoder;
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


// Function to decrypt QRC from raw binary file (no Base64 wrapping)
pub fn decode_qrc_from_file(path: &Path) -> Result<String> {
    let encrypted_data = std::fs::read(path)
        .context(format!("Failed to read QRC file: {:?}", path))?;

    if encrypted_data.is_empty() {
        return Ok(String::new());
    }

    // 本地缓存使用了不同的 DES 算法（带有逻辑 Bug 的魔改版）
    // 因此我们通过 FFI 调用原 C 代码实现
    let mut decrypted_data = encrypted_data;
    let mut key = *b"!@#)(*$^"; // The 8 byte key

    unsafe {
        Ddes(
            decrypted_data.as_mut_ptr(),
            key.as_mut_ptr(),
            decrypted_data.len() as c_int,
        );
    }

    // Zlib Decompress
    let mut zlib_decoder = ZlibDecoder::new(&decrypted_data[..]);
    let mut decompressed_data = String::new();

    match zlib_decoder.read_to_string(&mut decompressed_data) {
        Ok(_) => Ok(decompressed_data),
        Err(e) => Err(anyhow::anyhow!("Zlib decompression failed for file {:?}: {}", path, e)),
    }
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
                                let word_start = word_elem.attributes.get("StartTime").and_then(|s| s.parse().ok()).unwrap_or(0);
                                let word_duration = word_elem.attributes.get("Duration").and_then(|s| s.parse().ok()).unwrap_or(0);
                                
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
}


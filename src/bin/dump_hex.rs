// Test binary to verify Triple DES decryption for API hex QRC data
use std::ffi::c_int;

extern "C" {
    fn Ddes(buff: *mut u8, key: *mut u8, len: c_int) -> c_int;
    fn des(buff: *mut u8, key: *mut u8, len: c_int) -> c_int;
}

fn main() {
    let hex_str = std::fs::read_to_string("lyric_raw.txt").expect("read lyric_raw.txt");
    let hex_str = hex_str.trim();
    
    let encrypted = hex::decode(hex_str).expect("hex decode");
    println!("Encrypted data length: {} bytes", encrypted.len());
    println!("First 16 encrypted bytes: {:02X?}", &encrypted[..16.min(encrypted.len())]);

    // Triple DES: Ddes(KEY1) -> des(KEY2) -> Ddes(KEY3)
    let mut data = encrypted.clone();
    let mut key1 = *b"!@#)(NHLiuy*$%^&";
    let mut key2 = *b"123ZXC!@#)(*$%^&";
    let mut key3 = *b"!@#)(*$%^&abcDEF";
    let data_len = data.len() as c_int;
    
    unsafe {
        Ddes(data.as_mut_ptr(), key1.as_mut_ptr(), data_len);
        des(data.as_mut_ptr(), key2.as_mut_ptr(), data_len);
        Ddes(data.as_mut_ptr(), key3.as_mut_ptr(), data_len);
    }
    
    println!("\n[Triple DES] First 16 decrypted bytes: {:02X?}", &data[..16.min(data.len())]);
    if data[0] == 0x78 {
        println!("[Triple DES] ✓ Valid Zlib header detected!");
        
        // Try Zlib decompression
        use std::io::Read;
        use flate2::read::ZlibDecoder;
        let mut decoder = ZlibDecoder::new(&data[..]);
        let mut result = String::new();
        match decoder.read_to_string(&mut result) {
            Ok(_) => {
                println!("[Triple DES] ✓ Zlib decompression succeeded!");
                println!("Decompressed length: {} bytes", result.len());
                // Print first 200 chars
                let preview: String = result.chars().take(200).collect();
                println!("Preview:\n{}", preview);
            }
            Err(e) => {
                println!("[Triple DES] ✗ Zlib decompression failed: {}", e);
            }
        }
    } else {
        println!("[Triple DES] ✗ No Zlib header (expected 0x78, got 0x{:02X})", data[0]);
    }
}

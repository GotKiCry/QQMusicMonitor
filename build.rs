fn main() {
    println!("cargo:rerun-if-changed=src/qq_des/des.c");
    println!("cargo:rerun-if-changed=src/qq_des/QQMusicCommon.c");
    
    cc::Build::new()
        .file("src/qq_des/des.c")
        .file("src/qq_des/QQMusicCommon.c")
        .compile("qqmusicdes");
}

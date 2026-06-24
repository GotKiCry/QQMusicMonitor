fn main() {
    cc::Build::new()
        .file("src/qq_des/des.c")
        .file("src/qq_des/QQMusicCommon.c")
        .compile("qq_des");
    tauri_build::build();
}

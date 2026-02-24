# QQMusic Monitor 项目文档

## 1. 项目概览
**QQMusic Monitor** 是一个轻量级、高性能的 Windows 命令行工具，专用于实时监控 QQ 音乐客户端的播放状态。它通过读取内存的方式，直接获取当前播放歌曲的详细信息，并提供多种输出格式以满足直播推流、歌词显示或二次开发的需求。

## 2. 核心功能
*   **实时监控**：毫秒级响应歌曲切换和播放进度变化。
*   **多维度信息**：支持获取歌曲名、歌手、专辑、歌词、当前播放时间、总时长等。
*   **灵活输出**：
    *   **控制台 (CLI)**：带有实时进度条的直观显示。
    *   **文本文件 (`now_playing.txt`)**：单行文本输出，方便 OBS 等直播软件直接读取。
    *   **JSON 文件 (`now_playing.json`)**：结构化数据输出，便于网页组件或其他程序调用。
*   **高度可配置**：支持自定义扫描频率、内存偏移量、输出文件名等。
*   **低资源占用**：基于 Rust 开发，内存占用极低，无运行时依赖。

## 3. 技术架构

### 3.1 技术栈
*   **编程语言**：Rust (2021 Edition)
*   **平台**：Windows (x86/x64)
*   **关键库**：
    *   `winapi` / `ntapi`：调用 Windows API 进行进程查找和内存读取。
    *   `tokio`：异步运行时，处理并发任务。
    *   `crossterm`：跨平台终端操作，实现无闪烁 UI 刷新。
    *   `serde` / `serde_json`：数据序列化与 JSON 处理。
    *   `clap`：命令行参数解析。
    *   `anyhow`：错误处理。

### 3.2 实现原理
1.  **进程发现**：使用 Windows ToolHelp32 快照 API 查找 `QQMusic.exe` 进程 ID。
2.  **基址获取**：定位 `QQMusic.dll` 模块在内存中的基址 (Base Address)。
3.  **指针链解析**：
    *   通过预定义的**基址偏移量 (Base Offset)** 读取一级指针。
    *   根据**指针链 (Pointer Chain)** 逐级解引用，最终定位到存放歌曲信息的动态内存地址。
    *   这种方式能有效应对程序重启后内存地址变化的问题。
4.  **数据读取与解码**：
    *   从目标地址读取 UTF-16 LE 编码的字符串（QQ 音乐内部使用 Unicode）。
    *   将其转换为 UTF-8 字符串供 Rust 程序使用。
5.  **状态同步**：主循环定期轮询内存，对比数据变化，如有更新则刷新显示并写入文件。

## 4. 快速开始

### 4.1 环境要求
*   Windows 10/11
*   QQ 音乐客户端 (PC版)
*   (可选) Rust 开发环境

### 4.2 运行方式
```bash
# 基本运行（使用默认配置）
cargo run --release

# 指定配置文件
cargo run --release -- -c config_v2.toml

# 调试模式（输出详细内存读取日志）
cargo run --release -- --debug
```

### 4.3 配置文件 (`config.toml`)
```toml
[memory_offsets]
# 核心偏移量，随 QQ 音乐版本更新可能需要调整
song_name_offset = 0x00B6975C
song_singer_offset = 0x00B69760
...

[settings]
update_interval_ms = 500  # 刷新频率
process_name = "QQMusic.exe"
module_name = "QQMusic.dll"
output_txt = true         # 是否生成 txt 文件
output_json = true        # 是否生成 json 文件
```

## 5. 项目结构
```
src/
├── main.rs          # 程序入口，主循环与线程管理
├── config.rs        # 配置加载与解析
├── process.rs       # Windows 进程操作封装
├── memory.rs        # 内存读取与指针链解析核心逻辑
├── song_info.rs     # 歌曲信息数据结构定义
└── cli.rs           # 命令行参数定义
```

## 6. 常见问题
*   **读不到数据？**
    *   请确保以**管理员身份**运行终端。
    *   QQ 音乐版本更新可能导致偏移量失效，需使用 Cheat Engine 重新定位并更新 `config.toml`。
*   **乱码？**
    *   程序已处理 UTF-16 转 UTF-8，请确保终端支持 UTF-8 显示（Windows CMD 可尝试 `chcp 65001`）。

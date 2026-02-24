# QQMusic Monitor

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Windows-brightgreen.svg)
![Rust](https://img.shields.io/badge/language-Rust-orange.svg)

一个强大的实时读取 QQ 音乐播放信息的工具，支持通过多种方式（内存 Hook 与 Windows SMTC）获取歌曲名、歌手、专辑、歌词以及播放进度等信息。

## 功能特性

- 🎵 **双模式监控**：内置传统的内存读取模式（通过 `config.toml` 配置）与全新的 **Windows 系统媒体传输控制 (SMTC)** 接口支持
- 📊 **高精度进度**：实时显示播放进度条、当前时间和总时长，并支持 QRC 逐字高亮显示
- 📝 **智能歌词系统**：
  - 支持从网络 API 获取 LRC / 翻译 / QRC 数据
  - **本地缓存优先**：自动探测并优先读取 QQ 音乐本地 `qrc` 缓存目录下的加密歌词
  - **实时逐字同步**：在控制台实现字级高亮的滚动歌词效果
- 🔄 **零延迟刷新**：采用 `crossterm` 实现无闪烁的终端 UI 体验
- 📄 **多渠道输出**：
  - `now_playing.txt` & `now_playing.json`：完整的歌曲元数据
  - `current_lyric.txt`：**实时同步**的当前歌词行（支持 BOM UTF-16 LE，适配 OBS 等工具）
- ⚙️ **高度可配置**：支持详细的配置文件控制、内存偏移量调整以及丰富的命令行参数

## 显示效果

```text
歌曲名 - 歌手
专辑: 专辑名称
█████████████░░░░░ 01:23 / 03:45 [36.8%]

正在播放的逐字高亮歌词（当前播放部分会实时高亮）
这是对应的翻译行
🕐 14:30:25
```

## 安装与运行

### 环境要求
- Windows 10/11 操作系统 (SMTC 特性需要)
- [Rust](https://www.rust-lang.org/) 工具链 (1.75+)
- QQ 音乐客户端（已登录且正在播放）

### 快速开始
1. 克隆并进入目录：
   ```bash
   git clone https://github.com/GotKiCry/QQMusicMonitor.git
   cd QQMusicMonitor
   ```
2. 构建并运行：
   ```bash
   cargo run --release
   ```

### 常用运行参数
```bash
# 基本运行 (默认优先使用 SMTC 模式)
cargo run

# 静默运行（仅输出文件，不打印控制台 UI）
cargo run -- --quiet

# 自定义更新频率（单位：毫秒）
cargo run -- --interval 200

# 调试模式（查看歌词来源和解密过程）
cargo run -- --debug
```

## 技术实现

### 核心模块
- **`smtc.rs`**：利用 Windows Runtime APIs 调用系统级媒体信息，稳定性较高
- **`qrc.rs`**：核心加解密逻辑，支持 QQ 音乐专有的 QRC 动态歌词解析
- **`local_qrc.rs`**：自动扫描系统缓存目录（如 `AppData\Roaming\Tencent\QQMusic`），读取已下载的本地歌词
- **`lyrics.rs`**：备用的网络聚合歌词搜索与下载引擎

## 文档相关
- [详细项目文档](Project.md)
- [内存偏移量配置指南](config.toml)

## 开发者说明

由于本项目涉及复杂的 Windows API 调用与内存操作，建议在开发环境下通过以下方式调试：
```bash
cargo run --bin qqmusic-monitor -- --debug
```

## 许可证
本项目采用 MIT 许可证。

## 免责声明
本项目仅供技术交流与学习使用，所有技术手段（如内存读取、歌词解密）及其产生的数据包所有权归原服务商所有，请勿用于商业用途或其他违法违规行为。

⚠️ **注意事项**：
1. **项目尚不完善**：本项目目前处于早期开发阶段，可能存在未知的 Bug 或不稳定的情况。欢迎提交 Issue 反馈问题。
2. **兼容性限制**：由于依赖 Windows 特有的 API 或内存偏移量，部分功能可能在特定系统版本或 QQ 音乐版本下失效。
3. **风险自担**：使用者需自行承担因使用本工具可能产生的任何后果。

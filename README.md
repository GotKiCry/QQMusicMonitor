# QQMusic Reader

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Windows-brightgreen.svg)
![Rust](https://img.shields.io/badge/language-Rust-orange.svg)

一个实时读取QQ音乐播放信息的命令行工具，支持获取歌曲名、歌手、专辑、歌词以及播放进度等信息。

## 功能特性

- 🎵 **实时监控**：实时获取QQ音乐当前播放的歌曲信息
- 📊 **播放进度**：显示播放进度条、当前时间和总时长
- 📝 **歌词显示**：获取并显示当前播放歌曲的歌词
- 🔄 **自动刷新**：控制台输出自动刷新，无残留字符
- 📄 **文件输出**：支持将歌曲信息输出到TXT和JSON文件
- ⚙️ **可配置**：支持配置文件和命令行参数
- 🔍 **调试模式**：详细的内存读取调试信息

## 显示效果

```
歌曲名-歌手
专辑:专辑名称
█████████████░░░░░ 01:23 / 03:45 [36.8%]
这是歌词内容
可以有多行显示
🕐 14:30:25
```

## 安装要求

- Windows 操作系统
- [Rust](https://www.rust-lang.org/) 工具链 (1.70+)
- QQ音乐客户端

## 构建和运行

### 1. 克隆项目

```bash
git clone https://github.com/yourusername/QQMusicReader.git
cd QQMusicReader
```

### 2. 构建项目

```bash
cargo build --release
```

### 3. 运行程序

```bash
# 基本运行
cargo run

# 调试模式运行
cargo run -- --debug

# 自定义更新间隔
cargo run -- --interval 1000

# 静默模式（不输出控制台信息）
cargo run -- --quiet
```

## 使用方法

### 命令行参数

```bash
QQMusic Reader - 实时读取QQ音乐播放信息

使用方法: qqmusic-reader [选项]

选项:
  -c, --config <文件>     配置文件路径 (默认: config.toml)
  -d, --debug             启用调试模式
      --no-txt            禁用文本文件输出
      --no-json           禁用JSON文件输出
      --txt-file <文件>   自定义文本输出文件名 (默认: now_playing.txt)
      --json-file <文件>  自定义JSON输出文件名 (默认: now_playing.json)
  -i, --interval <毫秒>   更新间隔 (默认: 500)
  -r, --retries <次数>    最大重试次数 (默认: 3)
      --process <名称>    进程名称 (默认: QQMusic.exe)
      --module <名称>     模块名称 (默认: QQMusic.dll)
  -q, --quiet             静默模式（不输出控制台信息）
  -v, --version           显示版本信息
  -h, --help              显示帮助信息
```

### 配置文件

程序使用 `config.toml` 文件进行配置：

```toml
[memory_offsets]
# 基础偏移量（相对于QQMusic.dll基地址）
song_name_offset = 0x00B6975C
song_singer_offset = 0x00B69760
song_album_offset = 0x00B69764
song_lyrics_offset = 0x00B69768
current_time_offset = 0x00B6976C
total_time_offset = 0x00B69770

# 歌曲信息指针链
song_name_chain = [0x0]
song_singer_chain = [0x0]
song_album_chain = [0x0]
song_lyrics_chain = [0x0]
current_time_chain = [0x0]
total_time_chain = [0x0]

# 字符串字段偏移量
title_offset = 0x0

[settings]
# 更新间隔（毫秒）
update_interval_ms = 500

# 最大重试次数
max_retries = 3

# 进程和模块设置
process_name = "QQMusic.exe"
module_name = "QQMusic.dll"

# 输出文件设置
output_txt = true
output_json = true
txt_filename = "now_playing.txt"
json_filename = "now_playing.json"

# 调试模式
debug_mode = false

# 最大字符串长度限制
max_string_length = 4096
```

## 输出文件

程序会在当前目录生成以下文件：

- `now_playing.txt` - UTF-16 LE格式的歌曲名
- `now_playing.json` - JSON格式的歌曲信息

## 注意事项

⚠️ **重要提醒**：

1. **管理员权限**：请确保以管理员权限运行程序
2. **QQ音乐版本**：内存偏移量可能随QQ音乐版本更新而失效
3. **地址定位**：如果程序无法正常工作，请使用Cheat Engine等工具重新定位内存地址

### 重新定位内存地址

如果偏移量失效，您可以：

1. 使用Cheat Engine等内存扫描工具
2. 在QQ音乐中播放一首歌曲
3. 搜索歌曲名、歌手等字符串在内存中的位置
4. 更新config.toml中的偏移量配置

## 技术实现

### 核心技术

- **内存读取**：使用Windows API读取QQMusic进程内存
- **指针链解析**：支持多级指针链解析
- **实时监控**：多线程实时监控歌曲信息变化
- **终端控制**：使用crossterm库实现无闪烁刷新输出

### 项目结构

```
src/
├── main.rs          # 主程序入口
├── config.rs        # 配置文件处理
├── process.rs       # 进程操作
├── memory.rs        # 内存读取
├── song_info.rs     # 歌曲信息结构
└── cli.rs           # 命令行参数解析
```

## 开发

### 开发环境设置

```bash
# 安装Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 克隆项目
git clone https://github.com/yourusername/QQMusicReader.git
cd QQMusicReader

# 构建项目
cargo build
```

### 运行测试

```bash
cargo test
```

## 许可证

本项目采用 MIT 许可证 - 查看 [LICENSE](LICENSE) 文件了解详情。

## 贡献

欢迎提交 Issue 和 Pull Request！

## 免责声明

本工具仅供学习和研究使用，请遵守相关法律法规。使用者需自行承担使用风险。

## 致谢

- [Rust](https://www.rust-lang.org/) - 强大的系统编程语言
- [crossterm](https://github.com/crossterm-rs/crossterm) - 跨平台终端操作库
- [serde](https://github.com/serde-rs/serde) - 序列化/反序列化框架

---

## 代码生成说明

🤖 **本项目的代码主要使用以下AI工具生成和优化：**

- **Claude Code** - Anthropic 的AI编程助手
- **GLM-4.5** - 智谱AI的大语言模型

### 开发过程

1. **初始架构设计** - 使用AI分析需求并设计项目结构
2. **功能实现** - 基于AI生成的代码实现核心功能
3. **问题解决** - 使用AI调试和修复遇到的问题
4. **代码优化** - AI协助优化代码质量和性能
5. **文档编写** - AI协助生成项目文档

### AI辅助的优势

- **快速开发**：AI帮助快速生成样板代码和核心逻辑
- **问题诊断**：AI协助分析和解决技术问题
- **最佳实践**：AI建议使用Rust的最佳实践和设计模式
- **文档完善**：AI帮助生成详细的代码注释和文档

### 人工参与

虽然代码主要由AI生成，但整个过程需要：
- 需求分析和功能设计
- 代码审查和验证
- 测试和调试
- 配置调整和优化

这种人机协作的开发模式大大提高了开发效率，同时保证了代码质量。

---

**项目主页**: [https://github.com/yourusername/QQMusicReader](https://github.com/yourusername/QQMusicReader)  
**Bug反馈**: [https://github.com/yourusername/QQMusicReader/issues](https://github.com/yourusername/QQMusicReader/issues)
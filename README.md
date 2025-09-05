# QQMusic Reader - QQ音乐信息提取器

![Version](https://img.shields.io/badge/version-v1.0-blue)
![Platform](https://img.shields.io/badge/platform-Windows-blue)
![Language](https://img.shields.io/badge/language-Rust-orange)
![License](https://img.shields.io/badge/license-MIT-green)

QQMusic Reader 是一个专用于 Windows 平台的命令行工具，通过内存读取技术实时获取 QQ 音乐播放状态、歌曲元数据和歌词信息，并将数据输出到本地文件。

## ✨ 主要功能

- 🎵 **实时监控** - 监控 QQ 音乐进程状态
- 📖 **歌曲信息** - 获取当前播放歌曲的标题、艺术家信息
- ⏱️ **播放进度** - 实时显示播放进度和歌曲总时长
- 📝 **歌词显示** - 提取当前歌词内容
- 💾 **多格式输出** - 支持 JSON 和 TXT 格式的本地文件输出
- 🔄 **自动重连** - 自动检测 QQ 音乐重启并重新连接

## 🎯 输出示例

### 控制台输出
```
🎵 QQMusic Reader v1.0
========================================
⚠️  请确保以管理员权限运行！
========================================

🔍 Searching for QQMusic.exe...
✓ Found process 'QQMusic.exe' with PID: 12345
✓ Successfully opened process handle for PID: 12345
✓ Found module 'QQMusic.dll' at base address: 0x7FF123456000

🎵 Now Playing: 夜曲 - 周杰伦
--------------------------------------------------
[01:23] / [04:32] - 一盏离愁 孤单伫立在窗口
```

### JSON 输出文件 (now_playing.json)
```json
{
  "title": "夜曲",
  "artist": "周杰伦",
  "progress_sec": 83.5,
  "duration_sec": 272.0,
  "lyric": "一盏离愁 孤单伫立在窗口"
}
```

### TXT 输出文件 (now_playing.txt)
```
夜曲 - 周杰伦
[01:23] / [04:32]
Progress: 30.7%
Lyric: 一盏离愁 孤单伫立在窗口
```

## 🛠️ 系统要求

### 运行时要求
- **操作系统**: Windows 10/11 (64位)
- **权限**: 必须以管理员权限运行
- **目标软件**: QQ音乐桌面版 (已测试版本)

### 开发环境要求
- **Rust 工具链**: stable-msvc
- **Visual Studio 2022**: C++开发组件和Windows SDK
- **CMake**: 3.15+

## 📦 快速开始

### 方式一：下载预编译版本 (推荐)
1. 从 [Releases](https://github.com/your-repo/releases) 页面下载最新版本
2. 解压到任意目录
3. 以管理员身份运行 `qqmusic-reader.exe`

### 方式二：从源码编译
1. **安装 Rust 工具链**
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   rustup default stable-msvc
   ```

2. **安装 Visual Studio 2022**
   - 确保安装 C++ 开发组件和 Windows SDK

3. **克隆仓库并编译**
   ```bash
   git clone <repository-url>
   cd QQMusicReader
   cargo build --release
   ```

4. **运行程序**
   ```bash
   # 以管理员身份运行
   target\release\qqmusic-reader.exe
   ```

## 🚀 使用指南

### 基本使用步骤

1. **启动 QQ 音乐**
   - 确保 QQ 音乐正在运行
   - 开始播放任意歌曲

2. **运行 QQMusic Reader**
   ```bash
   # 必须以管理员权限运行！
   qqmusic-reader.exe
   ```

3. **查看输出**
   - 控制台会实时显示当前播放信息
   - 同时生成 `now_playing.json` 和 `now_playing.txt` 文件

### 程序行为说明

- **自动重试**: 如果 QQ 音乐未运行，程序会等待并重试连接
- **自动重连**: 如果 QQ 音乐重启，程序会自动重新连接
- **实时更新**: 歌曲信息每 500ms 更新一次
- **智能显示**: 只有在歌曲切换时才会刷新完整信息，其他时候只更新进度

## ⚙️ 配置说明

### 内存地址配置
程序使用硬编码的内存偏移量来读取 QQ 音乐数据：

```rust
// 元数据指针链 (歌曲标题、艺术家、歌词)
const META_BASE_OFFSET: usize = 0xB6A740;
const META_OFFSETS: [usize; 6] = [0x10, 0x58, 0x50, 0x0, 0x18, 0x0];

// 时间数据指针链 (播放进度、总时长)
const TIME_BASE_OFFSET: usize = 0xB6A740;
const TIME_OFFSETS: [usize; 5] = [0x10, 0x58, 0x50, 0x0, 0x8];
```

### 更新频率配置
```rust
const UPDATE_INTERVAL_MS: u64 = 500; // 更新间隔 (毫秒)
const MAX_RETRIES: u32 = 3;          // 最大重试次数
```

## 🔧 故障排除

### 常见问题

#### 1. "权限不足" 错误
**问题**: 程序提示无法打开进程或读取内存
**解决方案**: 
- 确保以管理员身份运行程序
- 右键 `.exe` 文件 → "以管理员身份运行"

#### 2. "进程未找到" 错误
**问题**: 程序找不到 QQMusic.exe 进程
**解决方案**: 
- 确保 QQ 音乐正在运行
- 检查进程名是否为 "QQMusic.exe"
- 重启 QQ 音乐后重试

#### 3. "读取内存失败" 错误
**问题**: 程序无法读取歌曲信息
**可能原因**: 
- QQ 音乐版本更新导致内存布局改变
- 当前没有播放歌曲
- 安全软件拦截了内存访问

**解决方案**: 
- 尝试播放一首歌曲
- 临时关闭安全软件的实时保护
- 如果是版本问题，需要使用 Cheat Engine 等工具重新定位内存地址

#### 4. 显示乱码
**问题**: 歌曲信息显示为乱码
**解决方案**: 
- 确保控制台支持 UTF-8 编码
- 在 cmd 中运行 `chcp 65001` 设置编码

### 兼容性说明

- ✅ **已测试**: QQ音乐 Windows 版本 [具体版本号]
- ❓ **可能兼容**: 其他版本的 QQ 音乐（需要调整内存偏移）
- ❌ **不兼容**: Mac 版本、网页版、手机版

## 📚 技术原理

### 内存读取机制
程序通过以下步骤获取音乐信息：

1. **进程扫描**: 使用 Windows API 扫描系统进程，查找 "QQMusic.exe"
2. **模块定位**: 获取 "QQMusic.dll" 模块的基地址
3. **指针链解析**: 通过预定义的偏移量链追踪内存指针
4. **数据提取**: 读取目标内存地址的 UTF-16 字符串和浮点数据
5. **格式转换**: 将原始数据转换为结构化的歌曲信息

### 架构设计
```
┌─────────────┐    ┌──────────────┐    ┌─────────────┐
│   main.rs   │───▶│  process.rs  │───▶│ QQ音乐进程   │
│   主控制    │    │   进程管理   │    │             │
└─────────────┘    └──────────────┘    └─────────────┘
       │                    │
       ▼                    ▼
┌─────────────┐    ┌──────────────┐
│  memory.rs  │───▶│ QQ音乐内存   │
│  内存读取   │    │   空间      │
└─────────────┘    └──────────────┘
       │
       ▼
┌─────────────┐    ┌──────────────┐
│ song_info.rs│───▶│   输出文件   │
│  数据结构   │    │ JSON / TXT   │
└─────────────┘    └──────────────┘
```

## 🔒 安全考虑

### 权限要求
- 程序需要管理员权限来访问其他进程的内存
- 仅进行内存读取操作，不会修改任何数据
- 不收集或传输任何用户个人信息

### 隐私保护
- 程序只读取音乐播放相关的信息
- 所有数据仅在本地处理和存储
- 不涉及网络传输或数据上传

### 安全风险
- 部分安全软件可能将此程序识别为可疑行为
- 建议在可信环境中使用
- 源代码完全开源，可自行审查

## 🤝 贡献指南

### 报告问题
如果遇到问题，请在 [Issues](https://github.com/your-repo/issues) 页面提交，并包含：
- 操作系统版本
- QQ 音乐版本
- 错误详细信息
- 复现步骤

### 贡献代码
1. Fork 本仓库
2. 创建功能分支 (`git checkout -b feature/amazing-feature`)
3. 提交更changes (`git commit -m 'Add some amazing feature'`)
4. 推送到分支 (`git push origin feature/amazing-feature`)
5. 创建 Pull Request

### 更新内存偏移
如果 QQ 音乐更新导致程序失效，可以：
1. 使用 Cheat Engine 等工具找到新的内存地址
2. 更新 `memory.rs` 中的偏移量常数
3. 提交 Pull Request 帮助其他用户

## 📜 版本历史

### v1.0.0 (2024-xx-xx)
- 🎉 首次发布
- ✨ 支持实时读取 QQ 音乐播放信息
- ✨ 支持 JSON 和 TXT 格式输出
- ✨ 自动重连功能
- ✨ 友好的控制台界面

## 📄 许可证

本项目基于 MIT 许可证开源 - 查看 [LICENSE](LICENSE) 文件了解详情。

## ⚠️ 免责声明

- 本工具仅供学习和个人使用
- 使用本工具产生的任何法律责任由用户自行承担
- QQ 音乐更新可能导致工具失效，需要手动更新内存偏移量
- 请遵守相关软件的使用条款和法律法规

## 🙏 致谢

- 感谢 Rust 社区提供的优秀生态
- 感谢 Windows API 文档和示例
- 感谢所有贡献者和使用者的反馈

---

**如果这个项目对你有帮助，请给个 ⭐ 支持一下！**
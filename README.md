# QQMusic Monitor

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Windows-brightgreen.svg)
![Rust](https://img.shields.io/badge/language-Rust-orange.svg)
![Tauri](https://img.shields.io/badge/framework-Tauri_2-blue.svg)

一个基于 **Tauri 2** 的实时读取 QQ 音乐播放信息的桌面应用，通过 Windows **SMTC（System Media Transport Controls）** API 获取歌曲名、歌手、专辑、歌词以及播放进度，提供玻璃拟态 GUI 界面、60fps 逐字 KTV 扫光歌词，并支持文件输出与 WebSocket 同步。

## 功能特性

- 🎵 **SMTC 实时监控**：通过 Windows 系统媒体传输控制接口读取 QQ 音乐播放状态，稳定性高，无需内存 Hack
- 🖥️ **Tauri GUI 前端**：玻璃拟态桌面界面，唱机组件 + 自动居中滚动歌词 + 60fps 逐字扫光高亮
- 📝 **智能歌词系统**：
  - 多级搜索策略匹配 QQ 音乐在线 API，优先获取逐字 QRC 格式
  - 自动探测 QQ 音乐本地 `qrc` 缓存目录，离线兜底
  - 支持双语对照翻译
- 🎤 **逐字 KTV 高亮**：解析 QQ 音乐专有加密 QRC，实现字级扫光效果（前端 CSS `background-clip: text`，终端 Stylize 渲染）
- ⏱️ **SMTC 滞后补偿**：基于 `LastUpdatedTime` 的漂移修正 + 用户可调偏移量，消除进度闪烁
- 🔄 **切歌瞬间修正**：前后端协同处理 SMTC media properties 与 timeline 异步更新间隙，杜绝切歌后旧歌词残留
- 📄 **多渠道输出**：
  - `now_playing.txt`（UTF-16 LE，适配 OBS）/ `now_playing.json` / `current_lyric.txt`
  - WebSocket `ws://127.0.0.1:3000/ws` 实时同步，供浏览器/OBS/直播场景嵌入
- ⚙️ **高度可配置**：GUI 设置面板实时调整偏移/轮询间隔/端口/输出开关，自动落盘 `config.toml`
- 🌙 **后台降频**：窗口隐藏时自动降低后端轮询频率，节省 CPU

## 显示效果

GUI 界面采用极光午夜设计风格，左侧黑胶唱机组件随播放状态旋转，右侧歌词自动居中滚动并以渐变扫光逐字高亮。支持四种主题色（极光青/罗兰紫/翡翠绿/落日橙）、可调字号字重、双语翻译开关。

## 环境要求

- Windows 10/11 操作系统（SMTC 特性需要）
- [Rust](https://www.rust-lang.org/) 工具链 (1.77+)
- [Node.js](https://nodejs.org/)（仅开发时需要 Tauri CLI）
- QQ 音乐客户端（已登录且正在播放）

## 快速开始

1. 克隆并进入目录：
   ```bash
   git clone https://github.com/GotKiCry/QQMusicMonitor.git
   cd QQMusicMonitor
   ```
2. 安装 Tauri CLI：
   ```bash
   npm install
   ```
3. 开发模式运行（启动 GUI 窗口 + 后端监控 + WebSocket 服务，热重载）：
   ```bash
   cargo tauri dev
   ```
4. 构建可执行文件：
   ```bash
   cargo tauri build
   ```

GUI 窗口启动后自动连接后端；浏览器/外部场景可访问 `http://127.0.0.1:3000` 或连 `ws://127.0.0.1:3000/ws`。

## 常用运行参数

```bash
cargo run --release -- --debug          # 调试模式（详细 QRC 解析日志）
cargo run --release -- --port 8080      # 自定义 WebSocket 服务端口
cargo run --release -- --interval 200   # 自定义 SMTC 轮询间隔（毫秒）
cargo run --release -- --offset 150     # 自定义 SMTC 滞后补偿（毫秒）
cargo run --release -- --no-server      # 禁用 WebSocket 服务
cargo run --release -- --help           # 完整参数列表
```

## 技术实现

### 后端核心模块（`src-tauri/src/`）
- **`smtc.rs`**：Windows SMTC API 封装，基于 `LastUpdatedTime` 的漂移修正算法（匹配 Windows 音量浮窗进度条）
- **`qrc.rs`**：QRC 加解密核心，支持 QQ 音乐专有 DES-CBC 魔改算法 + zlib 解压 + XML/文本解析
- **`local_qrc.rs`**：自动扫描 `AppData\Roaming\Tencent\QQMusic` 缓存目录，读取本地加密歌词
- **`lyrics.rs`**：多级搜索策略的在线歌词获取引擎（含韩文等非 ASCII 标题的修正匹配）
- **`server.rs`**：axum HTTP + WebSocket 广播服务，作为 Tauri event 之外的兜底同步通道
- **`qq_des/`**：C FFI 实现 QQ 音乐魔改 DES 算法

### 前端（`frontend/`，原生 ES Modules，无打包工具）
- **`src/connection/`**：Tauri event 监听（主通道）+ WebSocket 兜底（含指数退避重连）
- **`src/state/`**：全局状态管理 + SongInfo 更新处理 + 切歌重置逻辑
- **`src/lyrics/`**：翻译解析 + 歌词 DOM 构建（word-bg/word-fg 双层剪裁实现逐字扫光）
- **`src/render/`**：60fps requestAnimationFrame 渲染循环（插值 + 逐字剪裁 + translateY 居中滚动）
- **`src/settings/`**：设置抽屉交互（滑块/开关/步进器/主题选择，实时同步到 config.toml）
- **`css/`**：模块化样式（设计 token + 主题 + 布局 + 组件 + 响应式）

## 文档

- [详细项目文档](Project.md) — 架构、数据流、QRC 解密流程、SMTC 时间模型、切歌处理细节

## 开发者说明

调试模式可查看歌词来源、解密过程与同步延迟：

```bash
cargo tauri dev -- --debug
```

前端调试面板（设置 → 开发者选项）可实时查看完整 `SongInfo` JSON 与同步耗时。

## 许可证

本项目采用 MIT 许可证。

## 免责声明

本项目仅供技术交流与学习使用，所有技术手段（如歌词解密）及其产生的数据包所有权归原服务商所有，请勿用于商业用途或其他违法违规行为。

⚠️ **注意事项**：
1. **项目尚不完善**：本项目目前处于早期开发阶段，可能存在未知的 Bug 或不稳定的情况。欢迎提交 Issue 反馈问题。
2. **兼容性限制**：由于依赖 Windows 特有的 SMTC API，部分功能可能在特定系统版本或 QQ 音乐版本下失效。
3. **风险自担**：使用者需自行承担因使用本工具可能产生的任何后果。

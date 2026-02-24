# QQMusic Monitor 项目文档

## 1. 项目概览

**QQMusic Monitor** 是一个 Windows 命令行工具，通过 **SMTC（System Media Transport Controls）** API 实时读取 QQ 音乐客户端的播放状态，并支持：

- **终端 TUI** — 实时显示歌曲信息、进度条、逐字 KTV 高亮歌词
- **文件输出** — `now_playing.txt`（UTF-16 LE，适配 OBS）、`now_playing.json`、`current_lyric.txt`
- **Web 面板** — HTTP + WebSocket 服务，浏览器实时同步播放状态和歌词

## 2. 核心功能

- **毫秒级轮询** — 默认 100ms 间隔读取 SMTC 媒体状态
- **歌词三源获取**：
  1. QQ 音乐在线 API（`musicu.fcg`），优先获取逐字 QRC 格式
  2. QQ 音乐本地缓存文件（`_qm.qrc` / `_qm.lrc`）
  3. LRC 文本格式兜底
- **逐字 KTV 高亮** — 解析 QRC（QQ 音乐专有解密格式），在 TUI 和 Web 面板中实现逐字扫光效果
- **SMTC 滞后补偿** — 200ms 偏移量 + 实时时间插值，消除轮询间隔导致的闪烁
- **多音源过滤** — 只读取 QQ 音乐会话，排除浏览器/其他播放器干扰
- **Web 实时同步** — 内置 HTTP + WebSocket 服务，方便 OBS/直播场景嵌入

## 3. 技术架构

### 3.1 技术栈

| 层级 | 技术 |
|---|---|
| 语言 | **Rust** (2021 Edition) |
| 媒体接口 | **Windows SMTC** (`windows` crate `Media_Control` feature) |
| 异步运行时 | **tokio** |
| Web 服务 | **axum** (HTTP + WebSocket) |
| 终端渲染 | **crossterm** |
| 歌词解密 | **DES-CBC** + **zlib** (C FFI, 匹配 QQ 音乐魔改算法) |
| 序列化 | **serde** / **serde_json** |
| CLI | **clap** |
| HTTP 客户端 | **reqwest** |

### 3.2 项目结构

```
QQMusicMonitor/
├── src/
│   ├── main.rs           # 入口 + 主循环 + TUI 渲染 + 时间插值 + 文件输出
│   ├── smtc.rs           # Windows SMTC API 封装（媒体信息读取）
│   ├── lyrics.rs         # QQ 音乐在线歌词 API 调用（三策略搜索 + 获取）
│   ├── qrc.rs            # QRC 解析器（DES 解密 + zlib 解压 + XML/文本解析）
│   ├── local_qrc.rs      # QQ 音乐本地缓存文件发现与读取
│   ├── server.rs         # axum HTTP + WebSocket 广播服务
│   ├── config.rs         # TOML 配置加载
│   ├── cli.rs            # 命令行参数定义
│   ├── song_info.rs      # 核心数据结构（SongInfo, QrcLine, QrcWord）
│   └── qq_des/           # C FFI：QQ 音乐魔改 DES 算法实现
│       ├── des.c
│       ├── des.h
│       └── QQMusicCommon.c
├── WebDemo/
│   ├── index.html        # Web 面板 HTML
│   ├── app.js            # WebSocket 客户端 + 歌词渲染
│   └── style.css         # 深色主题 CSS
├── config.toml           # 默认配置文件
├── Cargo.toml
└── build.rs              # 编译 C 依赖
```

### 3.3 数据流

```
┌──────────┐  SMTC API (100ms)  ┌────────────────────────────────┐
│ QQ Music │◄───────────────────│  smtc.rs                        │
│ Client   │                   │  读取 title/artist/album/position│
└──────────┘                   └───────────────┬────────────────┘
                                               │ SongInfo
                                               ▼
┌─────────────────────────────────────────────────────────────┐
│ main.rs (主循环)                                              │
│                                                             │
│  ① SMTC 轮询 → SongInfo                                     │
│  ② 切歌时后台 fetch: lyrics.rs (在线API)                     │
│     └─ qrc.rs (DES解密 + zlib解压 → XML → QrcLine)          │
│     └─ local_qrc.rs (本地文件兜底)                           │
│  ③ 时间插值 (smtc_offset_ms + poll_time_nanos)              │
│  ④ TUI 渲染 (逐字高亮 + 进度条 + 状态图标)                   │
│  ⑤ 文件输出 (TXT/JSON/lyric TXT)                            │
│  ⑥ watch channel 广播                                       │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│ server.rs                 WebDemo/app.js                     │
│ axum HTTP + WebSocket     浏览器渲染同样 KTV 效果            │
│ ws://127.0.0.1:3000/ws   CSS background-clip 渐变扫光        │
└─────────────────────────────────────────────────────────────┘
```

## 4. 启动方式

```bash
# 基本运行
cargo run --release

# 调试模式（打印详细 QRC 解析日志）
cargo run --release -- -d

# 自定义端口
cargo run --release -- --port 8080

# 仅用 Web 模式（不显示 TUI）
cargo run --release -- -q

# 完整参数
cargo run --release -- --help
```

打开浏览器访问 `http://127.0.0.1:3000` 即可查看 Web 面板（WebDemo 目录需自行托管或直接访问 axum 默认服务）。

## 5. 配置

```toml
[settings]
update_interval_ms = 100   # 轮询间隔（毫秒）
max_retries = 3            # API 重试次数
output_txt = false         # 输出 now_playing.txt
output_json = true         # 输出 now_playing.json
output_lyric = true        # 输出 current_lyric.txt
debug_mode = false         # 调试日志
enable_server = true       # 启用 Web 服务
server_port = 3000         # Web 服务端口
```

## 6. QRC 歌词解密流程

QQ 音乐的逐字歌词采用加密存储，解析流程：

```
QRC 原始数据（Base64 或 HEX）
        │
        ▼
① DES 解密（魔改算法，C FFI）
   - 十六进制格式：三重 DES（Ddes + des + Ddes）
   - Base64 格式：标准 DES ECB
        │
        ▼
② zlib 解压（raw deflate）
        │
        ▼
③ XML 解析（两种策略）
   - Strategy 1: <LyricLine><LyricWord> 节点
   - Strategy 2: LyricContent 属性文本
        │
        ▼
④ QrcLine / QrcWord 结构体
```

## 7. SMTC 时间模型

```
实际播放位置 ─── SMTC 报告滞后 ─── SMTC.Position() 快照
                                       │
                               poll_time_nanos（SMTC 返回时刻）
                                       │
                               interpolated = position + elapsed_since_poll
                                       │
                               display_time = interpolated + smtc_offset_ms(200)
```

- `smtc_offset_ms = 200`：补偿 SMTC 报告位置滞后于实际播放的固定偏移
- `poll_time_nanos`：记录 SMTC 异步调用返回时刻，避免把异步耗时算入插值
- 暂停时冻结 `display_time`，恢复后继续插值

## 8. Web Demo

Web 面板（`WebDemo/`）与 TUI 保持一致的渲染逻辑：

- 通过 WebSocket 接收实时 `SongInfo` JSON
- 歌词状态：`before`（灰色）→ `active`（逐字高亮）→ `after`（黄色已完成）
- 逐字扫光：`background-clip: text` + `linear-gradient` 宽度由 `--progress` CSS 变量控制
- SMTC 偏移 + 只进不退的插值策略，消除闪烁
- 切歌时歌词自动重建

## 9. 歌词获取策略

`lyrics.rs::LyricFetcher::fetch_lyrics()` 采用三级搜索：

1. **"artist title"** → SmartBox API → 取第一个结果
2. **"title"** → SmartBox API → 取第一个结果
3. **清洗后的 title**（去括号、去后缀） → SmartBox API

获取到 `songmid` 后，优先调用 **musicu.fcg**（现代 API，支持 QRC），失败则回退 **fcg_query_lyric_new.fcg**（旧 API）。返回内容包含原文、翻译和加密 QRC。

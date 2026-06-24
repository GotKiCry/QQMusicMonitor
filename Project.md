# QQMusic Monitor 项目文档

## 1. 项目概览

**QQMusic Monitor** 是一个基于 **Tauri 2** 的 Windows 桌面应用，通过 **SMTC（System Media Transport Controls）** API 实时读取 QQ 音乐客户端的播放状态，并支持：

- **Tauri GUI 前端** — 玻璃拟态桌面界面，唱机组件 + 自动居中滚动歌词 + 60fps 逐字 KTV 扫光
- **终端 TUI**（可选）— 实时显示歌曲信息、进度条、逐字高亮歌词
- **文件输出** — `now_playing.txt`（UTF-16 LE，适配 OBS）、`now_playing.json`、`current_lyric.txt`
- **WebSocket 同步** — HTTP + WebSocket 服务，供浏览器/OBS 等外部场景实时同步播放状态和歌词

## 2. 核心功能

- **毫秒级轮询** — 默认 100ms 间隔读取 SMTC 媒体状态
- **歌词三源获取**：
  1. QQ 音乐在线 API（`musicu.fcg`），优先获取逐字 QRC 格式
  2. QQ 音乐本地缓存文件（`_qm.qrc` / `_qm.lrc`）
  3. LRC 文本格式兜底
- **逐字 KTV 高亮** — 解析 QRC（QQ 音乐专有解密格式），在 TUI 和前端实现逐字扫光效果
- **SMTC 滞后补偿** — 基于 `LastUpdatedTime` 的漂移修正 + 用户可调偏移量，消除轮询间隔导致的闪烁
- **切歌瞬间修正** — 检测 SMTC media properties 与 timeline 异步更新间隙，强制归零滞后进度，前端同步清空旧歌词/重置插值基线，杜绝旧歌词残留
- **多音源过滤** — 只读取 QQ 音乐会话，排除浏览器/其他播放器干扰
- **后台降频** — 窗口隐藏时前端通知后端将轮询间隔降至 2s，降低 CPU 占用

## 3. 技术架构

### 3.1 技术栈

| 层级 | 技术 |
|---|---|
| 桌面框架 | **Tauri 2** (Rust 后端 + WebView 前端) |
| 语言 | **Rust** (2021 Edition) / **JavaScript** (原生 ES Modules) |
| 媒体接口 | **Windows SMTC** (`windows` crate `Media_Control` feature) |
| 异步运行时 | **tokio** |
| Web 服务 | **axum** (HTTP + WebSocket) |
| 终端渲染 | **crossterm** |
| 歌词解密 | **DES-CBC** + **zlib** (C FFI, 匹配 QQ 音乐魔改算法) |
| 序列化 | **serde** / **serde_json** |
| CLI | **clap** |
| HTTP 客户端 | **reqwest** |
| 前端样式 | 原生 CSS（设计 token + 模块化拆分，无预处理器） |

### 3.2 项目结构

```
QQMusicMonitor/
├── src-tauri/                  # Tauri 后端（Rust）
│   ├── src/
│   │   ├── main.rs             # 入口 + Tauri 窗口启动 + 主循环 + TUI 渲染 + 文件输出
│   │   ├── smtc.rs             # Windows SMTC API 封装（媒体信息读取 + 漂移修正）
│   │   ├── lyrics.rs           # QQ 音乐在线歌词 API（多策略搜索 + 专辑图获取）
│   │   ├── qrc.rs              # QRC 解析器（DES 解密 + zlib 解压 + XML/文本解析）
│   │   ├── local_qrc.rs        # QQ 音乐本地缓存文件发现与读取
│   │   ├── server.rs           # axum HTTP + WebSocket 广播服务
│   │   ├── config.rs           # TOML 配置加载
│   │   ├── cli.rs              # 命令行参数定义
│   │   ├── song_info.rs        # 核心数据结构（SongInfo, QrcLine, QrcWord）
│   │   └── qq_des/             # C FFI：QQ 音乐魔改 DES 算法实现
│   │       ├── des.c / des.h
│   │       └── QQMusicCommon.c
│   ├── build.rs                # 编译 C 依赖
│   ├── Cargo.toml
│   ├── tauri.conf.json         # Tauri 配置（frontendDist 指向 ../frontend）
│   ├── capabilities/           # Tauri 权限配置
│   └── icons/                  # 应用图标
├── frontend/                   # Tauri 前端（原生 ES Modules，无打包工具）
│   ├── index.html              # 应用入口 HTML
│   ├── css/                    # 模块化样式
│   │   ├── tokens.css          # 设计 token 与主题（极光青/罗兰紫/翡翠绿/落日橙）
│   │   ├── base.css            # 重置 + body + 极光背景 + 专辑模糊背景
│   │   ├── layout.css          # 玻璃拟态框架 + 头部 + 双列布局
│   │   ├── components.css      # 唱机 + 歌词行 + 逐字扫光 + 进度条
│   │   ├── settings.css        # 设置抽屉 + 表单控件 + 主题选择 + 调试面板
│   │   └── responsive.css      # 移动端响应式适配
│   └── src/
│       ├── index.js            # 入口：DOM 绑定 + 配置加载 + 启动数据源 + 渲染循环
│       ├── config/             # 前后端配置加载与持久化
│       │   ├── frontend-config.js  # 前端偏好（localStorage：字号/字重/主题/翻译/调试）
│       │   └── backend-config.js   # 后端配置（Tauri invoke 读写 config.toml）
│       ├── connection/         # 数据源桥接
│       │   ├── ws.js           # WebSocket 连接 + 指数退避重连
│       │   └── tauri-bridge.js # Tauri event 监听 + 后台状态通知
│       ├── state/              # 全局运行时状态
│       │   ├── state.js        # 状态字段 + setter + resetForSongChange()
│       │   └── update-handler.js  # SongInfo 应用到 DOM + 切歌重置逻辑
│       ├── lyrics/             # 歌词处理
│       │   ├── translation.js  # 翻译 LRC 解析 + 时间近似匹配
│       │   └── builder.js      # 歌词行 DOM 构建（word-bg/word-fg 双层剪裁）
│       ├── render/
│       │   └── render-loop.js  # 60fps 渲染循环（插值 + 逐字剪裁 + translateY 居中）
│       ├── settings/
│       │   └── settings-ui.js  # 设置抽屉交互（滑块/开关/步进/主题/端口）
│       └── utils/
│           ├── format.js       # 时间格式化 + LRC 时间解析
│           └── dom.js          # $ 查询 + 元素批量绑定 + setTextIfChanged
├── config.toml                 # 默认配置文件
├── Cargo.toml                  # workspace 声明
└── package.json                # Tauri CLI 依赖
```

### 3.3 数据流

```
┌──────────┐  SMTC API (100ms)  ┌────────────────────────────────┐
│ QQ Music │◄───────────────────│  smtc.rs                        │
│ Client   │                   │  读取 title/artist/album/position│
└──────────┘                   │  + LastUpdatedTime 漂移修正      │
                               └───────────────┬────────────────┘
                                                │ SongInfo
                                                ▼
┌─────────────────────────────────────────────────────────────┐
│ main.rs (主循环)                                              │
│                                                             │
│  ① SMTC 轮询 → SongInfo                                     │
│  ② 切歌检测 → 后台 fetch: lyrics.rs (在线API多策略搜索)      │
│     └─ qrc.rs (DES解密 + zlib解压 → XML → QrcLine)          │
│     └─ local_qrc.rs (本地文件兜底)                           │
│  ③ 切歌瞬间修正：timeline 滞后 → position 归零               │
│  ④ 时间插值 (smtc_offset_ms)                                │
│  ⑤ TUI 渲染 (可选：逐字高亮 + 进度条)                       │
│  ⑥ 文件输出 (TXT/JSON/lyric TXT)                            │
│  ⑦ 双通道广播：watch channel + Tauri emit("song-info")      │
└────────────────────────┬────────────────────────────────────┘
                         │
            ┌────────────┴────────────┐
            ▼                         ▼
┌─────────────────────────┐  ┌─────────────────────────────────┐
│ server.rs               │  │ Tauri AppHandle.emit             │
│ axum HTTP + WebSocket   │  │ → frontend/src/connection/       │
│ ws://127.0.0.1:3000/ws  │  │   tauri-bridge.js 监听            │
│ (浏览器/OBS 兜底通道)    │  │   (主通道，零回环开销)           │
└─────────────────────────┘  └─────────────┬───────────────────┘
                                           ▼
                            ┌─────────────────────────────────┐
                            │ frontend/src/state/              │
                            │   update-handler.js              │
                            │   切歌 → resetForSongChange()    │
                            │   清空旧歌词 + 重置插值基线       │
                            └─────────────┬───────────────────┘
                                          ▼
                            ┌─────────────────────────────────┐
                            │ frontend/src/render/             │
                            │   render-loop.js (60fps)         │
                            │   插值时间 → 逐字剪裁 → 居中滚动 │
                            └─────────────────────────────────┘
```

## 4. 启动方式

```bash
# 基本运行（启动 Tauri GUI 窗口 + 后端监控 + WebSocket 服务）
cargo tauri dev          # 开发模式（热重载）
cargo tauri build        # 构建可执行文件

# 直接运行已构建的二进制
cargo run --release
```

GUI 窗口启动后自动连接后端；如需浏览器/外部场景接入，访问 `http://127.0.0.1:3000` 或连 `ws://127.0.0.1:3000/ws`。

常用命令行参数（通过 Tauri 透传）：

```bash
cargo run --release -- --debug        # 调试模式（详细 QRC 解析日志）
cargo run --release -- --port 8080    # 自定义 WebSocket 服务端口
cargo run --release -- --interval 200 # 自定义 SMTC 轮询间隔
cargo run --release -- --help         # 完整参数列表
```

## 5. 配置

配置文件 `config.toml`，前端设置面板可实时修改并通过 Tauri invoke 落盘：

```toml
[settings]
update_interval_ms = 100   # 轮询间隔（毫秒）
smtc_offset_ms = 200       # SMTC 滞后补偿（毫秒）
max_retries = 3            # API 重试次数
output_txt = false         # 输出 now_playing.txt
output_json = true         # 输出 now_playing.json
output_lyric = true        # 输出 current_lyric.txt
debug_mode = false         # 调试日志
enable_server = true       # 启用 WebSocket 服务
server_port = 3000         # WebSocket 服务端口
```

前端偏好（字号、字重、主题、翻译开关、调试面板）存储在浏览器 `localStorage`，独立于后端配置。

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
                               LastUpdatedTime（快照采样时刻）
                                       │
                               elapsed = now - LastUpdatedTime（clamp 5s）
                                       │
                               corrected = Position + elapsed
                                       │
                               display = corrected + smtc_offset_ms(200)
```

- `LastUpdatedTime` 修正：匹配 Windows 音量浮窗进度条的同款算法，消除 SMTC 快照滞后
- `smtc_offset_ms = 200`：用户可调的固定前置偏移，用于精细对齐
- `elapsed` clamp 到 5s，防止 `LastUpdatedTime` 异常时产生大跳变
- 暂停时冻结进度，恢复后继续插值

## 8. 前端（Tauri Webview）

前端（`frontend/`，原生 ES Modules，无打包工具）与 TUI 保持一致的渲染逻辑：

### 8.1 数据通道

- **Tauri 环境（主通道）**：`listen('song-info')` 事件接收实时 `SongInfo`，零回环开销
- **浏览器/独立调试（兜底）**：WebSocket `ws://127.0.0.1:3000/ws` + 指数退避重连

### 8.2 渲染

- 歌词状态：`before`（灰色）→ `active`（逐字高亮）→ `after`（已完成）
- 逐字扫光：`background-clip: text` + `linear-gradient`，通过 `.word-fg` 宽度 0~100% 控制
- 居中滚动：CSS `transform: translateY` + transition，活跃行变化时平滑居中（不改动 scrollTop）
- SMTC 偏移 + 只进不退的插值策略，消除闪烁

### 8.3 切歌处理（关键）

切歌瞬间 SMTC 的 media properties（title/artist）与 timeline（Position/EndTime）异步更新，会出现"新标题 + 旧进度"的错配快照。前端与后端协同处理：

- **后端**（`main.rs`）：切歌且 `current_time_ms > 5000` 时强制归零，防止旧进度继续广播
- **前端**（`update-handler.js`）：
  1. `showLoadingPlaceholder()` 立即清空旧歌词 DOM，显示"正在加载歌词..."
  2. `setLyricLines([])` 清空歌词数据，渲染循环直接 return
  3. `resetForSongChange()` 重置插值基线（`lastSampleProgress`/`lastSampleLocalTime`/`lastRawTimeMs`）
  4. 仅当后端推送非空 `qrc_data` 时才重建真歌词覆盖占位，避免用"无逐字歌词"覆盖加载提示

### 8.4 后台降频

`visibilitychange` 监听窗口隐藏，通过 `invoke('set_background_state')` 通知后端将轮询间隔从 100ms 降至 2s，降低 CPU 占用；窗口恢复时自动回滚。

## 9. 歌词获取策略

`lyrics.rs::LyricFetcher::fetch_lyrics()` 采用多级搜索，按命中率从高到低尝试：

1. **"artist title"** → SmartBox API → 取第一个结果
2. **"title"** → SmartBox API（artist 可能含多歌手分隔符导致失败）
3. **"cleaned_artist cleaned_title"** → 清洗括号/后缀后搜索
4. **"cleaned_title" 单独** → 修复非 ASCII 标题（如韩文 `삐딱하게`）匹配正确版本，避免英文别称命中错误专辑
5. **括号内别称** → 从标题括号中提取替代名搜索

获取到 `songmid` 后，优先调用 **musicu.fcg**（现代 API，支持 QRC），失败则回退 **fcg_query_lyric_new.fcg**（旧 API）。专辑封面通过 `get_song_detail_yqq` 获取 `albummid` 拼接高清图 URL。

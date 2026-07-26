# QQMusic Monitor 项目文档

## 1. 项目概览

**QQMusic Monitor** 是一个基于 **Tauri 2** 的 Windows 桌面应用，通过 **SMTC（System Media Transport Controls）** API 实时读取 QQ 音乐客户端的播放状态，并支持：

- **Tauri GUI 前端** — 玻璃拟态桌面界面，唱机组件 + 自动居中滚动歌词 + 60fps 逐字 KTV 扫光
- **文件输出** — `now_playing.txt`（UTF-16 LE，适配 OBS）、`now_playing.json`、`current_lyric.txt`
- **WebSocket 同步** — HTTP + WebSocket 服务，供浏览器/OBS 等外部场景实时同步播放状态和歌词

## 2. 核心功能

- **毫秒级轮询** — 默认 100ms 间隔读取 SMTC 媒体状态
- **歌词三源获取**：
  1. QQ 音乐在线 API（`musicu.fcg`），优先获取逐字 QRC 格式
  2. QQ 音乐本地缓存文件（`_qm.qrc` / `_qm.lrc`）
  3. LRC 文本格式兜底
- **逐字 KTV 高亮** — 解析 QRC（QQ 音乐专有解密格式），在前端实现逐字扫光效果
- **暂停/恢复零跳变** — 暂停淡出状态机建模 QQ 音乐 ~1s 淡入淡出（报进/冻结两阶段 + 提交/旧锚点过滤），WASAPI 音频会话提供「是否真实出声」地面真值，逐字歌词在暂停/恢复全程无瞬移、无回退
- **SMTC 滞后补偿** — 基于 `LastUpdatedTime` 的漂移修正 + 用户可调有符号偏移，消除轮询间隔导致的闪烁
- **SMTC 事件驱动** — 播放状态/timeline/会话变化事件实时唤醒主循环，状态切换延迟从轮询周期压到 ~10ms 级
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
| 音频真值 | **WASAPI 音频会话** (`IAudioSessionControl` / `IAudioMeterInformation`) |
| 异步运行时 | **tokio** |
| Web 服务 | **axum** (HTTP + WebSocket) |
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
│   │   ├── main.rs             # 入口 + Tauri 窗口启动 + 主循环 + 文件输出
│   │   ├── smtc.rs             # Windows SMTC API 封装（漂移修正 + 暂停淡出/恢复 hold 状态机）
│   │   ├── audio_session.rs    # WASAPI 音频会话监控（「是否真实出声」地面真值）
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
┌──────────┐  SMTC API (100ms + 事件唤醒)  ┌───────────────────────┐
│ QQ Music │◄──────────────────────────────│  smtc.rs               │
│ Client   │                               │  读取媒体信息 + 漂移修正 │
└──────────┘                               │  暂停淡出 / 恢复 hold   │
                                           └───────────┬───────────┘
                                                        │ SongInfo
                                                        ▼
┌─────────────────────────────────────────────────────────────┐
│ main.rs (主循环)                                              │
│                                                             │
│  ① SMTC 轮询/事件 → SongInfo（含淡出/恢复建模）              │
│  ② 音频门控：WASAPI 出声真值修正播放状态（淡出窗口内让路）     │
│  ③ 切歌检测 → 后台 fetch: lyrics.rs (在线API多策略搜索)      │
│     └─ qrc.rs (DES解密 + zlib解压 → XML → QrcLine)          │
│     └─ local_qrc.rs (本地文件兜底)                           │
│  ④ 切歌瞬间修正：timeline 滞后 → position 归零               │
│  ⑤ 有符号偏移补偿 (smtc_offset_ms)                          │
│  ⑥ 文件输出（JSON 限频、当前歌词按变化写入）                 │
│  ⑦ 双通道广播：watch channel + Tauri event                  │
└────────────────────────┬────────────────────────────────────┘
                         │
            ┌────────────┴────────────┐
            ▼                         ▼
┌─────────────────────────┐  ┌─────────────────────────────────┐
│ server.rs               │  │ Tauri AppHandle.emit             │
│ axum HTTP + WebSocket   │  │ → frontend/src/connection/       │
│ ws://127.0.0.1:3000/ws  │  │   song-info: 静态完整快照         │
│ (浏览器/OBS 兜底通道)    │  │   playback-progress: 轻量进度    │
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
smtc_offset_ms = 200       # SMTC 滞后补偿（毫秒，可为负数）
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

### 7.1 播放中

```
实际播放位置 ─── SMTC 报告滞后 ─── SMTC.Position() 快照
                                       │
                               LastUpdatedTime（快照采样时刻）
                                       │
                               elapsed = now - LastUpdatedTime（clamp 1s）
                                       │
                               corrected = Position + elapsed
                                       │
                               display = signed_offset(corrected, smtc_offset_ms)
                                       + 本机事件传输耗时
```

- `LastUpdatedTime` 修正：匹配 Windows 音量浮窗进度条的同款算法，消除 SMTC 快照滞后；实测 QQ 音乐播放中以 ~4Hz 刷新 timeline（位置粒度 10ms），修正后任意时刻误差 <100ms
- `smtc_offset_ms = 200`：后端唯一应用的有符号偏移；负值可让歌词稍晚显示
- 播放中 timeline 快照间隙：航位推算（上一帧发出值 + 真实流逝），不因快照迟到而冻结
- 前端依据 `server_ts` 补偿最多 250ms 的本机传输耗时，再用 `performance.now()` 在采样间插值

### 7.2 暂停/恢复（逐字精度关键）

实测发现「SMTC 报暂停 ≠ 音乐停止」：QQ 音乐默认淡出 ~1s，期间声音与真实位置仍在前进，但 SMTC Position 冻结；淡出结束后 QQ 推送「最终位置」提交（比暂停时刻 ~+1.2s），恢复播放也从该位置继续。若按 SMTC 字面处理，歌词会先停住、提交到达时被迫前跳。为此 `smtc.rs` 实现两个状态机：

**暂停淡出状态机**（两阶段）：

1. **报进阶段**：以暂停前最后一帧播放位置为锚按 1× 继续报进（与可听见的淡出同步）。QQ 可能在淡出进行中就提前写入最终位置（实测最快 +0.28s）——只接受「临近尾声的提交」（`is_tail_commit`），提前到达的一律忽略；WASAPI 连续静音采样（2 次）或超时（1.5s）判定淡出结束，冻结在冻结时刻的推算值（与前端插值显示连续，无回退）
2. **冻结阶段**：结构保持存活并过滤后续 paused 刷新——提交微调（±100ms）接受；QQ 先刷的落后旧锚点、淡出尾巴的迟到提交（向前 ≤1.5s）在宽限期（4s）内忽略（后者延迟到恢复播放时随锚点自然对齐）；其余移动视为真实 seek 放行

**恢复 hold**：暂停→播放后，在声音真正渲染出来（蓝牙重新建链/设备慢启动）或 QQ 位置真正移动之前，把对外位置冻结在原显示位置（`resume_freeze_floor`），解除后从冻结点平滑继续——与 QQ 恢复点的对齐跳变被推迟到出声瞬间，被音乐起步掩盖

**WASAPI 音频门控**（`audio_session.rs`）：SMTC 只是播放器「声称」的状态，音频会话的峰值电平/会话状态来自音频引擎本身：

- 播放中持续静音 >2s → 判定停滞（缓冲），提前按暂停显示
- 会话停止流 >150ms 但 SMTC 未报 → 提前按暂停显示
- 淡出窗口内门控让路（`pause_fade_active`），避免与淡出报进打架
- 找不到 QQ 音频会话时整体回退为纯 SMTC 行为

## 8. 前端（Tauri Webview）

前端使用原生 ES Modules，无需额外打包工具：

### 8.1 数据通道

- **Tauri 环境（主通道）**：`song-info` 仅在歌曲/歌词/封面变化时发送完整快照，`playback-progress` 按轮询频率发送轻量进度；监听建立后主动调用 `get_current_song_info` 防止遗漏首次快照
- **浏览器/独立调试（兜底）**：WebSocket `ws://127.0.0.1:3000/ws` + 指数退避重连

### 8.2 渲染

- 歌词状态：`before`（灰色）→ `active`（逐字高亮）→ `after`（已完成）
- 逐字扫光：`background-clip: text` + `linear-gradient`，通过 `.word-fg` 宽度 0~100% 控制
- 居中滚动：CSS `transform: translateY` + transition，活跃行变化时平滑居中（不改动 scrollTop）
- 后端单次偏移 + 前端只进不退的插值策略，消除重复偏移和闪烁

### 8.3 切歌处理（关键）

切歌瞬间 SMTC 的 media properties（title/artist）与 timeline（Position/EndTime）异步更新，会出现"新标题 + 旧进度"的错配快照。前端与后端协同处理：

- **后端**（`main.rs`）：切歌且 `current_time_ms > 5000` 时强制归零，防止旧进度继续广播
- **前端**（`update-handler.js`）：
  1. `showLoadingPlaceholder()` 立即清空旧歌词 DOM，显示"正在加载歌词..."
  2. `setLyricLines([])` 清空歌词数据，渲染循环直接 return
  3. `resetForSongChange()` 重置插值基线（`lastSampleProgress`/`lastSampleLocalTime`/`lastRawTimeMs`）
  4. 仅当后端推送非空 `qrc_data` 时才重建真歌词覆盖占位，避免用"无逐字歌词"覆盖加载提示
  5. 启动首帧（`songInfo` 为空）不套用 >5s 归零：后端启动时不存在"新歌名 + 旧 timeline"错配窗口，首帧暂停进度是权威值，直接信任，避免进度先闪 0 再跳回

### 8.4 暂停/恢复处理（前端）

- **play→pause**：直接 snap 到服务端权威位置——后端报暂停时携带的是淡出建模后的真实停止点；本地插值因事件/传输延迟必然略有偏差，烘焙本地值只会固化超调
- **暂停稳态**：始终跟随服务端位置（恒定且权威），同时覆盖暂停中任意幅度 seek
- **尾音蠕升抑制**：以未加传输补偿的 `current_time_ms` 检测「is_playing=true 但位置不变」的尾音帧，停止本地插值蠕升，把冻结时刻的回退量压到 ≈0
- **pause→play**：服务端略落后（≤500ms）时本地时间基准向未来平移，原地 hold 等待追平，不回跳也不累积超前
- **播放中后退 seek**：向后移动 >250ms 立即跟随（锚点抖动 ±50ms 触不到该阈值），避免死区拖慢 ~0.5s

### 8.5 后台降频

`visibilitychange` 监听窗口隐藏，通过 `invoke('set_background_state')` 通知后端将轮询间隔从 100ms 降至 2s，降低 CPU 占用；窗口恢复时自动回滚。

## 9. 歌词获取策略

`lyrics.rs::LyricFetcher::fetch_lyrics()` 采用多级搜索，按命中率从高到低尝试：

1. **"artist title"** → SmartBox API → 取第一个结果
2. **"title"** → SmartBox API（artist 可能含多歌手分隔符导致失败）
3. **"cleaned_artist cleaned_title"** → 清洗括号/后缀后搜索
4. **"cleaned_title" 单独** → 修复非 ASCII 标题（如韩文 `삐딱하게`）匹配正确版本，避免英文别称命中错误专辑
5. **括号内别称** → 从标题括号中提取替代名搜索

获取到 `songmid` 后，优先调用 **musicu.fcg**（现代 API，支持 QRC），失败则回退 **fcg_query_lyric_new.fcg**（旧 API）。专辑封面通过 `get_song_detail_yqq` 获取 `albummid` 拼接高清图 URL。

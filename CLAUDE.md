# QQMusic Monitor 项目记忆

## 记忆 · 2026-06-26 · 本地歌词/封面优先策略修复

- 决定：始终保持「本地歌词优先」已有架构（`has_local_qrc` 时跳过在线 `fetch_lyrics`，只补封面）；本次把封面解析也纳入本地优先。
- 修复 `local_qrc::normalize`：改为「非字母数字全折叠为单空格 + 折叠空白」，解决 SMTC `Go Again (feat. ELYSA)` 与缓存文件名 `Go Again (feat_ ELYSA)` 因 `.`/`_` 与括号导致 title 比较双双失败、本地 QRC 被跳过的问题。CJK 不受影响（`is_alphanumeric` 对中日韩为 true）。
- 新增 `local_qrc::parse_lyric_filename`：从 `Artist - Title - Duration - Album_qm.{qrc,lrc}` 解析各段；`lookup_local_lyrics` 据此把本地文件名里的专辑名填入缓存条目的新字段 `local_album`（比依赖 SMTC album 更可靠）。
- 新增 `LyricFetcher::search_song_mid`：多策略 songmid 搜索（与 `fetch_lyrics` 同策略序，但不做歌词非空校验），供「本地歌词已命中、仅缺封面」场景解析 album_mid。旧的 `has_local_qrc` 封面分支只搜了原始 `"{artist} {title}"`/`title`，遇到 `(Explicit)` 后缀常 0 命中 → 「封面: ✗ 未找到」。
- 重写 `has_local_qrc` 封面分支：1a 歌曲多策略搜索拿 album_mid → 1b 仍失败则用 `local_album` + SMTC album（各自清洗去括号）走 `search_album_mid_by_name` → 2 本地 `QQMusicPicture` 命中转 **base64 data URI**（与在线路径一致，弃用原 `file://`，避免 webview 受限）→ 否则在线 CDN URL。
- 注意：`LyricsCache` 原有 `get_entry` 是 `&mut self`（会更新 LRU 时间戳），在 read guard 下取字段会编译报错；已加只读版 `peek_entry`。
- 在线 `fetch_lyrics` 路径未改动，避免影响回归测试 `test_fetch_crooked_album_match`（依赖策略 3b 在括号别称前命中韩文原版专辑）。
- 测试：新增 `local_qrc::tests::test_normalize_punctuation_insensitive` 和 `test_parse_lyric_filename`；现有 12 测试全过。

## 环境约定

- 后端构建：`cargo build` / 类型检查：`cargo check`，均在 `src-tauri/`（用 `--manifest-path src-tauri/Cargo.toml`）。链接写 `target/debug/qqmusic-monitor.exe` 时若 GUI 在运行会被占用（os error 5），属正常。
- 本地缓存：`D:\QQMusicCache\{QQMusicLyricNew (~2600 文件), QQMusicPicture (~264 张, 按 `T002R{W}x{H}M000{album_mid}.jpg` 命名)}`。
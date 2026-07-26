use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
use windows::Media::Control::GlobalSystemMediaTransportControlsSession;
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus;
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use crate::song_info::SongInfo;
use anyhow::{Result, Context};
use std::sync::{Arc, OnceLock};
use tokio::sync::{Mutex, Notify};

/// SMTC 状态变化通知：暂停/恢复/seek/切歌时 Windows 主动推送，
/// 主循环借此摆脱纯轮询延迟（事件到达即唤醒，~10ms 级响应）
static SMTC_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();

pub fn state_changed_notify() -> Arc<Notify> {
    SMTC_NOTIFY.get_or_init(|| Arc::new(Notify::new())).clone()
}

fn fire_state_changed() {
    if let Some(n) = SMTC_NOTIFY.get() {
        n.notify_one();
    }
}

/// 会话事件注册守卫：会话切换时随 struct drop 自动注销
struct SessionEventGuard {
    session: GlobalSystemMediaTransportControlsSession,
    playback: i64,
    timeline: i64,
    media: i64,
}

impl Drop for SessionEventGuard {
    fn drop(&mut self) {
        let _ = self.session.RemovePlaybackInfoChanged(self.playback);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.timeline);
        let _ = self.session.RemoveMediaPropertiesChanged(self.media);
    }
}

fn register_session_events(session: &GlobalSystemMediaTransportControlsSession) -> Option<SessionEventGuard> {
    let playback = session
        .PlaybackInfoChanged(&TypedEventHandler::new(|_, _| {
            fire_state_changed();
            Ok(())
        }))
        .ok()?;
    let timeline = session
        .TimelinePropertiesChanged(&TypedEventHandler::new(|_, _| {
            fire_state_changed();
            Ok(())
        }))
        .ok()?;
    let media = session
        .MediaPropertiesChanged(&TypedEventHandler::new(|_, _| {
            fire_state_changed();
            Ok(())
        }))
        .ok()?;
    Some(SessionEventGuard {
        session: session.clone(),
        playback,
        timeline,
        media,
    })
}

/// 本帧位置的来源（恢复 hold 需要区分「锚定值」与「推算值」）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CorrectionKind {
    /// 未校正（暂停 / 时间戳无效 / 快照过期的兜底）
    #[default]
    Raw,
    /// 暂停→恢复且 timeline 未刷新：报的原始恢复点
    ResumeStale,
    /// 播放中 timeline 未更新：航位推算值
    DeadReckon,
    /// 新 timeline 快照锚定值（QQ 权威位置）
    Anchored,
}

/// 上一帧的 timeline 锚点状态，用于保持位置连续性
#[derive(Clone, Copy, Debug)]
struct TimelineState {
    is_playing: bool,
    ts_100ns: i64,
    /// 上一帧对外发出的校正后位置
    emitted_ms: u64,
    /// 上一帧发出时刻（Windows 纪元毫秒）
    emitted_at_ms: i64,
    kind: CorrectionKind,
    /// 本帧是否为「暂停→播放」的切换帧
    from_pause: bool,
}

/// 恢复 hold：暂停→播放后，在声音真正出来或 QQ 位置真正移动之前，
/// 把对外位置冻结在恢复点（此时 QQ 自己的 Position 也冻着，两边天然对齐，
/// 解除后无需回滚）。否则航位推算会随真实时间空跑，QQ 推送真实位置时
/// 前端被迫回跳（表现为"歌词前进几秒又回滚"）。
#[derive(Clone, Debug)]
struct ResumeHold {
    position_ms: u64,
    song_key: String,
    since: std::time::Instant,
}

/// 恢复 hold 解除条件：QQ 位置移动超过恢复点该值视为「真的开始播了」
const HOLD_POSITION_MOVED_MS: u64 = 300;
/// hold 最长持续时长（兜底，防止异常情况下永久冻结）
const RESUME_HOLD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 暂停淡出最长报进时长（QQ 默认淡出 ~1.2s，留余量）
const FADE_MAX_MS: u64 = 1_500;
/// 淡出窗口内连续静音采样达到该次数即判定淡出结束（尾音防抖；
/// 淡出包络单调衰减，低于阈值即接近终点，2 次足够）
const FADE_SILENT_SAMPLES_END: u32 = 2;
/// 冻结后位置微调接受阈值：提交值与冻结值通常差 ±50ms 内
const FROZEN_FINE_TOLERANCE_MS: i64 = 100;
/// 冻结后宽限期内的向前移动不超过该值视为「淡出尾巴的迟到提交」——
/// 忽略（冻结值已是听觉终点；提交值在恢复播放时自然对齐），
/// 超过则视为真实向前 seek 接受
const FROZEN_FWD_COMMIT_MAX_MS: i64 = 1_500;
/// 暂停超过该时长后，任何方向的移动都判定为真实 seek 而接受
const PAUSE_STALE_GRACE_MS: i64 = 4_000;

/// 暂停淡出跟踪：SMTC 报暂停 ≠ 音乐停止。QQ 默认淡出 ~1s，期间
/// 声音与真实位置仍在前进，但 SMTC Position 冻结；淡出结束后 QQ
/// 推送一次「最终位置」提交（比暂停时刻 ~+1.2s），恢复播放也从该位置继续。
/// 冻结后该结构在整个暂停期间保持存活，用于过滤 QQ 暂停后的
/// 旧锚点刷新与迟到提交。
#[derive(Clone, Debug)]
struct PauseFade {
    /// 淡出起点位置（暂停前最后一帧对外的播放位置）
    anchor_ms: u64,
    /// 起点对应的 Windows 纪元毫秒
    anchor_at_ms: i64,
    /// 最近一次处理的 timeline 时间戳（刷新检测：暂停中 ts 变化 = 提交/seek/旧锚点）
    ts_100ns: i64,
    song_key: String,
    /// 连续静音采样数
    silent_samples: u32,
    /// 暂停开始时刻（Windows 纪元毫秒，stale 宽限计时）
    started_at_ms: i64,
    /// 已冻结：Some(冻结位置)；None = 仍在淡出报进阶段
    frozen_ms: Option<u64>,
}

/// 淡出报进阶段单帧裁决（纯函数，便于测试）：
/// 只决定何时冻结；timeline 刷新的提交判定见 is_tail_commit。
/// 注意位置全程按 1× 报进（尾音低于 -60dB 但位置仍在前进），
/// 冻结值 = 冻结时刻的推算值——与前端插值显示天然连续，无回退。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FadeAction {
    /// 继续按 1× 报进（声音确实还在）
    Advance,
    /// 尾音结束 / 超时：冻结在当前推算位置
    FreezeTail,
    /// 临近尾声的提交到达：采用提交值冻结
    FreezeRaw,
}

fn fade_step(elapsed_ms: u64, audible: Option<bool>, pending_silent: u32) -> FadeAction {
    if elapsed_ms >= FADE_MAX_MS {
        return FadeAction::FreezeTail;
    }
    if audible == Some(false) && pending_silent + 1 >= FADE_SILENT_SAMPLES_END {
        return FadeAction::FreezeTail;
    }
    FadeAction::Advance
}

/// 报进阶段的 timeline 刷新是否为「临近尾声的提交」（纯函数）。
/// 提交值 = 淡出结束点（≈ anchor + 淡出时长）。QQ 可能在淡出进行中就
/// 提前写入该值（实测最快 +0.28s）——此时音乐仍在淡出，跳过去会瞬移
/// ~1s，必须忽略；落后于锚点的刷新是旧锚点，同样忽略。
fn is_tail_commit(raw_ms: u64, anchor_ms: u64, elapsed_ms: u64) -> bool {
    raw_ms > anchor_ms && raw_ms <= anchor_ms.saturating_add(elapsed_ms).saturating_add(250)
}

/// 冻结阶段收到 paused timeline 刷新时，是否接受 raw 为新冻结值（纯函数）。
/// 提交微调（±100ms）接受；大幅向前但未超 1500 且在宽限期内 = 淡出尾巴
/// 的迟到提交，忽略（恢复播放时自然对齐）；宽限期内的向后移动 = QQ 先刷
/// 的落后旧锚点，忽略；其余视为真实 seek 接受。
fn accept_frozen_refresh(raw_ms: u64, frozen_ms: u64, since_pause_ms: i64) -> bool {
    let diff = raw_ms as i64 - frozen_ms as i64;
    if diff.abs() <= FROZEN_FINE_TOLERANCE_MS {
        return true;
    }
    if diff > 0 {
        if since_pause_ms <= PAUSE_STALE_GRACE_MS {
            diff > FROZEN_FWD_COMMIT_MAX_MS
        } else {
            true
        }
    } else {
        since_pause_ms > PAUSE_STALE_GRACE_MS
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldAction {
    /// 当前不在 hold，也无需进入
    None,
    /// 进入 hold（切换帧 + 未出声 + 有会话可采样）
    Enter,
    /// 保持 hold（继续冻结）
    Hold,
    /// 解除 hold（出声 / QQ 位置移动 / 超时 / 又暂停 / 切歌）
    Release,
}

/// 恢复 hold 的纯决策函数（便于测试）
fn decide_resume_hold(
    holding: bool,
    from_pause: bool,
    is_playing: bool,
    song_unchanged: bool,
    anchored_position_moved: bool,
    sample: Option<crate::audio_session::AudioSample>,
    hold_elapsed: std::time::Duration,
) -> HoldAction {
    use crate::audio_session::PEAK_SILENCE_THRESHOLD;
    if holding {
        if !is_playing || !song_unchanged {
            return HoldAction::Release;
        }
        if anchored_position_moved {
            return HoldAction::Release;
        }
        if sample.map(|s| s.peak > PEAK_SILENCE_THRESHOLD).unwrap_or(false) {
            return HoldAction::Release;
        }
        if hold_elapsed > RESUME_HOLD_TIMEOUT {
            return HoldAction::Release;
        }
        return HoldAction::Hold;
    }
    // 进入条件：暂停→播放切换帧、能采样到 QQ 会话且当前无声
    if is_playing && from_pause {
        if let Some(s) = sample {
            if s.peak <= PEAK_SILENCE_THRESHOLD {
                return HoldAction::Enter;
            }
        }
    }
    HoldAction::None
}

struct SmtcReaderInner {
    manager: GlobalSystemMediaTransportControlsSessionManager,
    current_session: Option<GlobalSystemMediaTransportControlsSession>,
    last_song_key: Option<String>,
    last_thumbnail_base64: String,
    /// 上一帧的 timeline 锚点，用于识别快照更新 / 恢复播放 / 快照间隙
    prev_timeline_state: Option<TimelineState>,
    /// 当前会话的事件注册（随会话切换自动注销）
    event_guard: Option<SessionEventGuard>,
    /// 恢复 hold：暂停→播放后冻结位置直到声音出来 / QQ 位置移动
    resume_hold: Option<ResumeHold>,
    /// 暂停淡出跟踪（SMTC 报暂停但音乐仍在淡出的窗口）
    pause_fade: Option<PauseFade>,
    /// 冻结后恢复时，恢复 hold 应先冻结在这个原显示位置：
    /// QQ 从提交点继续（领先冻结值几十~几百 ms），若直接冻结在 QQ
    /// 恢复点，跳变发生在尚未出声的静默期；先冻结在原位置，把对齐
    /// 跳变推迟到出声/锚定时刻（被音乐起步掩盖）
    resume_freeze_floor: Option<u64>,
    /// 上一帧对外的播放状态（识别 播放→暂停 切换帧）
    last_reported_playing: bool,
    /// 最后一帧播放状态的对外位置与发出时刻（淡出起点锚）
    last_playing_emitted: Option<(u64, i64)>,
}

static SMTC_READER: OnceLock<Mutex<SmtcReaderInner>> = OnceLock::new();

// WASAPI 会话接口（IAudioSessionControl 等）不是 Send，不能放进上面的静态。
// 音频采样只发生在监控线程（run_monitor 所在线程），用 thread_local 持有。
thread_local! {
    static AUDIO_GATE: std::cell::RefCell<crate::audio_session::AudioGate> =
        std::cell::RefCell::new({
            crate::audio_session::AudioGate::ensure_com_init();
            crate::audio_session::AudioGate::new()
        });
}

fn sample_audio_gate() -> Option<crate::audio_session::AudioSample> {
    AUDIO_GATE.with(|g| g.borrow_mut().sample())
}

/// 暂停淡出是否进行中（报进或已冻结阶段）。淡出期间 is_playing 由
/// 淡出状态机全权负责，main 的音频门控必须让路——否则门控的
/// inactive 判定会在淡出收尾前把状态翻成暂停，与仍在前进的淡出
/// 报进打架（前端看到「已暂停但位置还在走」的过头/回退）
pub async fn pause_fade_active() -> bool {
    match SMTC_READER.get() {
        Some(m) => m.lock().await.pause_fade.is_some(),
        None => false,
    }
}

/// Windows FILETIME 纪元（1601）与 Unix 纪元（1970）的毫秒差
const WINDOWS_EPOCH_OFFSET_MS: i64 = 11_644_473_600_000;

/// 校正 SMTC 快照位置为当前真实播放位置（纯函数，便于测试）。
///
/// SMTC.Position 是 SMTC.LastUpdatedTime 时刻的快照。设计目标：播放中
/// 位置单调、连续、不回退、不冻结——回退和跳变都会直接表现为歌词闪烁。
///
/// 三种情况分别处理：
/// 1. 暂停→恢复但 timeline 未刷新（时间戳与暂停时相同）：快照仍是暂停
///    时刻的，elapsed 包含整个暂停时长，校正 = 抢跑。直接用原始位置，
///    覆盖任意时长的暂停（包括 <1.5s 的短暂停）。
/// 2. 播放中 timeline 未更新（快照间隙）：航位推算——音乐以 1× 速前进，
///    位置 = 上一帧发出值 + 真实流逝时间。不因快照迟到而冻结，
///    也不会在间隙结束后回跳（前提是 QQ 快照本身准确）。
/// 3. 新快照（正常 ~2Hz 更新 / seek / 恢复后已刷新）：以快照为锚，
///    elapsed = now - LastUpdatedTime（clamp 1000ms）。
///    快照异常陈旧（elapsed > 1500ms，正常播放应 <600ms）时放弃校正。
fn correct_position(
    raw_ms: u64,
    total_ms: u64,
    is_playing: bool,
    last_updated_100ns: i64,
    now_windows_ms: i64,
    prev: Option<TimelineState>,
) -> (u64, TimelineState) {
    let cap = |v: u64| if total_ms > 0 { v.min(total_ms) } else { v };

    let mut from_pause = false;
    let (corrected, kind) = if is_playing && last_updated_100ns > 0 {
        match prev {
            // 情况 1：恢复播放但 timeline 未刷新
            Some(p) if !p.is_playing && p.ts_100ns == last_updated_100ns => {
                from_pause = true;
                (raw_ms, CorrectionKind::ResumeStale)
            }
            // 情况 2：播放中 timeline 未更新 → 航位推算
            Some(p) if p.is_playing && p.ts_100ns == last_updated_100ns => {
                // clamp 5s 防系统时钟调整/休眠恢复导致的 dt 爆炸
                let dt = (now_windows_ms - p.emitted_at_ms).clamp(0, 5_000) as u64;
                (cap(p.emitted_ms + dt), CorrectionKind::DeadReckon)
            }
            // 情况 3：新快照 / 状态切换 / 首帧 → 以快照为锚
            _ => {
                from_pause = prev.map(|p| !p.is_playing).unwrap_or(false);
                let last_updated_ms = last_updated_100ns / 10_000;
                let elapsed_ms = (now_windows_ms - last_updated_ms).max(0) as u64;
                // 快照异常陈旧（正常播放 elapsed 应 <600ms）→ 放弃校正
                if elapsed_ms > 1500 {
                    (raw_ms, CorrectionKind::Raw)
                } else {
                    (cap(raw_ms + elapsed_ms.min(1000)), CorrectionKind::Anchored)
                }
            }
        }
    } else {
        (raw_ms, CorrectionKind::Raw)
    };

    let state = TimelineState {
        is_playing,
        ts_100ns: last_updated_100ns,
        emitted_ms: corrected,
        emitted_at_ms: now_windows_ms,
        kind,
        from_pause,
    };
    (corrected, state)
}

// Function to check if the session is from QQ Music.
fn is_qqmusic_session(session: &GlobalSystemMediaTransportControlsSession) -> bool {
    if let Ok(id) = session.SourceAppUserModelId() {
        let app_id = id.to_string().to_lowercase();
        app_id.contains("qqmusic")
            || app_id.contains("xuanwo")
            || app_id == "qqmusic"
    } else {
        false
    }
}

// Helper to read thumbnail from IRandomAccessStreamReference and convert to base64 data URL
async fn read_thumbnail_base64(thumbnail_ref: IRandomAccessStreamReference) -> Result<String> {
    let stream = thumbnail_ref.OpenReadAsync()?.await?;
    let content_type = stream.ContentType()?.to_string();
    let size = stream.Size()? as u32;
    if size == 0 {
        return Ok(String::new());
    }
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.await?;
    let mut buffer = vec![0u8; size as usize];
    reader.ReadBytes(&mut buffer)?;
    let base64_str = STANDARD.encode(&buffer);
    Ok(format!("data:{};base64,{}", content_type, base64_str))
}

// Function to read media information from a session.
// Uses LastUpdatedTime to compute drift-corrected playback position,
// matching the same algorithm Windows uses for its volume flyout progress bar.
async fn read_session_info(session: &GlobalSystemMediaTransportControlsSession, fetch_thumbnail: bool, prev_timeline_state: &mut Option<TimelineState>) -> Result<SongInfo> {
    let media_properties = session.TryGetMediaPropertiesAsync()?.await.context("Failed to get media properties")?;
    let timeline_properties = session.GetTimelineProperties()?;

    let title = media_properties.Title()?.to_string();
    let artist = media_properties.Artist()?.to_string();
    let album = media_properties.AlbumTitle()?.to_string();

    let album_pic_url = String::new();
    if fetch_thumbnail {
        if let Ok(thumbnail_ref) = media_properties.Thumbnail() {
            tokio::task::spawn_local(async move {
                match read_thumbnail_base64(thumbnail_ref).await {
                    Ok(base64_img) => {
                        if let Some(reader_mutex) = SMTC_READER.get() {
                            let mut r = reader_mutex.lock().await;
                            r.last_thumbnail_base64 = base64_img;
                        }
                    }
                    Err(e) => {
                        eprintln!("[SMTC] Failed to read thumbnail: {:?}", e);
                    }
                }
            });
        }
    }

    if title.is_empty() {
        anyhow::bail!("empty title");
    }

    let position = timeline_properties.Position()?;
    let end_time = timeline_properties.EndTime()?;

    let raw_current_ms = (position.Duration / 10_000) as u64;
    let total_time_ms = (end_time.Duration / 10_000) as u64;
    let total_seconds = (end_time.Duration as f64) / 10_000_000.0;

    let is_playing = if let Ok(playback_info) = session.GetPlaybackInfo() {
        matches!(playback_info.PlaybackStatus(), Ok(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing))
    } else {
        false
    };

    let last_updated_100ns = timeline_properties
        .LastUpdatedTime()
        .map(|t| t.UniversalTime)
        .unwrap_or(0);
    let now_windows_ms = chrono::Utc::now().timestamp_millis() + WINDOWS_EPOCH_OFFSET_MS;
    let (corrected_current_ms, new_timeline_state) = correct_position(
        raw_current_ms,
        total_time_ms,
        is_playing,
        last_updated_100ns,
        now_windows_ms,
        *prev_timeline_state,
    );
    *prev_timeline_state = Some(new_timeline_state);

    let corrected_seconds = corrected_current_ms as f64 / 1000.0;
    let progress = if total_seconds > 0.0 {
        (corrected_seconds / total_seconds) * 100.0
    } else {
        0.0
    };

    let server_ts = chrono::Utc::now().timestamp_millis() as u64;

    Ok(SongInfo {
        title,
        artist,
        album,
        lyrics: String::new(),
        trans: String::new(),
        qrc_raw: String::new(),
        qrc_data: Vec::new(),
        current_time: corrected_seconds as u64,
        total_time: total_seconds as u64,
        current_time_ms: corrected_current_ms,
        total_time_ms,
        progress_percent: progress as f32,
        is_playing,
        album_pic_url,
        server_ts,
    })
}

// Function to get current media info from Windows SMTC using cached session.
pub async fn get_current_media_info() -> Result<Option<SongInfo>> {
    let mut info = get_current_media_info_inner().await?;
    if let Some(ref mut i) = info {
        if let Some(reader_mutex) = SMTC_READER.get() {
            let mut reader = reader_mutex.lock().await;
            // 记录最后一帧「SMTC 层面播放中」的对外位置，作为暂停淡出的起点锚
            if let Some(s) = reader.prev_timeline_state {
                if s.is_playing {
                    reader.last_playing_emitted = Some((s.emitted_ms, s.emitted_at_ms));
                }
            }
            apply_pause_fade(&mut reader, i);
            apply_resume_hold(&mut reader, i);
        }
    }
    Ok(info)
}

/// 暂停淡出后处理：暂停 ≠ 音乐立即停止。QQ 默认淡出 ~1s，期间声音
/// 与真实位置仍前进，但 SMTC Position 冻结，直到淡出结束 QQ 推送一次
/// 「最终位置」提交（比暂停时刻 ~+1.2s），恢复播放也从该位置继续。
/// 若按 SMTC 字面立刻冻结，前端先停住、提交到达时被迫前跳——表现为
/// "暂停后歌词自己往前走"。处理分两阶段：
/// 1. 报进阶段：以暂停前最后一帧播放位置为锚按 1× 报进（与听觉一致；
///    尾音低于 -60dB 时位置仍在前进，不钉住——冻结值即冻结时刻的推算值，
///    与前端插值显示连续，无回退）。QQ 可能在淡出进行中就写入最终位置
///    （提前提交）——只接受「临近尾声的提交」（is_tail_commit），
///    提前到达的一律忽略。
/// 2. 冻结阶段：淡出结构保持存活。提交微调（±100ms）接受；QQ 先刷的
///    落后旧锚点与淡出尾巴的迟到提交（向前 ≤1.5s）在宽限期内忽略
///    （后者在恢复播放时随锚点自然对齐）；其余移动视为真实 seek 放行。
fn apply_pause_fade(reader: &mut SmtcReaderInner, info: &mut SongInfo) {
    let song_key = format!("{}|{}", info.title, info.artist);
    let now_ms = chrono::Utc::now().timestamp_millis() + WINDOWS_EPOCH_OFFSET_MS;
    let cap_total = |v: u64| if info.total_time_ms > 0 { v.min(info.total_time_ms) } else { v };

    // 切歌：清除淡出状态
    if reader
        .pause_fade
        .as_ref()
        .map(|f| f.song_key != song_key)
        .unwrap_or(false)
    {
        reader.pause_fade = None;
    }

    if info.is_playing {
        if let Some(fade) = reader.pause_fade.take() {
            match fade.frozen_ms {
                None => {
                    // 淡出进行中恢复：QQ 的真实位置在淡出期间一直在前进，
                    // SMTC 此刻报的还是暂停前的旧锚点（落后 ~1s）。以淡出锚
                    // 延续推算，避免显示位置先回退再前跳。
                    let elapsed = (now_ms - fade.anchor_at_ms).max(0) as u64;
                    let pos = cap_total(fade.anchor_ms.saturating_add(elapsed.min(FADE_MAX_MS)));
                    set_info_position(info, pos, true);
                }
                Some(frozen) => {
                    // 冻结后恢复：QQ 从提交点继续（通常领先冻结值几十~几百ms）。
                    // info 此刻已是 QQ 的恢复点（权威），不覆盖；但把原显示
                    // 位置交给恢复 hold，让它先冻结在原位置，把对齐跳变
                    // 推迟到出声/锚定时刻
                    reader.resume_freeze_floor = Some(frozen);
                }
            }
        }
        reader.last_reported_playing = true;
        return;
    }

    let state = match reader.prev_timeline_state {
        Some(s) => s,
        None => return,
    };

    let Some(mut fade) = reader.pause_fade.clone() else {
        // 播放→暂停 切换帧：仅在上一帧确为播放时启动淡出
        if !reader.last_reported_playing {
            return;
        }
        let (anchor_ms, anchor_at_ms) = reader
            .last_playing_emitted
            .unwrap_or((info.current_time_ms, now_ms));
        let elapsed = (now_ms - anchor_at_ms).max(0) as u64;
        let sample = sample_audio_gate();
        let audible = sample.map(|s| s.peak > crate::audio_session::PEAK_SILENCE_THRESHOLD);
        reader.pause_fade = Some(PauseFade {
            anchor_ms,
            anchor_at_ms,
            ts_100ns: state.ts_100ns,
            song_key,
            silent_samples: if audible == Some(false) { 1 } else { 0 },
            started_at_ms: now_ms,
            frozen_ms: None,
        });
        let pos = cap_total(anchor_ms.saturating_add(elapsed.min(FADE_MAX_MS)));
        set_info_position(info, pos, true);
        reader.last_reported_playing = true;
        return;
    };

    let ts_changed = state.ts_100ns != fade.ts_100ns;

    // —— 冻结阶段：过滤后续 paused 刷新（旧锚点 vs 迟到提交 vs seek）——
    if let Some(frozen) = fade.frozen_ms {
        if ts_changed {
            fade.ts_100ns = state.ts_100ns;
            let raw = info.current_time_ms;
            if accept_frozen_refresh(raw, frozen, now_ms - fade.started_at_ms) {
                fade.frozen_ms = Some(cap_total(raw));
            }
        }
        set_info_position(info, fade.frozen_ms.unwrap_or(frozen), false);
        reader.pause_fade = Some(fade);
        reader.last_reported_playing = false;
        return;
    }

    // —— 报进阶段 ——
    let elapsed = (now_ms - fade.anchor_at_ms).max(0) as u64;
    let sample = sample_audio_gate();
    let audible = sample.map(|s| s.peak > crate::audio_session::PEAK_SILENCE_THRESHOLD);
    let tail_commit = ts_changed && is_tail_commit(info.current_time_ms, fade.anchor_ms, elapsed);
    let action = if tail_commit {
        FadeAction::FreezeRaw
    } else {
        fade_step(elapsed, audible, fade.silent_samples)
    };
    if audible.unwrap_or(true) {
        fade.silent_samples = 0;
    } else {
        fade.silent_samples += 1;
    }

    match action {
        FadeAction::Advance => {
            let pos = cap_total(fade.anchor_ms.saturating_add(elapsed.min(FADE_MAX_MS)));
            set_info_position(info, pos, true);
            fade.ts_100ns = state.ts_100ns;
            reader.pause_fade = Some(fade);
            reader.last_reported_playing = true;
        }
        FadeAction::FreezeTail => {
            // 冻结在冻结时刻的推算值：与前端插值显示连续，无回退
            let pos = cap_total(fade.anchor_ms.saturating_add(elapsed.min(FADE_MAX_MS)));
            set_info_position(info, pos, false);
            fade.frozen_ms = Some(pos);
            fade.ts_100ns = state.ts_100ns;
            reader.pause_fade = Some(fade);
            reader.last_reported_playing = false;
        }
        FadeAction::FreezeRaw => {
            // 临近尾声的提交：与报进位置几乎连续，直接采用提交值冻结
            let pos = cap_total(info.current_time_ms);
            set_info_position(info, pos, false);
            fade.frozen_ms = Some(pos);
            fade.ts_100ns = state.ts_100ns;
            reader.pause_fade = Some(fade);
            reader.last_reported_playing = false;
        }
    }
}

fn set_info_position(info: &mut SongInfo, pos_ms: u64, is_playing: bool) {
    info.is_playing = is_playing;
    info.current_time_ms = pos_ms;
    info.current_time = pos_ms / 1000;
    info.progress_percent = if info.total_time_ms > 0 {
        (pos_ms as f64 / info.total_time_ms as f64 * 100.0) as f32
    } else {
        0.0
    };
}

/// 恢复 hold 后处理：暂停→播放时若音频尚未真正渲染（蓝牙重新建链 /
/// 设备慢启动 / QQ 管线延迟），把对外位置冻结在恢复点并标记暂停。
/// 此期间 QQ 自己的 Position 同样冻结，两边天然对齐；
/// 解除（出声 / QQ 位置移动 / 超时兜底）后从冻结点平滑继续，无回滚。
fn apply_resume_hold(reader: &mut SmtcReaderInner, info: &mut SongInfo) {
    let state = reader.prev_timeline_state;
    let from_pause = state.map(|s| s.from_pause).unwrap_or(false);

    // 只有「hold 中」或「暂停→播放切换帧」才需要音频采样
    if reader.resume_hold.is_none() && !(info.is_playing && from_pause) {
        return;
    }
    let song_key = format!("{}|{}", info.title, info.artist);
    let sample = sample_audio_gate();

    if let Some(hold) = reader.resume_hold.clone() {
        let anchored_moved = state
            .map(|s| s.kind == CorrectionKind::Anchored)
            .unwrap_or(false)
            && info.current_time_ms > hold.position_ms + HOLD_POSITION_MOVED_MS;

        match decide_resume_hold(
            true,
            false,
            info.is_playing,
            hold.song_key == song_key,
            anchored_moved,
            sample,
            hold.since.elapsed(),
        ) {
            HoldAction::Hold => {
                freeze_info_position(info, hold.position_ms);
                // 把内部锚点写回冻结位置：航位推算不会空跑，
                // 解除后从冻结点平滑继续（不回滚、不前跳）
                if let Some(mut s) = reader.prev_timeline_state {
                    s.emitted_ms = hold.position_ms;
                    s.emitted_at_ms =
                        chrono::Utc::now().timestamp_millis() + WINDOWS_EPOCH_OFFSET_MS;
                    reader.prev_timeline_state = Some(s);
                }
            }
            _ => {
                reader.resume_hold = None;
            }
        }
        return;
    }

    // 进入 hold：暂停→播放切换帧、未出声、且能采样到 QQ 会话
    if decide_resume_hold(false, from_pause, info.is_playing, true, false, sample, Default::default())
        == HoldAction::Enter
    {
        // 冻结后恢复：优先冻结在原显示位置（resume_freeze_floor）。
        // QQ 恢复点领先时：对齐跳变推迟到出声/锚定时刻；
        // QQ 恢复点落后时（少见）：前端 hold-in-place 等服务端追平，
        // 两个方向都不在静默期产生位置跳变
        let freeze_at = reader
            .resume_freeze_floor
            .take()
            .unwrap_or(info.current_time_ms);
        reader.resume_hold = Some(ResumeHold {
            position_ms: freeze_at,
            song_key,
            since: std::time::Instant::now(),
        });
        freeze_info_position(info, freeze_at);
    } else {
        reader.resume_freeze_floor = None;
    }
}

fn freeze_info_position(info: &mut SongInfo, pos_ms: u64) {
    set_info_position(info, pos_ms, false);
}

async fn get_current_media_info_inner() -> Result<Option<SongInfo>> {
    // Lazily initialize the global SMTC reader
    let reader_mutex = match SMTC_READER.get() {
        Some(m) => m,
        None => {
            let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.await?;
            // 会话集合变化（QQ 启动/退出/重建会话）时唤醒主循环重扫
            let _ = manager.SessionsChanged(&TypedEventHandler::new(|_, _| {
                fire_state_changed();
                Ok(())
            }));
            let inner = SmtcReaderInner {
                manager,
                current_session: None,
                last_song_key: None,
                last_thumbnail_base64: String::new(),
                prev_timeline_state: None,
                event_guard: None,
                resume_hold: None,
                pause_fade: None,
                resume_freeze_floor: None,
                last_reported_playing: false,
                last_playing_emitted: None,
            };
            let _ = SMTC_READER.set(Mutex::new(inner));
            SMTC_READER.get().unwrap()
        }
    };

    let mut reader = reader_mutex.lock().await;
    // reborrow 为 &mut SmtcReaderInner，使 current_session 与
    // prev_timeline_state 的不相交字段借用能同时成立（MutexGuard 经
    // Deref 访问字段会被当作借用整个 guard）
    let reader = &mut *reader;

    // 1. Try reading from the cached session first (do NOT fetch thumbnail initially to save CPU)
    if let Some(session) = &reader.current_session {
        if let Ok(mut info) = read_session_info(session, false, &mut reader.prev_timeline_state).await {
            let song_key = format!("{}|{}", info.title, info.artist);
            if let Some(last_key) = &reader.last_song_key {
                if song_key == *last_key {
                    // Cache hit: same song and session is valid.
                    // Populate with the cached thumbnail base64 and return immediately.
                    info.album_pic_url = reader.last_thumbnail_base64.clone();
                    return Ok(Some(info));
                }
            }
        }
        // Cache miss or read error: clear the cache and fall back to full scan
        reader.current_session = None;
        reader.last_song_key = None;
        reader.last_thumbnail_base64 = String::new();
        reader.event_guard = None;
    }

    // 2. Scan all active sessions for QQ Music
    let mut qqmusic_session = None;
    if let Ok(sessions) = reader.manager.GetSessions() {
        if let Ok(size) = sessions.Size() {
            for i in 0..size {
                if let Ok(session) = sessions.GetAt(i) {
                    if is_qqmusic_session(&session) {
                        qqmusic_session = Some(session);
                        break;
                    }
                }
            }
        }
    }

    let mut qqmusic_found = false;
    if let Some(session) = qqmusic_session {
        qqmusic_found = true;
        // Cache miss: must fetch thumbnail here exactly once
        if let Ok(info) = read_session_info(&session, true, &mut reader.prev_timeline_state).await {
            reader.last_song_key = Some(format!("{}|{}", info.title, info.artist));
            reader.last_thumbnail_base64 = info.album_pic_url.clone();
            reader.event_guard = register_session_events(&session);
            reader.current_session = Some(session);
            return Ok(Some(info));
        }
    }

    // If QQ Music session was found but read failed, do not downgrade to other media sources
    if qqmusic_found {
        return Ok(None);
    }

    // 3. Fallback: try default current session
    if let Ok(session) = reader.manager.GetCurrentSession() {
        // Cache miss: must fetch thumbnail here exactly once
        if let Ok(info) = read_session_info(&session, true, &mut reader.prev_timeline_state).await {
            reader.last_song_key = Some(format!("{}|{}", info.title, info.artist));
            reader.last_thumbnail_base64 = info.album_pic_url.clone();
            reader.event_guard = register_session_events(&session);
            reader.current_session = Some(session);
            return Ok(Some(info));
        }
    }

    Ok(None)
}

pub async fn try_toggle_play_pause() -> Result<()> {
    let reader_mutex = SMTC_READER.get().context("SMTC reader not initialized")?;
    let reader = reader_mutex.lock().await;
    if let Some(session) = &reader.current_session {
        session.TryTogglePlayPauseAsync()?.await?;
    }
    Ok(())
}

pub async fn try_skip_next() -> Result<()> {
    let reader_mutex = SMTC_READER.get().context("SMTC reader not initialized")?;
    let reader = reader_mutex.lock().await;
    if let Some(session) = &reader.current_session {
        session.TrySkipNextAsync()?.await?;
    }
    Ok(())
}

pub async fn try_skip_previous() -> Result<()> {
    let reader_mutex = SMTC_READER.get().context("SMTC reader not initialized")?;
    let reader = reader_mutex.lock().await;
    if let Some(session) = &reader.current_session {
        session.TrySkipPreviousAsync()?.await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 任意的 Windows 纪元毫秒时刻
    const NOW: i64 = 20_000_000_000;

    fn ms_to_100ns(ms: i64) -> i64 {
        ms * 10_000
    }

    fn anchor(is_playing: bool, ts_100ns: i64, emitted_ms: u64, emitted_at_ms: i64) -> Option<TimelineState> {
        Some(TimelineState {
            is_playing,
            ts_100ns,
            emitted_ms,
            emitted_at_ms,
            kind: CorrectionKind::Raw,
            from_pause: false,
        })
    }

    fn correct(raw: u64, total: u64, playing: bool, ts: i64, now: i64, prev: Option<TimelineState>) -> u64 {
        let (v, state) = correct_position(raw, total, playing, ts, now, prev);
        assert_eq!(state.emitted_ms, v);
        assert_eq!(state.emitted_at_ms, now);
        assert_eq!(state.is_playing, playing);
        v
    }

    #[test]
    fn fresh_snapshot_adds_elapsed() {
        let ts = ms_to_100ns(NOW - 300);
        // 首帧（prev=None）或 ts 变化 → 以快照为锚
        assert_eq!(correct(10_000, 200_000, true, ts, NOW, None), 10_300);
        assert_eq!(
            correct(10_000, 200_000, true, ts, NOW, anchor(true, ms_to_100ns(NOW - 800), 9_500, NOW - 100)),
            10_300
        );
    }

    #[test]
    fn playing_unchanged_timeline_dead_reckons() {
        // 播放中快照未更新：位置 = 上一帧发出值 + 真实流逝，与 raw 无关
        let ts = ms_to_100ns(NOW - 1_000);
        let prev = anchor(true, ts, 10_500, NOW - 100);
        assert_eq!(correct(10_000, 200_000, true, ts, NOW, prev), 10_600);
    }

    #[test]
    fn dead_reckoning_never_freezes_during_snapshot_gap() {
        // 模拟 3s 快照间隙：位置持续随真实时间前进，不冻结、不回退
        let ts = ms_to_100ns(NOW - 3_000);
        let mut prev = anchor(true, ts, 10_000, NOW - 3_000);
        let mut last = 10_000;
        for i in 1..=30 {
            let now = NOW - 3_000 + i * 100;
            let (v, state) = correct_position(10_000, 200_000, true, ts, now, prev);
            assert!(v > last, "位置必须单调前进");
            last = v;
            prev = Some(state);
        }
        assert_eq!(last, 13_000);
    }

    #[test]
    fn dead_reckoning_dt_is_clamped() {
        // 系统时钟回拨 → dt=0；休眠 10s → dt clamp 5s
        let ts = ms_to_100ns(NOW - 1_000);
        let prev = anchor(true, ts, 10_000, NOW + 500);
        assert_eq!(correct(10_000, 200_000, true, ts, NOW, prev), 10_000);
        let prev = anchor(true, ts, 10_000, NOW - 10_000);
        assert_eq!(correct(10_000, 200_000, true, ts, NOW, prev), 15_000);
    }

    #[test]
    fn resume_with_unrefreshed_timeline_uses_raw_regardless_of_pause_length() {
        // 长暂停 30s
        let ts = ms_to_100ns(NOW - 30_000);
        assert_eq!(
            correct(10_000, 200_000, true, ts, NOW, anchor(false, ts, 10_000, NOW - 30_000)),
            10_000
        );
        // 短暂停 800ms（elapsed < 1500，同样不能校正）
        let ts = ms_to_100ns(NOW - 800);
        assert_eq!(
            correct(10_000, 200_000, true, ts, NOW, anchor(false, ts, 10_000, NOW - 800)),
            10_000
        );
    }

    #[test]
    fn resume_then_dead_reckons_until_fresh_snapshot() {
        // 恢复播放后 timeline 一直未刷新：raw → 航位推算平滑前进
        let ts = ms_to_100ns(NOW - 30_000);
        let (v1, s1) = correct_position(10_000, 200_000, true, ts, NOW, anchor(false, ts, 10_000, NOW - 30_000));
        assert_eq!(v1, 10_000);
        let (v2, _) = correct_position(10_000, 200_000, true, ts, NOW + 100, Some(s1));
        assert_eq!(v2, 10_100);
    }

    #[test]
    fn resume_with_refreshed_timeline_corrects_normally() {
        let old_ts = ms_to_100ns(NOW - 30_000);
        let new_ts = ms_to_100ns(NOW - 100);
        assert_eq!(
            correct(10_000, 200_000, true, new_ts, NOW, anchor(false, old_ts, 10_000, NOW - 30_000)),
            10_100
        );
    }

    #[test]
    fn stale_snapshot_beyond_1500ms_falls_back_to_raw() {
        let ts = ms_to_100ns(NOW - 2_000);
        assert_eq!(correct(10_000, 200_000, true, ts, NOW, None), 10_000);
    }

    #[test]
    fn fresh_snapshot_elapsed_is_clamped_to_1000ms() {
        let ts = ms_to_100ns(NOW - 1_200);
        assert_eq!(correct(10_000, 200_000, true, ts, NOW, None), 11_000);
    }

    #[test]
    fn paused_position_is_not_corrected() {
        let ts = ms_to_100ns(NOW - 300);
        assert_eq!(correct(10_000, 200_000, false, ts, NOW, anchor(false, ts, 10_000, NOW)), 10_000);
    }

    #[test]
    fn invalid_timestamp_uses_raw() {
        assert_eq!(correct(10_000, 200_000, true, 0, NOW, None), 10_000);
    }

    #[test]
    fn corrected_position_is_capped_at_total() {
        let ts = ms_to_100ns(NOW - 300);
        assert_eq!(correct(199_900, 200_000, true, ts, NOW, None), 200_000);
        // 航位推算同样封顶
        let prev = anchor(true, ts, 199_950, NOW - 100);
        assert_eq!(correct(199_900, 200_000, true, ts, NOW, prev), 200_000);
    }

    #[test]
    fn resume_stale_marks_from_pause_and_kind() {
        let ts = ms_to_100ns(NOW - 30_000);
        let (_, s) = correct_position(10_000, 200_000, true, ts, NOW, anchor(false, ts, 10_000, NOW - 30_000));
        assert!(s.from_pause);
        assert_eq!(s.kind, CorrectionKind::ResumeStale);
        // 下一帧（播放中、timeline 未变）：from_pause 只存在于切换帧
        let (_, s2) = correct_position(10_000, 200_000, true, ts, NOW + 100, Some(s));
        assert!(!s2.from_pause);
        assert_eq!(s2.kind, CorrectionKind::DeadReckon);
    }

    #[test]
    fn anchored_after_pause_marks_from_pause() {
        let old_ts = ms_to_100ns(NOW - 30_000);
        let new_ts = ms_to_100ns(NOW - 100);
        let (_, s) = correct_position(10_000, 200_000, true, new_ts, NOW, anchor(false, old_ts, 10_000, NOW - 30_000));
        assert!(s.from_pause);
        assert_eq!(s.kind, CorrectionKind::Anchored);
    }

    #[test]
    fn steady_playing_clears_from_pause() {
        let ts = ms_to_100ns(NOW - 300);
        let (_, s) = correct_position(10_000, 200_000, true, ts, NOW, anchor(true, ms_to_100ns(NOW - 800), 9_500, NOW - 100));
        assert!(!s.from_pause);
        assert_eq!(s.kind, CorrectionKind::Anchored);
    }

    // —— decide_resume_hold ——

    fn audio_sample(peak: f32) -> Option<crate::audio_session::AudioSample> {
        Some(crate::audio_session::AudioSample { peak, any_active: true })
    }

    #[test]
    fn hold_enters_on_resume_with_silence() {
        assert_eq!(
            decide_resume_hold(false, true, true, true, false, audio_sample(0.0), Default::default()),
            HoldAction::Enter
        );
    }

    #[test]
    fn hold_not_entered_when_already_audible() {
        assert_eq!(
            decide_resume_hold(false, true, true, true, false, audio_sample(0.5), Default::default()),
            HoldAction::None
        );
    }

    #[test]
    fn hold_not_entered_without_session() {
        // 无 QQ 音频会话 → 回退旧行为（不 hold）
        assert_eq!(
            decide_resume_hold(false, true, true, true, false, None, Default::default()),
            HoldAction::None
        );
    }

    #[test]
    fn hold_persists_while_silent_and_unmoved() {
        assert_eq!(
            decide_resume_hold(true, false, true, true, false, audio_sample(0.0), std::time::Duration::from_millis(500)),
            HoldAction::Hold
        );
    }

    #[test]
    fn hold_releases_on_audio_peak() {
        assert_eq!(
            decide_resume_hold(true, false, true, true, false, audio_sample(0.5), std::time::Duration::from_millis(500)),
            HoldAction::Release
        );
    }

    #[test]
    fn hold_releases_on_position_movement() {
        assert_eq!(
            decide_resume_hold(true, false, true, true, true, audio_sample(0.0), std::time::Duration::from_millis(500)),
            HoldAction::Release
        );
    }

    #[test]
    fn hold_releases_on_timeout() {
        assert_eq!(
            decide_resume_hold(true, false, true, true, false, audio_sample(0.0), RESUME_HOLD_TIMEOUT + std::time::Duration::from_millis(1)),
            HoldAction::Release
        );
    }

    #[test]
    fn hold_releases_on_pause_or_song_change() {
        assert_eq!(
            decide_resume_hold(true, false, false, true, false, audio_sample(0.0), std::time::Duration::from_millis(100)),
            HoldAction::Release
        );
        assert_eq!(
            decide_resume_hold(true, false, true, false, false, audio_sample(0.0), std::time::Duration::from_millis(100)),
            HoldAction::Release
        );
    }

    // —— fade_step（暂停淡出报进阶段）——

    #[test]
    fn fade_advances_while_audible_within_window() {
        assert_eq!(fade_step(300, Some(true), 0), FadeAction::Advance);
        // 无音频会话（None）按有声处理，超时兜底
        assert_eq!(fade_step(300, None, 0), FadeAction::Advance);
        // 静音未达次数：尾音防抖，继续报进（位置保持不再前进）
        assert_eq!(fade_step(300, Some(false), 0), FadeAction::Advance);
        // timeline 刷新不再直接结束报进（提前提交必须忽略，见 is_tail_commit）
        assert_eq!(fade_step(300, Some(true), 0), FadeAction::Advance);
    }

    #[test]
    fn fade_freezes_tail_on_sustained_silence() {
        // 连续静音达到次数（淡出尾音结束 / 用户关闭淡出）
        assert_eq!(
            fade_step(300, Some(false), FADE_SILENT_SAMPLES_END - 1),
            FadeAction::FreezeTail
        );
    }

    #[test]
    fn fade_freezes_on_timeout() {
        assert_eq!(fade_step(FADE_MAX_MS, Some(true), 0), FadeAction::FreezeTail);
        assert_eq!(fade_step(FADE_MAX_MS + 500, None, 0), FadeAction::FreezeTail);
    }

    // —— is_tail_commit（报进阶段的提交判定）——

    #[test]
    fn tail_commit_accepts_only_near_end_forward_refresh() {
        let anchor = 100_000;
        // 临近尾声（elapsed≈淡出时长）：向前刷新 = 提交，接受
        assert!(is_tail_commit(101_200, anchor, 1_100));
        // 提前提交：淡出才走 300ms，值已是 +1.2s 的最终值 → 忽略（音乐会瞬移）
        assert!(!is_tail_commit(101_200, anchor, 300));
        // 落后/等于锚点的刷新 = 旧锚点，忽略
        assert!(!is_tail_commit(99_000, anchor, 500));
        assert!(!is_tail_commit(anchor, anchor, 500));
    }

    // —— accept_frozen_refresh（冻结阶段的刷新过滤）——

    #[test]
    fn frozen_refresh_accepts_fine_commit_adjustment() {
        // 提交微调（±100ms 内，两个方向）接受
        assert!(accept_frozen_refresh(108_400, 108_310, 1_000));
        assert!(accept_frozen_refresh(108_220, 108_310, 1_000));
        assert!(accept_frozen_refresh(108_310, 108_310, 1_000));
    }

    #[test]
    fn frozen_refresh_ignores_late_commit_tail_within_grace() {
        // 宽限期内向前 ≤1.5s = 淡出尾巴的迟到提交，忽略（恢复播放时自然对齐）
        assert!(!accept_frozen_refresh(122_629, 122_149, 2_100));
        // 超过 1.5s 的向前移动 = 真实向前 seek，接受
        assert!(accept_frozen_refresh(130_000, 122_149, 2_100));
        // 宽限期后的向前移动 = 真实 seek，接受
        assert!(accept_frozen_refresh(123_000, 122_149, PAUSE_STALE_GRACE_MS + 1));
    }

    #[test]
    fn frozen_refresh_ignores_stale_anchor_within_grace() {
        // QQ 暂停后先刷的落后旧锚点在宽限期内必须忽略
        assert!(!accept_frozen_refresh(111_481, 112_584, 1_660));
        assert!(!accept_frozen_refresh(107_284, 112_584, PAUSE_STALE_GRACE_MS));
        // 宽限期后的向后移动 = 用户真实 seek，接受
        assert!(accept_frozen_refresh(60_000, 112_584, PAUSE_STALE_GRACE_MS + 1));
    }
}

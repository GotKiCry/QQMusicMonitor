//! WASAPI 音频会话监控 —— 「QQ音乐是否真的在出声」的实时地面真值。
//!
//! SMTC 只能告诉你播放器「声称」的状态，且上报有延迟；
//! 音频会话的峰值电平/会话状态来自音频引擎本身：
//! - 恢复播放：电平出现 = 声音真的开始渲染（覆盖蓝牙重新建链、设备慢启动的间隙）
//! - 播放中持续静音：卡顿/缓冲停滞（SMTC 仍声称 playing）
//! - 会话 Inactive：客户端停止了音频流——只发生在真实暂停/切歌间隙，
//!   歌曲中的静音段落不会触发（客户端仍在流式输出静音采样）
//!
//! 找不到 QQ 音频会话时整体回退为纯 SMTC 行为。

use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, eRender, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// 峰值电平低于此值视为静音（-60dB）
pub(crate) const PEAK_SILENCE_THRESHOLD: f32 = 0.001;
/// 播放中持续静音超过该时长判定为停滞（缓冲等）。需大于歌曲中常见的安静段落
const STALL_SILENCE: Duration = Duration::from_millis(2000);
/// 会话停止流持续该时长即判定真实暂停（防抖）
const INACTIVE_PAUSE: Duration = Duration::from_millis(150);
/// 会话解析成功后的刷新周期
const RESOLVE_OK_INTERVAL: Duration = Duration::from_secs(10);
/// 未找到会话时的重试周期
const RESOLVE_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// 门控裁决：如何修正 SMTC 声称的播放状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// 维持 SMTC 的 is_playing
    Pass,
    /// 立即按暂停显示（冻结在当前显示位置，由前端烘焙基线；不回卷，
    /// 真实位置由 QQ 下一次 timeline 锚定来修正）
    ForcePause,
}

/// 一次音频会话采样（COM 层的输出，纯数据，便于测试）
#[derive(Debug, Clone, Copy)]
pub struct AudioSample {
    /// 所有 QQ 会话的峰值电平最大值（0..1）
    pub peak: f32,
    /// 任一会话处于 Active
    pub any_active: bool,
}

/// 门控状态机（纯逻辑，不含 COM）
#[derive(Default)]
struct GateState {
    silence_since: Option<Instant>,
    inactive_since: Option<Instant>,
}

impl GateState {
    fn reset(&mut self) {
        self.silence_since = None;
        self.inactive_since = None;
    }

    fn decide(
        &mut self,
        smtc_playing: bool,
        sample: Option<AudioSample>,
        now: Instant,
    ) -> GateDecision {
        let Some(sample) = sample else {
            // 无 QQ 音频会话 → 回退纯 SMTC
            self.silence_since = None;
            self.inactive_since = None;
            return GateDecision::Pass;
        };

        let audible = sample.peak > PEAK_SILENCE_THRESHOLD;
        if audible {
            self.silence_since = None;
        } else if self.silence_since.is_none() {
            self.silence_since = Some(now);
        }
        if sample.any_active {
            self.inactive_since = None;
        } else if self.inactive_since.is_none() {
            self.inactive_since = Some(now);
        }

        // SMTC 已暂停：直接透传（暂停检测由 SMTC 事件驱动，已经足够快）
        if !smtc_playing {
            return GateDecision::Pass;
        }

        // 播放中持续静音 → 停滞（缓冲）。冻结在当前位置，不回卷——
        // QQ 恢复后会推送新 timeline 锚定，位置由它修正
        if let Some(since) = self.silence_since {
            if now.duration_since(since) >= STALL_SILENCE {
                return GateDecision::ForcePause;
            }
        }

        // 会话停止流但 SMTC 还没报暂停 → 提前按暂停显示
        if !audible {
            if let Some(since) = self.inactive_since {
                if now.duration_since(since) >= INACTIVE_PAUSE {
                    return GateDecision::ForcePause;
                }
            }
        }

        GateDecision::Pass
    }
}

struct SessionMeter {
    control: IAudioSessionControl,
    meter: IAudioMeterInformation,
}

pub struct AudioGate {
    state: GateState,
    sessions: Vec<SessionMeter>,
    last_resolve: Option<Instant>,
}

impl AudioGate {
    pub fn new() -> Self {
        Self {
            state: GateState::default(),
            sessions: Vec::new(),
            last_resolve: None,
        }
    }

    /// 在监控线程上初始化 COM（重复调用安全）
    pub fn ensure_com_init() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    /// 切歌时调用：清空门控状态（不影响会话缓存）
    pub fn reset(&mut self) {
        self.state.reset();
    }

    /// 仅采样一次音频会话（供 smtc.rs 恢复 hold 使用）
    pub fn sample(&mut self) -> Option<AudioSample> {
        let now = Instant::now();
        self.refresh_sessions(now);
        self.sample_sessions()
    }

    pub fn update(&mut self, smtc_playing: bool) -> GateDecision {
        let now = Instant::now();
        let sample = self.sample();
        self.state.decide(smtc_playing, sample, now)
    }

    fn refresh_sessions(&mut self, now: Instant) {
        let interval = if self.sessions.is_empty() {
            RESOLVE_RETRY_INTERVAL
        } else {
            RESOLVE_OK_INTERVAL
        };
        let due = self
            .last_resolve
            .map(|t| now.duration_since(t) >= interval)
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_resolve = Some(now);
        self.sessions = find_qqmusic_sessions();
    }

    fn sample_sessions(&mut self) -> Option<AudioSample> {
        if self.sessions.is_empty() {
            return None;
        }
        let mut peak = 0f32;
        let mut any_active = false;
        // 查询失败的会话视为失效，丢弃后等待下次重解析
        self.sessions.retain(|s| {
            let p = unsafe { s.meter.GetPeakValue() };
            match p {
                Ok(p) => {
                    peak = peak.max(p);
                    if let Ok(st) = unsafe { s.control.GetState() } {
                        if st == AudioSessionStateActive {
                            any_active = true;
                        }
                    }
                    true
                }
                Err(_) => false,
            }
        });
        if self.sessions.is_empty() {
            None
        } else {
            Some(AudioSample { peak, any_active })
        }
    }
}

fn find_qqmusic_sessions() -> Vec<SessionMeter> {
    let pids = find_qqmusic_pids();
    if pids.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    unsafe {
        let Ok(enumerator) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return out;
        };
        let Ok(endpoints) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else {
            return out;
        };
        let count = endpoints.GetCount().unwrap_or(0);
        for i in 0..count {
            let Ok(device) = endpoints.Item(i) else { continue };
            let Ok(session_mgr) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)
            else {
                continue;
            };
            let Ok(session_enum) = session_mgr.GetSessionEnumerator() else { continue };
            let Ok(n) = session_enum.GetCount() else { continue };
            for j in 0..n {
                let Ok(control) = session_enum.GetSession(j) else { continue };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                let Ok(pid) = control2.GetProcessId() else { continue };
                if !pids.contains(&pid) {
                    continue;
                }
                if let Ok(meter) = control.cast::<IAudioMeterInformation>() {
                    out.push(SessionMeter { control, meter });
                }
            }
        }
    }
    out
}

fn find_qqmusic_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return pids;
        };
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                if let Ok(name) = String::from_utf16(&entry.szExeFile[..len]) {
                    // QQMusic.exe / QQMusicExternal.exe 等
                    if name.to_ascii_lowercase().starts_with("qqmusic") {
                        pids.push(entry.th32ProcessID);
                    }
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(peak: f32, any_active: bool) -> Option<AudioSample> {
        Some(AudioSample { peak, any_active })
    }

    fn audible() -> Option<AudioSample> {
        sample(0.5, true)
    }

    fn silent() -> Option<AudioSample> {
        sample(0.0, true)
    }

    #[test]
    fn no_session_falls_back_to_smtc() {
        let mut g = GateState::default();
        let now = Instant::now();
        assert_eq!(g.decide(true, None, now), GateDecision::Pass);
        assert_eq!(g.decide(false, None, now), GateDecision::Pass);
    }

    #[test]
    fn smtc_paused_passes_through() {
        let mut g = GateState::default();
        let now = Instant::now();
        assert_eq!(g.decide(false, silent(), now), GateDecision::Pass);
    }

    #[test]
    fn brief_silence_does_not_freeze() {
        let mut g = GateState::default();
        let t0 = Instant::now();
        g.decide(true, audible(), t0);
        // 1s 静音（歌曲安静段落）→ 不判定停滞
        assert_eq!(
            g.decide(true, silent(), t0 + Duration::from_millis(500)),
            GateDecision::Pass
        );
        assert_eq!(
            g.decide(true, silent(), t0 + Duration::from_millis(1500)),
            GateDecision::Pass
        );
    }

    #[test]
    fn sustained_silence_is_stall_force_pause() {
        let mut g = GateState::default();
        let t0 = Instant::now();
        g.decide(true, audible(), t0);
        g.decide(true, silent(), t0 + Duration::from_millis(100));
        assert_eq!(
            g.decide(true, silent(), t0 + Duration::from_millis(2500)),
            GateDecision::ForcePause
        );
    }

    #[test]
    fn inactive_session_fast_pauses() {
        let mut g = GateState::default();
        let t0 = Instant::now();
        g.decide(true, audible(), t0);
        // 会话停止流（真实暂停），SMTC 还没报
        assert_eq!(
            g.decide(true, sample(0.0, false), t0 + Duration::from_millis(100)),
            GateDecision::Pass // 150ms 防抖内
        );
        assert_eq!(
            g.decide(true, sample(0.0, false), t0 + Duration::from_millis(300)),
            GateDecision::ForcePause
        );
    }

    #[test]
    fn inactive_but_audible_does_not_pause() {
        let mut g = GateState::default();
        let t0 = Instant::now();
        g.decide(true, audible(), t0);
        // any_active=false 但仍有声音（状态抖动）→ 不暂停
        assert_eq!(
            g.decide(true, sample(0.3, false), t0 + Duration::from_millis(300)),
            GateDecision::Pass
        );
    }

    #[test]
    fn reset_clears_pending_states() {
        let mut g = GateState::default();
        let t0 = Instant::now();
        g.decide(true, silent(), t0);
        g.reset();
        // 重置后静音时长清零，重新计时
        assert_eq!(
            g.decide(true, silent(), t0 + Duration::from_millis(100)),
            GateDecision::Pass
        );
    }
}

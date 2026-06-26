use windows::Win32::Media::Audio::{
    IAudioClock, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEnumerator, IAudioSessionManager2,
    IMMDeviceEnumerator, AudioSessionStateActive,
    eRender, eConsole,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::core::{Interface, GUID};
use std::sync::OnceLock;
use tokio::sync::Mutex;

struct AudioClockState {
    clock: IAudioClock,
    frequency: u64,
}

unsafe impl Send for AudioClockState {}

static AUDIO_CLOCK: OnceLock<Mutex<Option<AudioClockState>>> = OnceLock::new();

fn audio_clock_mutex() -> &'static Mutex<Option<AudioClockState>> {
    AUDIO_CLOCK.get_or_init(|| Mutex::new(None))
}

const CLSID_MMDEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);

unsafe fn try_init_com() {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
}

unsafe fn find_audio_session() -> Option<(IAudioClock, u64)> {
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_INPROC_SERVER).ok()?;
    let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
    let manager: IAudioSessionManager2 =
        device.Activate::<IAudioSessionManager2>(CLSCTX_INPROC_SERVER, None).ok()?;
    let sessions: IAudioSessionEnumerator = manager.GetSessionEnumerator().ok()?;
    let count = sessions.GetCount().ok()?;

    for i in 0..count {
        let ctl: IAudioSessionControl = sessions.GetSession(i).ok()?;
        let ctl2: IAudioSessionControl2 = ctl.cast::<IAudioSessionControl2>().ok()?;

        if ctl2.IsSystemSoundsSession().is_ok() {
            continue;
        }

        let state = ctl.GetState().ok()?;
        if state != AudioSessionStateActive {
            continue;
        }

        let pid = ctl2.GetProcessId().ok()?;
        if !is_qqmusic_process(pid) {
            continue;
        }

        let clock: IAudioClock = ctl.cast::<IAudioClock>().ok()?;
        let frequency = clock.GetFrequency().ok()?;
        return Some((clock, frequency));
    }
    None
}

fn is_qqmusic_process(pid: u32) -> bool {
    unsafe {
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;
        use windows::Win32::Foundation::CloseHandle;

        if pid == 0 {
            return false;
        }
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut name_buf = [0u16; 260];
        let len = GetModuleBaseNameW(handle, None, &mut name_buf);
        let _ = CloseHandle(handle);
        if len == 0 {
            return false;
        }
        let name = String::from_utf16_lossy(&name_buf[..len as usize]).to_lowercase();
        name.contains("qqmusic") || name.contains("qq音乐")
    }
}

pub async fn refresh_audio_clock() {
    unsafe { try_init_com(); }
    let result = unsafe { find_audio_session() };
    let mut guard = audio_clock_mutex().lock().await;
    *guard = result.map(|(c, f)| AudioClockState { clock: c, frequency: f });
}

pub async fn get_playback_position_ms() -> Option<u64> {
    let guard = audio_clock_mutex().lock().await;
    let state = guard.as_ref()?;
    let freq = state.frequency;
    if freq == 0 {
        return None;
    }

    unsafe {
        let mut pos: u64 = 0;
        state.clock.GetPosition(&mut pos, None).ok()?;
        Some(pos * 1000 / freq)
    }
}

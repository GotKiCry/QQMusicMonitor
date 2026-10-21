// Hide the console window in release builds; keep it in debug builds for logs.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(dead_code)]
use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU64, Ordering};
use std::sync::OnceLock;
use tokio::sync::RwLock;

static CONFIG: OnceLock<RwLock<Config>> = OnceLock::new();
static CACHED_CACHE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
static LATEST_SONG_INFO: OnceLock<RwLock<Arc<SongInfo>>> = OnceLock::new();
static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

/// 窗口是否可见（后台托盘模式时为 false）。
/// 隐藏期间跳过 GUI 事件推送，并在无同步服务时降低 SMTC 轮询频率以释放资源。
static GUI_VISIBLE: AtomicBool = AtomicBool::new(true);

/// 状态栏歌词（悬浮歌词条窗口）是否启用。
/// 启用时即使主窗口隐藏到托盘，GUI 事件推送与轮询频率也保持不变。
static LYRIC_BAR_ENABLED: AtomicBool = AtomicBool::new(false);

/// 歌词条位置偏好：0 = 右侧（通知区左侧），1 = 左侧（开始按钮右侧）
static LYRIC_BAR_SIDE: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(0);

/// 歌词条边距偏好（逻辑像素）：距任务栏屏幕边缘的距离（左=距左缘，右=距通知区左缘）
static LYRIC_BAR_GAP: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// 歌词条宽度偏好（逻辑像素）：窗口最大宽度，任务栏可用空间不足时自动收缩。
/// 默认 LYRIC_BAR_DEFAULT_W（520），设置项可调 LYRIC_BAR_MIN_W..LYRIC_BAR_MAX_W。
static LYRIC_BAR_WIDTH: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(LYRIC_BAR_DEFAULT_W as u32);

/// 歌词条左键抬起的时间/位置（双击打开主界面用；Windows 双击判定 = 时间 + 距离）
static LAST_LYRIC_BAR_CLICK_MS: AtomicU64 = AtomicU64::new(0);
static LAST_LYRIC_BAR_CLICK_X: AtomicI32 = AtomicI32::new(0);
static LAST_LYRIC_BAR_CLICK_Y: AtomicI32 = AtomicI32::new(0);

/// 待定单击序号：单击动作延迟到双击判定窗口之后再执行，双击时递增使待定项作废
/// （否则双击的第一击会先弹出迷你播放器，第二击再收起——视觉上闪一下）。
static PENDING_SINGLE_CLICK_SEQ: AtomicU64 = AtomicU64::new(0);


/// 全局调试开关（main 启动时按配置设置，供非异步路径打印诊断）
fn debug_mode_enabled() -> bool {
    DEBUG_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct FrontConfig {
    pub server_port: u16,
    pub smtc_offset_ms: i64,
    pub update_interval_ms: u64,
}

#[tauri::command]
// Function to get current configuration asynchronously.
async fn get_app_config() -> Result<FrontConfig, String> {
    let cfg = CONFIG.get().ok_or("Config not initialized")?;
    let settings = {
        let guard = cfg.read().await;
        guard.settings.clone()
    };
    Ok(FrontConfig {
        server_port: settings.server_port,
        smtc_offset_ms: settings.smtc_offset_ms,
        update_interval_ms: settings.update_interval_ms,
    })
}

#[tauri::command]
// Function to save configuration asynchronously.
async fn save_app_config(new_cfg: FrontConfig) -> Result<(), String> {
    let cfg_lock = CONFIG.get().ok_or("Config not initialized")?;
    
    // 1. Update memory config
    {
        let mut guard = cfg_lock.write().await;
        guard.settings.server_port = new_cfg.server_port;
        guard.settings.smtc_offset_ms = new_cfg.smtc_offset_ms;
        guard.settings.update_interval_ms = new_cfg.update_interval_ms;
    }
    
    // 2. Write config.toml
    let toml_to_write = {
        let guard = cfg_lock.read().await;
        toml::to_string_pretty(&*guard)
            .map_err(|e| format!("Failed to serialize config: {}", e))?
    };
    
    tokio::fs::write("config.toml", toml_to_write)
        .await
        .map_err(|e| format!("Failed to write config.toml: {}", e))?;
        
    Ok(())
}

#[tauri::command]
async fn media_toggle_play_pause() -> Result<(), String> {
    smtc::try_toggle_play_pause().await.map_err(|e| format!("{}", e))
}

#[tauri::command]
async fn media_next() -> Result<(), String> {
    smtc::try_skip_next().await.map_err(|e| format!("{}", e))
}

#[tauri::command]
async fn media_previous() -> Result<(), String> {
    smtc::try_skip_previous().await.map_err(|e| format!("{}", e))
}

#[tauri::command]
// 前端将窗口隐藏到托盘前调用：标记后台模式，后端据此降频/停止 GUI 推送。
async fn window_hidden() {
    GUI_VISIBLE.store(false, Ordering::SeqCst);
}

/// 迷你播放器退场动画播完后的前端回执：真正隐藏窗口。
#[tauri::command]
async fn hide_mini_player(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("mini-player") {
        let _ = win.hide();
    }
}

/// 状态栏歌词窗口兜底高度（拿不到任务栏位置时使用；宽度取用户偏好）
const LYRIC_BAR_FALLBACK_HEIGHT: f64 = 48.0;
const LYRIC_BAR_BOTTOM_OFFSET: f64 = 0.0;

/// 歌词条窗口句柄（WinEvent 回调置顶用）
static LYRIC_BAR_HWND: AtomicIsize = AtomicIsize::new(0);

/// 迷你播放器逻辑尺寸（透明窗口，卡片外圈留白给 CSS 投影）。
/// 卡片本体 320×332 = 窗口减去 inset 留白（30/24/34）——留白必须覆盖
/// box-shadow 的最大外扩量（否则阴影被窗口边界裁切成方形暗晕），
/// 并容纳显隐动画 ±26px 的滑动行程。
const MINI_PLAYER_W: f64 = 368.0;
const MINI_PLAYER_H: f64 = 396.0;

/// 窗口内卡片本体的透明留白（与 mini-player.css 的 `inset: 30px 24px 34px`
/// 对齐：上 30 / 左右 24 / 下 34）。留白只为容纳投影与滑入动画——
/// 用户看到的是卡片本体，所以定位、边距、显示器钳制一律按卡片边缘算，
/// 不能按窗口边缘（否则卡片会离歌词条边界差一整个阴影留白）。
const MINI_PLAYER_INSET_TOP: f64 = 30.0;
const MINI_PLAYER_INSET_RIGHT: f64 = 24.0;
const MINI_PLAYER_INSET_BOTTOM: f64 = 34.0;
const MINI_PLAYER_INSET_LEFT: f64 = 24.0;

/// 迷你播放器最近一次显示时刻（Unix ms）：失焦自动隐藏的防抖基准。
/// 某些系统上 show+set_focus 会抖一次 Focused(false)，刚显示的一小段
/// 时间内的失焦事件直接忽略，防止窗口一闪而过。
static MINI_PLAYER_SHOWN_AT_MS: AtomicU64 = AtomicU64::new(0);

/// 迷你播放器是否放置在歌词条下方（任务栏在屏幕顶部时）。
/// 显隐动画的滑动方向由此决定：卡片永远从歌词条一侧滑入/滑出。
static MINI_PLAYER_PLACED_BELOW_BAR: AtomicBool = AtomicBool::new(false);

/// 状态栏歌词条右键菜单：低级鼠标钩子需要的 AppHandle
static HOOK_APP: OnceLock<tauri::AppHandle> = OnceLock::new();
/// 低级鼠标钩子句柄（保持安装直到进程退出，系统会回收）
#[allow(dead_code)]
static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);
/// 低级键盘钩子句柄：菜单打开时接管 Esc（见 lyric_bar_keyboard_hook）
#[allow(dead_code)]
static KEY_HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// 右键菜单是否正在显示。歌词条被 SetParent 到任务栏后不是前台窗口，
/// TrackPopupMenu 不会在点击菜单外部时自行关闭（微软文档记录的行为），
/// 因此菜单打开期间由低级鼠标钩子接管「菜单外点击 → 关闭菜单」。
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
/// 当前弹出菜单的窗口句柄（窗口类 #32768；钩子线程首次需要时枚举发现并缓存）
static MENU_HWND: AtomicIsize = AtomicIsize::new(0);
/// 菜单外按下已被吞掉：对应松开也要吞，避免半个点击落到底层窗口
static MENU_SWALLOW_UP: AtomicBool = AtomicBool::new(false);

/// 获取 Windows 任务栏矩形（物理像素）。
/// 通过 SHAppBarMessage(ABM_GETTASKBARPOS) 查询，任务栏不在屏幕底部时返回 None。
fn taskbar_rect() -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::UI::Shell::{ABM_GETTASKBARPOS, APPBARDATA, SHAppBarMessage};
    unsafe {
        let mut abd = APPBARDATA::default();
        abd.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
        if SHAppBarMessage(ABM_GETTASKBARPOS, &mut abd) == 0 {
            return None;
        }
        let rc = abd.rc;
        Some((rc.left, rc.top, rc.right, rc.bottom))
    }
}

/// 状态栏歌词窗口定位：嵌入任务栏右侧空白区（时钟左侧），与任务栏同高。
/// 返回 (x, y, width, height) 逻辑坐标；任务栏不可用时回退到屏幕底部悬浮。
fn lyric_bar_geometry(monitor: &tauri::Monitor) -> (f64, f64, f64, f64) {
    let scale = monitor.scale_factor();
    if let Some((l, t, r, b)) = taskbar_rect() {
        let tb_w = (r - l) as f64 / scale;
        let tb_h = (b - t) as f64 / scale;
        let x = l as f64 / scale + tb_w * 0.55;
        let y = t as f64 / scale;
        let w = tb_w * 0.4;
        let h = tb_h;
        return (x, y, w, h);
    }
    let logical = monitor.size().to_logical::<f64>(scale);
    let width = configured_lyric_bar_width();
    (
        (logical.width - width) / 2.0,
        (logical.height - LYRIC_BAR_FALLBACK_HEIGHT - LYRIC_BAR_BOTTOM_OFFSET).max(0.0),
        width,
        LYRIC_BAR_FALLBACK_HEIGHT,
    )
}

/// 歌词条逻辑尺寸（与前端 CSS 布局匹配）：默认宽度（设置项默认值）、高度、
/// 可调宽度范围。宽度偏好存于 LYRIC_BAR_WIDTH，定位时按可用空间封顶。
const LYRIC_BAR_DEFAULT_W: f64 = 520.0;
const LYRIC_BAR_H: f64 = 48.0;
const LYRIC_BAR_MIN_W: f64 = 180.0;
const LYRIC_BAR_MAX_W: u32 = 1600;

/// 当前歌词条宽度偏好（逻辑像素，clamp 到可调范围）
fn configured_lyric_bar_width() -> f64 {
    (LYRIC_BAR_WIDTH.load(Ordering::SeqCst) as f64).clamp(LYRIC_BAR_MIN_W, LYRIC_BAR_MAX_W as f64)
}

/// 找到主任务栏窗口（Shell_TrayWnd）
fn find_taskbar() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    unsafe {
        FindWindowW(windows::core::w!("Shell_TrayWnd"), None)
            .ok()
            .filter(|h| !h.0.is_null())
    }
}

/// 任务栏子窗口中已知的系统控件类名（枚举第三方插件时排除）
const KNOWN_TASKBAR_CHILD_CLASSES: &[&str] = &[
    "ReBarWindow32",
    "MSTaskSwWClass",
    "MSTaskListWClass",
    "TrayNotifyWnd",
    "TrayClockWClass",
    "Start",
    "Windows.UI.Composition.DesktopWindowContentBridge",
];

/// 检测嵌入任务栏的第三方插件窗口（如 TrafficMonitor 等），
/// 返回它们相对任务栏左上角的矩形（物理像素）。
/// 用于歌词条定位时避让，防止与其他插件挤在一起。
///
/// 过滤规则：
///  - 排除歌词条自身及其所有后代窗口（WebView2 会创建多个子窗口，
///    类名不在系统白名单内，之前被误判为插件导致自激振荡）
///  - 排除父链含 TrayNotifyWnd 的后代（通知区内部控件：托盘按钮/时钟/
///    隐藏图标弹窗宿主，歌词条右侧锚点本身贴着通知区左缘，永不进入）
///  - 排除高度远超任务栏的窗口（D3D 合成中间窗口等，其矩形可能是
///    陈旧/失真的巨大区域）
///
/// 注意：TrafficMonitor 的任务栏窗口是 `#32770` 对话框类（标题
/// TrafficMonitorTaskbarWindow），父窗口是它自建的宿主窗口而非
/// TrayNotifyWnd，因此会正常落入插件列表参与避让。
fn detect_taskbar_plugins(
    taskbar: windows::Win32::Foundation::HWND,
    bar_hwnd: windows::Win32::Foundation::HWND,
) -> Vec<windows::Win32::Foundation::RECT> {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, FindWindowExW, GetClassNameW, GetParent, GetWindowRect,
        IsWindowVisible,
    };
    use windows::core::BOOL;

    struct PluginList {
        tb_left: i32,
        tb_height: i32,
        bar_hwnd: windows::Win32::Foundation::HWND,
        tray_hwnd: windows::Win32::Foundation::HWND,
        rects: Vec<RECT>,
    }

    /// 沿父窗口链向上检查 hwnd 是否为 ancestor 的后代窗口。
    /// 注意不能用 GA_ROOT：嵌入任务栏后歌词条的顶层祖先是 Shell_TrayWnd 本身。
    unsafe fn is_descendant_of(
        hwnd: windows::Win32::Foundation::HWND,
        ancestor: windows::Win32::Foundation::HWND,
    ) -> bool {
        if ancestor.0.is_null() {
            return false;
        }
        let mut cur = GetParent(hwnd).ok();
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = GetParent(p).ok();
        }
        false
    }

    unsafe extern "system" fn enum_proc(
        hwnd: windows::Win32::Foundation::HWND,
        lparam: LPARAM,
    ) -> BOOL {
        let list = &mut *(lparam.0 as *mut PluginList);
        if IsWindowVisible(hwnd).as_bool() {
            // 排除歌词条自身及其后代（WebView2 会创建多个子窗口，
            // 类名不在系统白名单内，之前被误判为插件导致自激振荡）
            if hwnd == list.bar_hwnd || is_descendant_of(hwnd, list.bar_hwnd) {
                return BOOL(1);
            }
            // 排除通知区内部控件（父链含 TrayNotifyWnd）
            if is_descendant_of(hwnd, list.tray_hwnd) {
                return BOOL(1);
            }
            let mut buf = [0u16; 256];
            let len = GetClassNameW(hwnd, &mut buf);
            let name = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
            if !KNOWN_TASKBAR_CHILD_CLASSES.contains(&name.as_str()) {
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_ok() {
                    let w = rc.right - rc.left;
                    let h = rc.bottom - rc.top;
                    // 过滤过小窗口（图标级）与远高于任务栏的窗口（D3D 合成
                    // 中间窗口等），只关注占据任务栏条状区域的真实插件
                    if w >= 20 && h >= 10 && h <= list.tb_height * 2 {
                        rc.left -= list.tb_left;
                        rc.right -= list.tb_left;
                        list.rects.push(rc);
                    }
                }
            }
        }
        BOOL(1)
    }

    unsafe {
        let mut tb_rect = RECT::default();
        if GetWindowRect(taskbar, &mut tb_rect).is_err() {
            return Vec::new();
        }
        let tray_hwnd = FindWindowExW(Some(taskbar), None, windows::core::w!("TrayNotifyWnd"), None)
            .ok()
            .unwrap_or(windows::Win32::Foundation::HWND(std::ptr::null_mut()));
        let mut list = PluginList {
            tb_left: tb_rect.left,
            tb_height: tb_rect.bottom - tb_rect.top,
            bar_hwnd,
            tray_hwnd,
            rects: Vec::new(),
        };
        let _ = EnumChildWindows(
            Some(taskbar),
            Some(enum_proc),
            LPARAM(&mut list as *mut PluginList as isize),
        );
        list.rects
    }
}

/// 通知区（TrayNotifyWnd）的屏幕矩形；找不到时返回 None
fn tray_rect_of(taskbar: windows::Win32::Foundation::HWND) -> Option<windows::Win32::Foundation::RECT> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowRect};

    unsafe {
        let tray = FindWindowExW(Some(taskbar), None, windows::core::w!("TrayNotifyWnd"), None)
            .ok()
            .filter(|h| !h.0.is_null())?;
        let mut rc = RECT::default();
        GetWindowRect(tray, &mut rc).ok()?;
        Some(rc)
    }
}

/// 任务列表（MSTaskSwWClass）的屏幕矩形。
/// 该类窗口在 Win10/11 上都嵌套在 ReBarWindow32 之下，FindWindowExW 只搜
/// 直接子窗口找不到，需用 EnumChildWindows 递归查找。
fn tasklist_rect_of(
    taskbar: windows::Win32::Foundation::HWND,
) -> Option<windows::Win32::Foundation::RECT> {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW, GetWindowRect};
    use windows::core::BOOL;

    struct Ctx {
        class: String,
        found: Option<RECT>,
    }

    unsafe extern "system" fn enum_proc(
        hwnd: windows::Win32::Foundation::HWND,
        lparam: LPARAM,
    ) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        let mut buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf);
        let name = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
        if name == ctx.class {
            let mut rc = RECT::default();
            if GetWindowRect(hwnd, &mut rc).is_ok() {
                ctx.found = Some(rc);
                return BOOL(0); // stop enumeration
            }
        }
        BOOL(1)
    }

    unsafe {
        let mut ctx = Ctx {
            class: "MSTaskSwWClass".to_string(),
            found: None,
        };
        let _ = EnumChildWindows(
            Some(taskbar),
            Some(enum_proc),
            LPARAM(&mut ctx as *mut Ctx as isize),
        );
        ctx.found
    }
}

/// 任务栏「任务按钮」区域的并集矩形（屏幕物理像素）。
///
/// 不能用 MSTaskSwWClass / MSTaskListWClass 的窗口矩形：Win11 任务栏由
/// XAML（DesktopWindowContentBridge）渲染，这两个兼容窗口的矩形明显偏小
/// （实测右缘 1004，而按钮实际到 1269），直接采用会让歌词条盖住任务按钮。
/// 这里用 UI Automation 枚举 AutomationId 形如 `Appid:` 的按钮取并集。
fn taskbar_buttons_rect() -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, TreeScope_Descendants, UIA_ButtonControlTypeId,
        UIA_ControlTypePropertyId,
    };

    unsafe {
        // UIA 客户端需要 COM；线程可能已初始化（S_FALSE / 模式冲突均可忽略）
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL).ok()?;
        let taskbar = find_taskbar()?;
        let root = automation.ElementFromHandle(taskbar).ok()?;

        let button_type = VARIANT::from(UIA_ButtonControlTypeId.0);
        let condition = automation
            .CreatePropertyCondition(UIA_ControlTypePropertyId, &button_type)
            .ok()?;
        let items = root.FindAll(TreeScope_Descendants, &condition).ok()?;
        let count = items.Length().ok()?;

        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for i in 0..count {
            let Ok(item) = items.GetElement(i) else { continue };
            let Ok(id) = item.CurrentAutomationId() else { continue };
            // 只取任务按钮（固定/运行中的程序），开始按钮与托盘图标不参与
            if !id.to_string().starts_with("Appid:") {
                continue;
            }
            let Ok(rect) = item.CurrentBoundingRectangle() else { continue };
            if rect.right <= rect.left {
                continue;
            }
            bounds = Some(match bounds {
                None => (rect.left, rect.top, rect.right, rect.bottom),
                Some((l, t, r, b)) => (
                    l.min(rect.left),
                    t.min(rect.top),
                    r.max(rect.right),
                    b.max(rect.bottom),
                ),
            });
        }
        bounds
    }
}

/// 歌词条定位的布局快照：锚点、可用区间与第三方插件占用（UIA 枚举任务
/// 按钮、EnumChildWindows 找插件）都是昂贵查询，完整定位算一次缓存下来；
/// 拖动宽度滑块时只按快照重算 x/宽度并 SetWindowPos（完整定位约 55ms，
/// 每帧调用会拖慢拖动；快照路径约 3ms）。
#[derive(Clone)]
struct LyricBarLayout {
    tb_w: i32,
    tb_h: i32,
    bar_h: i32,
    gap: i32,
    min_w: i32,
    user_gap: i32,
    side_left: bool,
    zone_left: i32,
    zone_right: i32,
    right_anchor: i32,
    /// 第三方插件占用区间（相对任务栏左缘）：(left, right)
    plugins: Vec<(i32, i32)>,
}

impl LyricBarLayout {
    /// 按宽度偏好计算位置与宽度并应用（含插件避让与边界钳制）。
    fn apply(&self, bar_hwnd: windows::Win32::Foundation::HWND, requested_w: i32) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_SHOWWINDOW,
        };

        let available = (self.zone_right - self.zone_left - self.user_gap).max(self.min_w);
        let mut bar_w = requested_w.min(available).max(self.min_w);
        let mut x = if self.side_left {
            self.user_gap
        } else {
            self.zone_right - bar_w - self.user_gap
        };

        // —— 避让其他任务栏插件（TrafficMonitor 等嵌入窗口） ——
        // 推开后按剩余空间重算宽度：插件挤占会让可用长度变短，不能沿用原宽度
        for &(p_left, p_right) in &self.plugins {
            let p_w = p_right - p_left;
            if p_w < 20 {
                continue;
            }
            if p_left < x + bar_w && p_right > x {
                if self.side_left {
                    x = p_right + self.gap;
                    let limit = (self.zone_right - x - self.user_gap).max(self.min_w);
                    bar_w = bar_w.min(limit);
                } else {
                    let limit_right = (p_left - self.gap).max(self.zone_left);
                    let limit = (limit_right - self.zone_left - self.user_gap).max(self.min_w);
                    bar_w = bar_w.min(limit);
                    x = limit_right - bar_w - self.user_gap;
                }
            }
        }

        // 收进任务栏范围内，右侧不越过通知区
        x = x.max(0);
        if x + bar_w > self.tb_w - self.gap {
            bar_w = (self.tb_w - self.gap - x).max(self.min_w);
        }
        if !self.side_left && x + bar_w > self.right_anchor {
            bar_w = (self.right_anchor - x).max(self.min_w);
            x = self.right_anchor - bar_w;
        }

        let y = ((self.tb_h - self.bar_h) / 2).max(0);
        // HWND_TOP：保持在任务栏其他子窗口之上（后续新建的插件窗口可能抢占），
        // 配合 watchdog 每 5s 的布局自愈保证歌词条始终可见。
        unsafe {
            let _ = SetWindowPos(
                bar_hwnd,
                Some(HWND_TOP),
                x,
                y,
                bar_w,
                self.bar_h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        true
    }
}

/// 最近一次完整定位得到的布局快照（拖动宽度预览用）；窗口销毁时清空。
static LYRIC_BAR_LAYOUT: std::sync::Mutex<Option<LyricBarLayout>> =
    std::sync::Mutex::new(None);

/// 计算歌词条在任务栏内的位置并应用（支持左右偏好与插件避让）。
/// 完整定位会刷新布局快照（拖动预览用）；返回是否成功。
fn position_lyric_bar_in_taskbar(
    bar_hwnd: windows::Win32::Foundation::HWND,
    scale: f64,
) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowRect};

    unsafe {
        let Some(taskbar) = find_taskbar() else {
            return false;
        };
        let mut tb_rect = RECT::default();
        if GetWindowRect(taskbar, &mut tb_rect).is_err() {
            return false;
        }
        let tb_w = tb_rect.right - tb_rect.left;
        let tb_h = tb_rect.bottom - tb_rect.top;

        let min_w = (LYRIC_BAR_MIN_W * scale) as i32;
        // 高度贴合任务栏：小任务栏（如 40px）时收缩到任务栏高度，
        // 避免子窗口上下溢出被父窗口裁剪。前端 CSS 按窗口高度自适应隐藏翻译行。
        let bar_h = ((LYRIC_BAR_H * scale) as i32).min(tb_h);
        // 与第三方插件/任务栏边缘的间距（系统控件锚点使用贴边，见下）
        let gap = (20.0 * scale) as i32;

        let side_left = LYRIC_BAR_SIDE.load(Ordering::SeqCst) == 1;

        // —— 右锚：通知区左缘（贴着通知区显示；找不到时回退距任务栏右缘 gap） ——
        let mut right_anchor = tb_w - gap;
        if let Some(tray_rect) = tray_rect_of(taskbar) {
            right_anchor = tray_rect.left - tb_rect.left;
        }

        // —— 左锚：开始按钮右缘（贴着开始按钮显示） ——
        // Win11 的 Start 类窗口是隐藏占位窗口（实测右缘可达任务栏宽度的 40%），
        // 检测到异常值（>200 物理像素）时回退真实开始按钮的典型宽度（48 逻辑像素）
        let mut left_anchor = gap;
        if let Some(start) =
            FindWindowExW(Some(taskbar), None, windows::core::w!("Start"), None)
                .ok()
                .filter(|h| !h.0.is_null())
        {
            let mut start_rect = RECT::default();
            if GetWindowRect(start, &mut start_rect).is_ok() {
                let start_right = start_rect.right - tb_rect.left;
                left_anchor = if start_right <= (200.0 * scale) as i32 {
                    start_right
                } else {
                    (48.0 * scale) as i32
                };
            }
        }

        // —— 任务按钮区：优先 UIA 真实边界，回退 MSTaskSwWClass 窗口矩形 ——
        // （Win11 的兼容窗口矩形偏小，见 taskbar_buttons_rect 注释）
        let buttons = taskbar_buttons_rect()
            .map(|(l, _, r, _)| (l - tb_rect.left, r - tb_rect.left));
        let tasklist = tasklist_rect_of(taskbar).map(|rc| {
            (
                rc.left - tb_rect.left,
                rc.right - tb_rect.left,
            )
        });
        let pin_left = buttons.map(|(l, _)| l).or(tasklist.map(|(tl, _)| tl));
        let pin_right = buttons.map(|(_, r)| r).or(tasklist.map(|(_, tr)| tr));

        // Win10 左对齐任务栏：搜索框位于开始按钮与任务列表之间，把左锚
        // 移到任务列表左缘（搜索框右侧），避免盖住搜索框。居中任务栏的
        // 任务列表起点远离左缘（≥400 物理像素），不参与。
        if let Some(tl) = pin_left {
            if tl < (400.0 * scale) as i32 {
                left_anchor = left_anchor.max(tl);
            }
        }

        // —— 可用区间（相对任务栏左上角，物理像素） ——
        // 左侧：任务栏左缘 → 任务按钮区左缘（居中任务栏，避免盖住按钮）
        //       或任务栏右缘（左对齐任务栏）；
        // 右侧：max(左锚, 任务按钮区右缘 + gap) → 通知区左缘。
        let (zone_left, zone_right) = if side_left {
            let right = match pin_left {
                Some(tl) if tl >= (400.0 * scale) as i32 => (tl - gap).max(min_w),
                _ => tb_w - gap,
            };
            (0, right)
        } else {
            let left = match pin_right {
                Some(tr) => left_anchor.max(tr + gap),
                None => left_anchor,
            };
            (left, right_anchor)
        };

        // 用户可调边距（逻辑 px → 物理 px）：左侧 = 距任务栏左缘
        // （0 = 贴左缘，可覆盖开始按钮——歌词条鼠标穿透不影响点击）；
        // 右侧 = 距通知区左缘（0 = 贴通知区）
        let user_gap = (LYRIC_BAR_GAP.load(Ordering::SeqCst) as f64 * scale) as i32;

        // —— 第三方插件占用区间（随布局快照缓存，拖动预览不必重新枚举） ——
        let plugins: Vec<(i32, i32)> = detect_taskbar_plugins(taskbar, bar_hwnd)
            .into_iter()
            .map(|p| (p.left, p.right))
            .collect();

        // 宽度计算 / 插件避让 / SetWindowPos 统一交给布局快照应用：
        // 宽度 = min(用户偏好, 可用空间)，避免空间不足时盖住任务按钮
        let layout = LyricBarLayout {
            tb_w,
            tb_h,
            bar_h,
            gap,
            min_w,
            user_gap,
            side_left,
            zone_left,
            zone_right,
            right_anchor,
            plugins,
        };
        let requested_w = (configured_lyric_bar_width() * scale) as i32;
        let ok = layout.apply(bar_hwnd, requested_w);
        if ok {
            // 缓存给拖动宽度预览用：完整定位（含 UIA 枚举）约 55ms，
            // 预览路径只做 SetWindowPos，约 3ms
            if let Ok(mut cache) = LYRIC_BAR_LAYOUT.lock() {
                *cache = Some(layout);
            }
        }
        ok
    }
}

/// 把歌词条窗口嵌入任务栏（参照 TrafficMonitor 的做法）：
/// SetParent 使其成为任务栏的子窗口——子窗口永远渲染在任务栏内容
/// 之上，任务栏被点击/置顶都不会盖住它。返回是否成功嵌入。
///
/// 注意：windows-rs 0.62 的 SetParent 包装把「旧父为 NULL」（顶层窗口
/// 首次嵌入的合法成功结果）误判为 Err，因此不能依赖其返回值；
/// 跨进程子窗口（无 WS_CHILD 风格位）的 GetParent/IsChild 亦不可靠，
/// 统一用 GetAncestor(GA_PARENT) 验证嵌入结果。
fn embed_lyric_bar_into_taskbar(
    bar_hwnd: windows::Win32::Foundation::HWND,
    scale: f64,
) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, SetParent, GA_PARENT};

    unsafe {
        let Some(taskbar) = find_taskbar() else {
            return false;
        };
        // 成为任务栏的子窗口（忽略包装层返回值，见上方注释）
        let _ = SetParent(bar_hwnd, Some(taskbar));
        if GetAncestor(bar_hwnd, GA_PARENT) != taskbar {
            return false;
        }
        position_lyric_bar_in_taskbar(bar_hwnd, scale)
    }
}

/// 创建歌词条窗口并尝试嵌入任务栏；嵌入失败则回退为屏幕底部悬浮。
fn create_lyric_bar(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let monitor = app
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "未找到显示器".to_string())?;
    let scale = monitor.scale_factor();

    let win = WebviewWindowBuilder::new(
        app,
        "lyric-bar",
        WebviewUrl::App("lyric-bar.html".into()),
    )
    .title("QQMusic Monitor - 状态栏歌词")
    .inner_size(configured_lyric_bar_width(), LYRIC_BAR_H)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .shadow(false)
    .visible(false)
    .build()
    .map_err(|e| e.to_string())?;

    let _ = win.set_ignore_cursor_events(true);
    // tauri 返回的 HWND 来自 windows 0.61，转为项目使用的 0.62 指针类型
    let hwnd_raw = win.hwnd().map_err(|e| e.to_string())?;
    let hwnd = windows::Win32::Foundation::HWND(hwnd_raw.0);

    let embedded = embed_lyric_bar_into_taskbar(hwnd, scale);
    if !embedded {
        let (x, y, w, h) = lyric_bar_geometry(&monitor);
        let _ = win.set_position(tauri::Position::Logical((x, y).into()));
        let _ = win.set_size(tauri::Size::Logical((w, h).into()));
    }

    // 句柄必须等定位完成后才登记：提前登记时并发的 set_lyric_bar_side /
    // set_lyric_bar_gap 会在窗口尚未 SetParent 的瞬间按「顶层窗口」坐标系
    // 重定位一次（数值是任务栏客户区坐标），窗口当场落到错误的屏幕位置。
    LYRIC_BAR_HWND.store(hwnd_raw.0 as isize, Ordering::SeqCst);

    let _ = win.show();
    let _ = win.set_ignore_cursor_events(true);
    Ok(())
}

/// 当前主显示器缩放比例（查询失败时按 1.0 处理）
fn current_monitor_scale(app: &tauri::AppHandle) -> f64 {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0)
}

/* ============================================================
 * 迷你播放器（左键点击状态栏歌词弹出）
 *
 * flyout 语义：左键歌词条切换显隐；窗口失焦自动隐藏（点击窗口外
 * 任意位置即收起）。钩子吞掉歌词条上的左键 DOWN/UP，所以切换那次
 * 点击不会引发焦点变化，与 Focused(false) 自动隐藏无竞态。
 * 窗口懒创建一次后复用（hide/show），避免反复加载 WebView。
 * ============================================================ */

/// 创建迷你播放器窗口（懒加载，仅首次左键时调用）。
fn create_mini_player_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    let win = WebviewWindowBuilder::new(
        app,
        "mini-player",
        WebviewUrl::App("mini-player.html".into()),
    )
    .title("QQMusic Monitor - 迷你播放器")
    .inner_size(MINI_PLAYER_W, MINI_PLAYER_H)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .maximizable(false)
    .minimizable(false)
    .shadow(false)
    .visible(false)
    .build()
    .map_err(|e| e.to_string())?;

    // 失焦自动隐藏（flyout 行为）：点击窗口外任意位置即收起。
    // 250ms 防抖：部分系统上 show+set_focus 会先抖一次 Focused(false)。
    let handle = app.clone();
    win.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(false) = event {
            let shown = MINI_PLAYER_SHOWN_AT_MS.load(Ordering::SeqCst);
            if current_timestamp_ms().saturating_sub(shown) < 250 {
                return;
            }
            request_hide_mini_player(&handle);
        }
    });
    Ok(win)
}

/// 把迷你播放器定位到歌词条上方（水平对其中线），钳制在显示器工作区内。
/// 歌词条句柄不可用时回退锚定任务栏矩形；任务栏在屏幕顶部时改放歌词条下方。
fn position_mini_player(app: &tauri::AppHandle, win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let scale = current_monitor_scale(app);

    // 锚点矩形（逻辑 px）：优先歌词条屏幕矩形，回退任务栏矩形
    let mut anchor: Option<(f64, f64, f64, f64)> = None;
    let raw = LYRIC_BAR_HWND.load(Ordering::SeqCst);
    if raw != 0 {
        let mut rc = RECT::default();
        unsafe {
            if GetWindowRect(HWND(raw as *mut core::ffi::c_void), &mut rc).is_ok() {
                anchor = Some((
                    rc.left as f64 / scale,
                    rc.top as f64 / scale,
                    rc.right as f64 / scale,
                    rc.bottom as f64 / scale,
                ));
            }
        }
    }
    if anchor.is_none() {
        anchor = taskbar_rect().map(|(l, t, r, b)| {
            (l as f64 / scale, t as f64 / scale, r as f64 / scale, b as f64 / scale)
        });
    }
    let Some((al, at, ar, ab)) = anchor else { return; };

    // —— 按卡片本体（不含阴影留白）对齐歌词条边界 ——
    // 窗口外的透明留白只为投影和滑入动画服务，用户看到的是卡片本身，
    // 因此对齐、间距、显示器钳制都按卡片边缘计算。
    let card_w = MINI_PLAYER_W - MINI_PLAYER_INSET_LEFT - MINI_PLAYER_INSET_RIGHT;
    let card_h = MINI_PLAYER_H - MINI_PLAYER_INSET_TOP - MINI_PLAYER_INSET_BOTTOM;
    // 卡片底缘距歌词条顶缘 8px（窗口坐标 = 卡片坐标 - 上留白）
    let mut x = (al + ar) / 2.0 - card_w / 2.0;
    let mut y = at - 8.0 - card_h - MINI_PLAYER_INSET_TOP;

    if let Ok(Some(mon)) = app.primary_monitor() {
        let mpos = mon.position().to_logical::<f64>(scale);
        let msize = mon.size().to_logical::<f64>(scale);
        // 水平：与歌词条更靠近屏幕边界的一边对齐（用户 2026-10 要求）——
        // 左侧布局让卡片左缘贴歌词条左缘，右侧布局让卡片右缘贴歌词条右缘，
        // 不再取中线居中；歌词条比卡片宽/窄时都按边缘齐平。
        let dist_left = al - mpos.x;
        let dist_right = (mpos.x + msize.width) - ar;
        x = if dist_left <= dist_right {
            al - MINI_PLAYER_INSET_LEFT
        } else {
            ar - MINI_PLAYER_INSET_LEFT - card_w
        };
        // 任务栏在顶部（锚点上方放不下）→ 改放锚点下方，卡片顶缘距锚点 8px
        if y < mpos.y + 8.0 {
            y = ab + 8.0 - MINI_PLAYER_INSET_TOP;
        }
        // 钳制在显示器内：按卡片本体边缘留 8px 边距，窗口留白不计入
        let min_x = mpos.x + 8.0 - MINI_PLAYER_INSET_LEFT;
        let max_x = (mpos.x + msize.width - 8.0 - MINI_PLAYER_INSET_LEFT - card_w).max(min_x);
        x = x.clamp(min_x, max_x);
        let min_y = mpos.y + 8.0 - MINI_PLAYER_INSET_TOP;
        let max_y = (mpos.y + msize.height - 8.0 - MINI_PLAYER_INSET_TOP - card_h).max(min_y);
        y = y.clamp(min_y, max_y);
    }
    // 记录放置方位（供显隐动画选方向）：y 越过锚点顶缘 = 放在歌词条下方
    MINI_PLAYER_PLACED_BELOW_BAR.store(y > at, Ordering::SeqCst);
    let _ = win.set_position(tauri::Position::Logical((x, y).into()));
}

/// 显示并聚焦迷你播放器（记录显示时刻供失焦防抖），并通知前端重播滑入动画。
fn show_mini_player(win: &tauri::WebviewWindow) {
    use tauri::Emitter;
    MINI_PLAYER_SHOWN_AT_MS.store(current_timestamp_ms(), Ordering::SeqCst);
    let _ = win.show();
    let _ = win.set_focus();
    // from_below：卡片在歌词条上方（常规底部任务栏）→ 从下方滑入；反之从上方
    let from_below = !MINI_PLAYER_PLACED_BELOW_BAR.load(Ordering::SeqCst);
    let _ = win.emit("mini-player-show", serde_json::json!({ "from_below": from_below }));
}

/// 请求隐藏迷你播放器：先发事件让前端播滑出动画（播完回执 hide_mini_player），
/// 500ms 兜底强制收起——页面异常时窗口不应残留；期间被重新聚焦则放弃兜底。
fn request_hide_mini_player(app: &tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    let Some(win) = app.get_webview_window("mini-player") else { return; };
    if !win.is_visible().unwrap_or(false) { return; }
    let _ = win.emit("mini-player-hide-request", ());
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Some(w) = handle.get_webview_window("mini-player") {
            if w.is_visible().unwrap_or(false) && !w.is_focused().unwrap_or(false) {
                let _ = w.hide();
            }
        }
    });
}

/// 左键点击歌词条：切换迷你播放器显隐（主线程执行）。
fn toggle_mini_player(app: &tauri::AppHandle) {
    use tauri::Manager;

    if let Some(win) = app.get_webview_window("mini-player") {
        if win.is_visible().unwrap_or(false) {
            request_hide_mini_player(app);
            return;
        }
        position_mini_player(app, &win);
        show_mini_player(&win);
        return;
    }
    if let Ok(win) = create_mini_player_window(app) {
        position_mini_player(app, &win);
        show_mini_player(&win);
    } else if debug_mode_enabled() {
        eprintln!("[mini-player] 创建迷你播放器窗口失败");
    }
}

/// 双击歌词条：收起可能刚被单击弹出的迷你播放器，恢复主界面。
fn open_main_from_lyric_bar(app: &tauri::AppHandle) {
    request_hide_mini_player(app);
    show_main_window(app);
}

/// 销毁迷你播放器（歌词条关闭/隐藏时连带收起——没有歌词条就没有入口）。
fn destroy_mini_player(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("mini-player") {
        let _ = win.destroy();
    }
}

/// 歌词条自愈守护：
/// - explorer 重启会销毁其子窗口（歌词条窗口被一并销毁），检测到句柄
///   失效后重建窗口并重新嵌入
/// - 父窗口句柄变化（任务栏重建）时重新 SetParent 嵌入
/// - 任务栏矩形或缩放比例变化（托盘图标增减/分辨率/DPI/任务栏移动）时
///   重新定位，保证歌词条始终避让通知区与其他插件
fn spawn_lyric_bar_watchdog(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut last_tb_rect: Option<(i32, i32, i32, i32)> = None;
        let mut last_tray_rect: Option<(i32, i32, i32, i32)> = None;
        let mut last_tasklist_rect: Option<(i32, i32, i32, i32)> = None;
        let mut last_buttons_rect: Option<(i32, i32, i32, i32)> = None;
        let mut last_plugins: Option<Vec<(i32, i32, i32, i32)>> = None;
        let mut last_scale: f64 = 0.0;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if !LYRIC_BAR_ENABLED.load(Ordering::SeqCst) {
                last_tb_rect = None;
                continue;
            }
            let raw = LYRIC_BAR_HWND.load(Ordering::SeqCst);
            if raw == 0 {
                continue;
            }
            unsafe {
                use windows::Win32::Foundation::{HWND, RECT};
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetAncestor, GetWindowRect, IsWindow, GA_PARENT,
                };
                use tauri::Manager;
                let bar = HWND(raw as *mut core::ffi::c_void);
                if IsWindow(Some(bar)).as_bool() {
                    if let Some(taskbar) = find_taskbar() {
                        // GetParent/IsChild 对跨进程子窗口（无 WS_CHILD 风格位）
                        // 不可靠，用 GetAncestor(GA_PARENT) 判断嵌入状态
                        let embedded = GetAncestor(bar, GA_PARENT) == taskbar;
                        if !embedded {
                            // 任务栏重建（explorer 重启）或嵌入丢失：重新嵌入
                            let scale = current_monitor_scale(&app);
                            embed_lyric_bar_into_taskbar(bar, scale);
                        }
                        // 布局变化检测：任务栏/通知区/任务列表矩形、第三方插件
                        // （TrafficMonitor 等，用户拖动后自动避让）或缩放比例变化
                        // 时重新定位
                        let mut rc = RECT::default();
                        let tb_now = GetWindowRect(taskbar, &mut rc)
                            .ok()
                            .map(|_| (rc.left, rc.top, rc.right, rc.bottom));
                        let tray_now = tray_rect_of(taskbar)
                            .map(|r| (r.left, r.top, r.right, r.bottom));
                        let tasklist_now = tasklist_rect_of(taskbar)
                            .map(|r| (r.left, r.top, r.right, r.bottom));
                        // 任务按钮区（UIA 真实边界）：按钮增减/移动后要重定位，
                        // 否则歌词条会盖住新出现的按钮
                        let buttons_now = taskbar_buttons_rect();
                        let mut plugins_now = detect_taskbar_plugins(taskbar, bar)
                            .iter()
                            .map(|r| (r.left, r.top, r.right, r.bottom))
                            .collect::<Vec<_>>();
                        // 排序后比较，避免枚举顺序抖动造成误判
                        plugins_now.sort_unstable();
                        let scale = current_monitor_scale(&app);
                        let layout_changed = tb_now != last_tb_rect
                            || tray_now != last_tray_rect
                            || tasklist_now != last_tasklist_rect
                            || buttons_now != last_buttons_rect
                            || Some(plugins_now.clone()) != last_plugins
                            || (scale - last_scale).abs() > f64::EPSILON;
                        if layout_changed {
                            position_lyric_bar_in_taskbar(bar, scale);
                            last_tb_rect = tb_now;
                            last_tray_rect = tray_now;
                            last_tasklist_rect = tasklist_now;
                            last_buttons_rect = buttons_now;
                            last_plugins = Some(plugins_now);
                            last_scale = scale;
                        }
                    }
                } else {
                    // 窗口已被外部销毁：重建（布局快照同时失效）
                    if let Some(win) = app.get_webview_window("lyric-bar") {
                        let _ = win.destroy();
                    }
                    LYRIC_BAR_HWND.store(0, Ordering::SeqCst);
                    if let Ok(mut cache) = LYRIC_BAR_LAYOUT.lock() {
                        *cache = None;
                    }
                    let _ = create_lyric_bar(&app);
                    last_tb_rect = None;
                }
            }
        }
    });
}

/* ============================================================
 * 状态栏歌词条右键菜单（低级鼠标钩子）
 *
 * 歌词条窗口是鼠标穿透的（ignore_cursor_events），无法直接接收右键。
 * 用 WH_MOUSE_LL 全局低级鼠标钩子：右键落在歌词条矩形内时吞掉事件
 * （避免任务栏弹出自己的右键菜单）并弹原生菜单：
 *   - 隐藏状态栏歌词（写回前端偏好，下次启动不再创建）
 *   - 退出 QQMusic Monitor
 *
 * 收起菜单：歌词条被 SetParent 到任务栏后不是前台窗口，TrackPopupMenu
 * 不会在点击菜单外部时自行关闭（微软文档记录的已知行为）。因此菜单
 * 打开期间由同一个低级鼠标钩子接管：菜单外的任何点击都先关闭菜单，
 * 并吞掉这次点击（与系统菜单一致，关闭菜单的点击不落到下层窗口）。
 * ============================================================ */

/// 安装状态栏歌词条右键菜单钩子（独立线程 + 消息循环）：
/// 鼠标钩子负责「右键命中歌词条 → 弹菜单」与「菜单外点击 → 关闭菜单」，
/// 键盘钩子负责菜单打开时的 Esc（歌词条不是前台窗口，键盘事件不会路由给它）。
fn spawn_lyric_bar_menu_hooks(app: tauri::AppHandle) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, MSG, WH_KEYBOARD_LL,
        WH_MOUSE_LL,
    };

    let _ = HOOK_APP.set(app);
    std::thread::spawn(move || unsafe {
        match SetWindowsHookExW(WH_MOUSE_LL, Some(lyric_bar_mouse_hook), None, 0) {
            Ok(hook) => {
                HOOK_HANDLE.store(hook.0 as isize, Ordering::SeqCst);
            }
            Err(err) => eprintln!("[hook] 状态栏歌词右键菜单鼠标钩子安装失败: {:?}", err),
        }
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(lyric_bar_keyboard_hook), None, 0) {
            Ok(hook) => {
                KEY_HOOK_HANDLE.store(hook.0 as isize, Ordering::SeqCst);
            }
            Err(err) => eprintln!("[hook] 状态栏歌词右键菜单键盘钩子安装失败: {:?}", err),
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    });
}

/// 枚举回调：找到本进程的弹出菜单窗口（窗口类 #32768）。
unsafe extern "system" fn find_menu_window_proc(
    hwnd: windows::Win32::Foundation::HWND,
    _l_param: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowThreadProcessId};

    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
    if pid == std::process::id() {
        let mut buf = [0u16; 16];
        let len = GetClassNameW(hwnd, &mut buf);
        if len > 0 && String::from_utf16_lossy(&buf[..len as usize]) == "#32768" {
            MENU_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        }
    }
    true.into()
}

/// 当前正在弹出的菜单窗口（首次需要时枚举发现并缓存）。
unsafe fn active_menu_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

    let raw = MENU_HWND.load(Ordering::SeqCst);
    if raw != 0 {
        return Some(HWND(raw as *mut core::ffi::c_void));
    }
    let _ = EnumWindows(Some(find_menu_window_proc), LPARAM(0));
    let raw = MENU_HWND.load(Ordering::SeqCst);
    if raw != 0 {
        Some(HWND(raw as *mut core::ffi::c_void))
    } else {
        None
    }
}

/// 光标是否落在当前弹出的菜单内（还没定位到菜单窗口时按「在菜单内」处理，
/// 即不拦截——宁可这次不关闭，也不要吞掉用户正常点击）。
unsafe fn cursor_over_active_menu() -> bool {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};

    let Some(menu) = active_menu_hwnd() else {
        return true;
    };
    let mut rc = RECT::default();
    let mut pt = POINT::default();
    GetWindowRect(menu, &mut rc).is_ok()
        && GetCursorPos(&mut pt).is_ok()
        && pt.x >= rc.left
        && pt.x <= rc.right
        && pt.y >= rc.top
        && pt.y <= rc.bottom
}

/// 用 Esc 关掉当前菜单：菜单窗口是 TrackPopupMenu 的内部窗口（类 #32768），
/// 直接投递键盘消息即可让它按「取消」退出（实测可靠）。
unsafe fn close_active_menu() {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP};

    if let Some(menu) = active_menu_hwnd() {
        let _ = PostMessageW(Some(menu), WM_KEYDOWN, WPARAM(0x1B), LPARAM(0));
        let _ = PostMessageW(Some(menu), WM_KEYUP, WPARAM(0x1B), LPARAM(0));
    }
}

/// 菜单打开时的键盘钩子：只接管 Esc。
/// 歌词条不是前台窗口，键盘输入会送到前台应用而不是菜单；不拦截的话
/// 用户按 Esc 既关不掉菜单，还会误触发前台应用的 Esc。
unsafe extern "system" fn lyric_bar_keyboard_hook(
    n_code: i32,
    w_param: windows::Win32::Foundation::WPARAM,
    l_param: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, KBDLLHOOKSTRUCT, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    if n_code == HC_ACTION as i32 && MENU_OPEN.load(Ordering::SeqCst) {
        let msg = w_param.0 as u32;
        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN || msg == WM_KEYUP || msg == WM_SYSKEYUP {
            let kb = l_param.0 as *const KBDLLHOOKSTRUCT;
            if !kb.is_null() && (*kb).vkCode == 0x1B {
                if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                    close_active_menu();
                }
                return windows::Win32::Foundation::LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, n_code, w_param, l_param)
}

/// 光标当前是否落在歌词条窗口矩形内（屏幕物理像素判定）。
unsafe fn cursor_in_lyric_bar() -> bool {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};

    let raw = LYRIC_BAR_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return false;
    }
    let hwnd = HWND(raw as *mut core::ffi::c_void);
    let mut rc = RECT::default();
    let mut pt = POINT::default();
    GetWindowRect(hwnd, &mut rc).is_ok()
        && GetCursorPos(&mut pt).is_ok()
        && pt.x >= rc.left
        && pt.x <= rc.right
        && pt.y >= rc.top
        && pt.y <= rc.bottom
}

/// 系统双击时间（毫秒，下限 200ms）：双击容差判定用。
fn lyric_bar_double_click_ms() -> u64 {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
    unsafe { GetDoubleClickTime().max(200) as u64 }
}

/// 单击动作延迟：只需覆盖「第一击抬起 → 第二击按下」的间隔，不必等满
/// 系统双击时间（默认 500ms，单击反馈太拖沓）。用户实测 250ms 仍偏慢，
/// 压到 150ms；慢速双击（>150ms）由第二击按下作废待定动作兜底。
fn lyric_bar_single_click_delay_ms() -> u64 {
    lyric_bar_double_click_ms().min(150)
}

/// 本次左键按下是否可能构成双击的第二击（距上次抬起在系统双击时间内且
/// 位置接近）。第二击一按下就作废待定单击——等不到抬起，迷你播放器已经
/// 延迟执行了。
fn lyric_bar_press_starts_double_click() -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let last_ms = LAST_LYRIC_BAR_CLICK_MS.load(Ordering::SeqCst);
    if last_ms == 0 {
        return false;
    }
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    current_timestamp_ms().saturating_sub(last_ms) <= lyric_bar_double_click_ms()
        && (pt.x - LAST_LYRIC_BAR_CLICK_X.load(Ordering::SeqCst)).abs() <= 4
        && (pt.y - LAST_LYRIC_BAR_CLICK_Y.load(Ordering::SeqCst)).abs() <= 4
}

/// 判断本次左键抬起是否构成歌词条上的双击（系统双击时间 + 光标位置容差），
/// 并把本次抬起记录为下一次判定的基准。
fn is_lyric_bar_double_click() -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let now = current_timestamp_ms();
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let double_click_ms = lyric_bar_double_click_ms();
    let last_ms = LAST_LYRIC_BAR_CLICK_MS.load(Ordering::SeqCst);
    let is_double = last_ms != 0
        && now.saturating_sub(last_ms) <= double_click_ms
        && (pt.x - LAST_LYRIC_BAR_CLICK_X.load(Ordering::SeqCst)).abs() <= 4
        && (pt.y - LAST_LYRIC_BAR_CLICK_Y.load(Ordering::SeqCst)).abs() <= 4;

    LAST_LYRIC_BAR_CLICK_MS.store(now, Ordering::SeqCst);
    LAST_LYRIC_BAR_CLICK_X.store(pt.x, Ordering::SeqCst);
    LAST_LYRIC_BAR_CLICK_Y.store(pt.y, Ordering::SeqCst);
    is_double
}

/// 低级鼠标钩子回调：
/// 1. 菜单打开期间，菜单外的任何鼠标按键 → 关闭菜单并吞掉这次点击
///    （等价于系统菜单「点击外部即关闭」的行为；歌词条不是前台窗口，
///    TrackPopupMenu 自己不会关）；
/// 2. 其余情况：左键命中歌词条 → 吞掉并切换迷你播放器（弹窗 flyout）；
///    右键命中歌词条 → 吞掉并弹菜单。
unsafe extern "system" fn lyric_bar_mouse_hook(
    n_code: i32,
    w_param: windows::Win32::Foundation::WPARAM,
    l_param: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HC_ACTION, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
        WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    };

    if n_code == HC_ACTION as i32 {
        let msg = w_param.0 as u32;
        let is_down = msg == WM_LBUTTONDOWN || msg == WM_RBUTTONDOWN;
        let is_up = msg == WM_LBUTTONUP || msg == WM_RBUTTONUP;

        // 菜单打开期间滚轮/侧键不属于菜单操作：一并吞掉，避免滚动或
        // 前进/后退落到下层窗口（系统菜单同样不会把它们传给别的窗口）
        if MENU_OPEN.load(Ordering::SeqCst)
            && (msg == WM_MOUSEWHEEL
                || msg == WM_MOUSEHWHEEL
                || msg == WM_XBUTTONDOWN
                || msg == WM_XBUTTONUP)
        {
            return LRESULT(1);
        }

        // —— 菜单打开期间：接管菜单外的点击 ——
        if MENU_OPEN.load(Ordering::SeqCst) && (is_down || is_up) {
            if cursor_over_active_menu() {
                // 点在菜单内：交给菜单自己处理，同时保证不会误弹新菜单
                return CallNextHookEx(None, n_code, w_param, l_param);
            }
            if is_down {
                MENU_SWALLOW_UP.store(true, Ordering::SeqCst);
                close_active_menu();
            } else {
                MENU_SWALLOW_UP.store(false, Ordering::SeqCst);
            }
            // 吞掉点击：与系统菜单一致，关闭菜单的那次点击不落到下层窗口
            return LRESULT(1);
        }
        // 菜单已经关掉、但松开来迟一步：仍然吞掉，保证这次点击整体不生效
        if is_up && MENU_SWALLOW_UP.swap(false, Ordering::SeqCst) {
            return LRESULT(1);
        }

        // —— 左键命中歌词条 → 吞掉：单击切换迷你播放器，双击打开主界面 ——
        // 歌词条是鼠标穿透窗口，原本左键会穿透到任务栏；弹出交互接管后
        // DOWN/UP 一并吞掉，避免任务栏抢前台导致弹窗被 Focused(false)
        // 立刻收起（toggling 的那次点击不会引发焦点变化）。
        if msg == WM_LBUTTONDOWN || msg == WM_LBUTTONUP {
            if cursor_in_lyric_bar() {
                if msg == WM_LBUTTONUP {
                    if let Some(app) = HOOK_APP.get() {
                        if is_lyric_bar_double_click() {
                            // 双击：作废待定单击，直接恢复主界面（不弹迷你播放器）
                            PENDING_SINGLE_CLICK_SEQ.fetch_add(1, Ordering::SeqCst);
                            let handle = app.clone();
                            let _ = app.run_on_main_thread(move || open_main_from_lyric_bar(&handle));
                        } else {
                            // 单击：延迟一小段（≤250ms）再切换迷你播放器，给双击第二击
                            // 留出判定时间；第二击按下时会立即作废这个待定动作
                            let seq = PENDING_SINGLE_CLICK_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
                            let app_handle = app.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(std::time::Duration::from_millis(
                                    lyric_bar_single_click_delay_ms(),
                                ));
                                if PENDING_SINGLE_CLICK_SEQ.load(Ordering::SeqCst) == seq {
                                    let task_handle = app_handle.clone();
                                    let _ = app_handle.run_on_main_thread(move || {
                                        toggle_mini_player(&task_handle)
                                    });
                                }
                            });
                        }
                    }
                } else if lyric_bar_press_starts_double_click() {
                    // 双击的第二击一按下就作废待定单击（不必等抬起），
                    // 单击延迟窗口内不会误弹迷你播放器
                    PENDING_SINGLE_CLICK_SEQ.fetch_add(1, Ordering::SeqCst);
                }
                return LRESULT(1);
            }
        }

        // —— 右键命中歌词条 → 吞掉并弹菜单 ——
        if msg == WM_RBUTTONDOWN || msg == WM_RBUTTONUP {
            if cursor_in_lyric_bar() {
                if msg == WM_RBUTTONUP {
                    if let Some(app) = HOOK_APP.get() {
                        let handle = app.clone();
                        let _ = app.run_on_main_thread(move || show_lyric_bar_menu(&handle));
                    }
                }
                // 吞掉右键（DOWN/UP），任务栏不会弹出自己的菜单
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, n_code, w_param, l_param)
}

/// 在歌词条位置弹出原生右键菜单（主线程执行）。
///
/// 菜单打开期间置 MENU_OPEN，由低级鼠标钩子负责「点击菜单外部 → 关闭菜单」，
/// 见 lyric_bar_mouse_hook。
fn show_lyric_bar_menu(app: &tauri::AppHandle) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, TrackPopupMenu, MF_SEPARATOR,
        MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
    };

    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return; };
        let _ = AppendMenuW(menu, MF_STRING, 1, windows::core::w!("打开主页"));
        let _ = AppendMenuW(menu, MF_STRING, 2, windows::core::w!("打开设置"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 3, windows::core::w!("隐藏状态栏歌词"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, windows::core::PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, 4, windows::core::w!("退出 QQMusic Monitor"));
        let mut pt = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut pt);
        let raw = LYRIC_BAR_HWND.load(Ordering::SeqCst);
        if raw == 0 {
            let _ = DestroyMenu(menu);
            return;
        }
        let owner = HWND(raw as *mut core::ffi::c_void);
        // 钩子线程据此接管菜单外点击；菜单窗口句柄由钩子按需枚举发现
        MENU_HWND.store(0, Ordering::SeqCst);
        MENU_SWALLOW_UP.store(false, Ordering::SeqCst);
        MENU_OPEN.store(true, Ordering::SeqCst);
        let ret = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, None, owner, None);
        MENU_OPEN.store(false, Ordering::SeqCst);
        MENU_HWND.store(0, Ordering::SeqCst);
        let _ = DestroyMenu(menu);
        // TPM_RETURNCMD：返回值即菜单项 id（0 = 取消）
        let cmd = ret.0 as u32;
        match cmd {
            1 => show_main_window(app),
            2 => open_settings_window(app),
            3 => hide_lyric_bar_from_menu(app),
            4 => app.exit(0),
            _ => {}
        }
    }
}

/// 右键菜单「打开设置」：恢复主界面，并通知前端打开设置抽屉。
fn open_settings_window(app: &tauri::AppHandle) {
    use tauri::{Emitter, Manager};

    show_main_window(app);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.emit("open-settings", ());
    }
}

/// 右键菜单"隐藏状态栏歌词"：销毁歌词条并持久化偏好（下次启动不再创建）。
fn hide_lyric_bar_from_menu(app: &tauri::AppHandle) {
    use tauri::Manager;

    LYRIC_BAR_ENABLED.store(false, Ordering::SeqCst);
    if let Some(win) = app.get_webview_window("lyric-bar") {
        let _ = win.destroy();
    }
    LYRIC_BAR_HWND.store(0, Ordering::SeqCst);
    // 窗口销毁后布局快照立即失效（下次预览回退完整定位重建快照）
    if let Ok(mut cache) = LYRIC_BAR_LAYOUT.lock() {
        *cache = None;
    }
    destroy_mini_player(app);

    let path = frontend_prefs_path();
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(obj) = value.as_object_mut() {
                obj.insert("lyricBar".to_string(), serde_json::Value::Bool(false));
                if let Ok(text) = serde_json::to_string(&value) {
                    let _ = std::fs::write(&path, text);
                }
            }
        }
    }
}

#[tauri::command]
// 查询任务栏是否为浅色主题，供歌词条选择文字配色。
//
// 任务栏明暗跟随「Windows 模式」，权威来源是注册表
// HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\
//   SystemUsesLightTheme（1=浅色，0=深色）。
// 不能用 DWMWA_USE_IMMERSIVE_DARK_MODE 判断——它是应用窗口级属性，
// 对 Shell_TrayWnd 实测恒返回 0（即使系统为深色模式），会把深色
// 任务栏误报为浅色：歌词条自动配色按「浅色」出深字，表现为
// 「自动模式没效果」。注册表读取失败时才回退 DWM 属性。
fn get_taskbar_light_theme() -> bool {
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, RegGetValueW, RRF_RT_REG_DWORD,
    };

    unsafe {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            windows::core::w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        );
        if status.is_ok() {
            return value != 0;
        }
    }

    // 回退：DWM 属性（注册表不可用时的旧行为；默认按深色处理）
    use windows::core::BOOL;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};

    unsafe {
        let Some(taskbar) = find_taskbar() else {
            return false;
        };
        let mut dark_mode = BOOL(0);
        if DwmGetWindowAttribute(
            taskbar,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &mut dark_mode as *mut BOOL as *mut core::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        )
        .is_ok()
        {
            return !dark_mode.as_bool();
        }
    }
    false
}

/// 歌词条已存在时按当前偏好立即重新定位（DPI 取主显示器）。
/// 三个位置偏好命令（side / gap / width）共用；窗口尚未创建时为空操作。
/// 通知歌词条点亮左右边界竖线（前端约 3s 后自动淡出）。
fn flash_lyric_bar_bounds(app: &tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    if let Some(win) = app.get_webview_window("lyric-bar") {
        let _ = win.emit("lyric-bar-bounds", ());
    }
}

fn relocate_lyric_bar(app: &tauri::AppHandle) {
    use windows::Win32::Foundation::HWND;

    let raw = LYRIC_BAR_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return;
    }
    let bar = HWND(raw as *mut core::ffi::c_void);
    let scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);
    if position_lyric_bar_in_taskbar(bar, scale) {
        // 调整后让歌词条立即点亮左右边界竖线（前端约 3s 后淡出），
        // 用户拖宽度/切位置/改边距时能直接看到窗口的实际边界。
        flash_lyric_bar_bounds(app);
    }
}

#[tauri::command]
// 状态栏歌词位置偏好：'left' 任务栏左侧（开始按钮右侧），'right' 通知区左侧。
// 歌词条存在时立即重新定位（含插件避让检测）。
async fn set_lyric_bar_side(app: tauri::AppHandle, side: String) -> Result<(), String> {
    let v = if side == "left" { 1u8 } else { 0u8 };
    LYRIC_BAR_SIDE.store(v, Ordering::SeqCst);
    relocate_lyric_bar(&app);
    Ok(())
}

#[tauri::command]
// 状态栏歌词边距偏好（逻辑像素）：距任务栏边缘的距离
// （左侧 = 距任务栏左缘，右侧 = 距通知区左缘）。歌词条存在时立即重定位。
async fn set_lyric_bar_gap(app: tauri::AppHandle, gap: u32) -> Result<(), String> {
    LYRIC_BAR_GAP.store(gap.min(500), Ordering::SeqCst);
    relocate_lyric_bar(&app);
    Ok(())
}

#[tauri::command]
// 状态栏歌词宽度偏好（逻辑像素）：窗口最大宽度（默认 520），任务栏可用空间
// 不足时自动收缩。歌词条存在时立即重定位。
async fn set_lyric_bar_width(app: tauri::AppHandle, width: u32) -> Result<(), String> {
    LYRIC_BAR_WIDTH.store(
        width.clamp(LYRIC_BAR_MIN_W as u32, LYRIC_BAR_MAX_W),
        Ordering::SeqCst,
    );
    relocate_lyric_bar(&app);
    Ok(())
}

#[tauri::command]
// 拖动宽度滑块时的实时预览：更新偏好后按布局快照快速重定位。
// 完整定位每次都要 UIA 枚举任务按钮/插件（约 55ms），拖动时每帧调用会
// 明显卡顿；命中快照只做 SetWindowPos（约 3ms）。快照不存在时回退完整定位。
async fn set_lyric_bar_width_preview(app: tauri::AppHandle, width: u32) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;

    LYRIC_BAR_WIDTH.store(
        width.clamp(LYRIC_BAR_MIN_W as u32, LYRIC_BAR_MAX_W),
        Ordering::SeqCst,
    );

    let raw = LYRIC_BAR_HWND.load(Ordering::SeqCst);
    if raw == 0 {
        return Ok(());
    }
    let bar = HWND(raw as *mut core::ffi::c_void);

    let cached = LYRIC_BAR_LAYOUT
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    match cached {
        Some(layout) => {
            let scale = app
                .primary_monitor()
                .ok()
                .flatten()
                .map(|m| m.scale_factor())
                .unwrap_or(1.0);
            layout.apply(bar, (configured_lyric_bar_width() * scale) as i32);
            flash_lyric_bar_bounds(&app);
        }
        None => relocate_lyric_bar(&app),
    }
    Ok(())
}

/// 前端偏好文件路径：跨 origin 持久化。
/// dev 模式下前端跑在 http://127.0.0.1:1430（被占用时 CLI 会换随机端口），
/// 直接运行 exe 时跑在 tauri://localhost——localStorage 按 origin 隔离，
/// 端口/协议一变配置就"丢失"。存到后端文件后与 origin 无关。
fn frontend_prefs_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    base.join("qqmusic-monitor").join("frontend-prefs.json")
}

#[tauri::command]
// 读取前端偏好 JSON（不存在时返回 null）
async fn get_frontend_prefs() -> Result<Option<String>, String> {
    match tokio::fs::read_to_string(frontend_prefs_path()).await {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Failed to read frontend prefs: {}", e)),
    }
}

#[tauri::command]
// 保存前端偏好 JSON（自动创建目录）
async fn save_frontend_prefs(json: String) -> Result<(), String> {
    let path = frontend_prefs_path();
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("Failed to create prefs dir: {}", e))?;
    }
    tokio::fs::write(&path, json)
        .await
        .map_err(|e| format!("Failed to write frontend prefs: {}", e))
}

#[tauri::command]
// 状态栏歌词开关：开启时创建歌词窗口并通过 SetParent 嵌入任务栏
// （显示在通知区域左侧，任务栏自身交互不会盖住它），关闭时销毁。
// 歌词条复用 song-info / playback-progress 事件。
//
// side / gap / width 为位置与尺寸偏好，与开关一次事务式传入：窗口创建时的
// 首次定位读的就是这三个静态值，若等创建完再单独调 set_lyric_bar_side /
// _gap / _width，首次打开必然先按默认值（右侧、0 边距、520 宽）出现在错误位置。
async fn set_lyric_bar_enabled(
    app: tauri::AppHandle,
    enabled: bool,
    side: Option<String>,
    gap: Option<u32>,
    width: Option<u32>,
) -> Result<(), String> {
    use tauri::Manager;

    if let Some(side) = side {
        LYRIC_BAR_SIDE.store(if side == "left" { 1 } else { 0 }, Ordering::SeqCst);
    }
    if let Some(gap) = gap {
        LYRIC_BAR_GAP.store(gap.min(500), Ordering::SeqCst);
    }
    if let Some(width) = width {
        LYRIC_BAR_WIDTH.store(
            width.clamp(LYRIC_BAR_MIN_W as u32, LYRIC_BAR_MAX_W),
            Ordering::SeqCst,
        );
    }

    LYRIC_BAR_ENABLED.store(enabled, Ordering::SeqCst);

    if enabled {
        static WATCHDOG_RUNNING: AtomicBool = AtomicBool::new(false);
        if !WATCHDOG_RUNNING.swap(true, Ordering::SeqCst) {
            spawn_lyric_bar_watchdog(app.clone());
        }
        if app.get_webview_window("lyric-bar").is_none() {
            create_lyric_bar(&app)?;
        }
    } else if let Some(win) = app.get_webview_window("lyric-bar") {
        LYRIC_BAR_HWND.store(0, Ordering::SeqCst);
        // 窗口销毁后布局快照立即失效（下次预览回退完整定位重建快照）
        if let Ok(mut cache) = LYRIC_BAR_LAYOUT.lock() {
            *cache = None;
        }
        let _ = win.destroy();
        destroy_mini_player(&app);
    }

    Ok(())
}

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

mod cli;
mod config;
mod smtc;
mod lyrics;
mod song_info;
mod qrc;
mod local_qrc;
mod server;
mod audio_session;

use cli::Cli;
use config::Config;
use song_info::{PlaybackProgress, QrcLine, SongInfo};
use lyrics::LyricFetcher;

use std::collections::HashMap;

/// 当前 Unix 毫秒时间戳，用于 LRU 淘汰
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tauri::command]
async fn get_current_song_info() -> Result<SongInfo, String> {
    let latest = LATEST_SONG_INFO
        .get()
        .ok_or("Song state not initialized")?;
    Ok(latest.read().await.as_ref().clone())
}

fn apply_signed_offset(time_ms: u64, offset_ms: i64) -> u64 {
    if offset_ms >= 0 {
        time_ms.saturating_add(offset_ms as u64)
    } else {
        time_ms.saturating_sub(offset_ms.unsigned_abs())
    }
}

struct LastSongState {
    title: String,
    artist: String,
    total_time_ms: u64,
}

impl From<&SongInfo> for LastSongState {
    fn from(info: &SongInfo) -> Self {
        Self {
            title: info.title.clone(),
            artist: info.artist.clone(),
            total_time_ms: info.total_time_ms,
        }
    }
}

fn same_gui_payload(previous: &SongInfo, current: &SongInfo) -> bool {
    previous.title == current.title
        && previous.artist == current.artist
        && previous.album == current.album
        && previous.lyrics == current.lyrics
        && previous.trans == current.trans
        && previous.qrc_data == current.qrc_data
        && previous.album_pic_url == current.album_pic_url
        && previous.album_pic_hd_url == current.album_pic_hd_url
}

#[cfg(test)]
mod synchronization_tests {
    use super::*;

    #[test]
    fn signed_offset_supports_advance_and_delay() {
        assert_eq!(apply_signed_offset(1_000, 200), 1_200);
        assert_eq!(apply_signed_offset(1_000, -200), 800);
        assert_eq!(apply_signed_offset(100, -200), 0);
        assert_eq!(apply_signed_offset(u64::MAX - 5, 10), u64::MAX);
    }

    #[test]
    fn gui_payload_ignores_progress_only_changes() {
        let previous = SongInfo {
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            lyrics: "lyrics".to_string(),
            current_time_ms: 1_000,
            ..SongInfo::default()
        };
        let mut current = previous.clone();
        current.current_time_ms = 1_100;
        current.progress_percent = 12.5;
        current.server_ts = 42;

        assert!(same_gui_payload(&previous, &current));

        current.lyrics = "updated lyrics".to_string();
        assert!(!same_gui_payload(&previous, &current));
    }

    #[test]
    fn lyrics_cache_blocks_duplicate_and_immediate_retry() {
        let mut cache = LyricsCache::new();
        assert!(cache.begin_fetch("Song", "Artist"));
        assert!(!cache.begin_fetch("Song", "Artist"));

        cache.mark_retry("Song", "Artist");
        assert!(!cache.begin_fetch("Song", "Artist"));
    }

    #[test]
    fn album_names_match_accepts_consistent_names() {
        // 标点/括号差异容忍（SMTC 带括号后缀 vs 实际专辑名）
        assert!(album_names_match("ONE PIECE SUPER BEST (海贼王10周年 SUPER BEST)", "ONE PIECE SUPER BEST"));
        assert!(album_names_match("听！我们的歌", "听！我们的歌"));
        // 完全无关的专辑名不得误判为一致（如搜索 "Jay" 首条返回"钢琴曲纯音乐经典"）
        assert!(!album_names_match("Jay", "钢琴曲纯音乐经典"));
        assert!(!album_names_match("COUP D'ETAT [+ ONE OF A KIND & HEARTBREAKER]", "G-DRAGON 2ND ALBUM : COUP D`ETAT"));
        // 空串不可匹配
        assert!(!album_names_match("", "Jay"));
        assert!(!album_names_match("Jay", ""));
    }

    #[test]
    fn album_names_match_accepts_variant_naming() {
        // 同一专辑的不同命名变体：SMTC 报 "World War Joy (Explicit)"，
        // 歌曲详情返回 "World War Joy...Takeaway (Explicit)"——单词重叠应判为同一专辑
        assert!(album_names_match("World War Joy (Explicit)", "World War Joy...Takeaway (Explicit)"));
        // 重叠度不足的仍拒绝：相似但不同的专辑（0.75 重叠）与无关专辑
        assert!(!album_names_match("World War Joy (Explicit)", "World War III (Explicit)"));
        assert!(!album_names_match("Suicide Squad: The Album", "The Eminem Show (Clean)"));
        // 子集关系（"The Monster" ⊂ "The Monster Mash"）属于包含匹配，仍放行
        assert!(album_names_match("The Monster", "The Monster Mash"));
    }

    #[test]
    fn artist_names_match_stays_strict() {
        // 歌手判断必须严格：The Chainsmokers/Halsey 与 新田真剑佑/The Chainsmokers
        // 共享单词但完全不是同一首歌的歌手，不得误判
        assert!(!artist_names_match("The Chainsmokers/Halsey", "新田真剑佑/The Chainsmokers"));
        // 多歌手子集仍应放行（feat. 变体）
        assert!(artist_names_match("Eminem/Survivor", "Eminem"));
        assert!(artist_names_match("G-DRAGON (권지용)", "G-DRAGON"));
    }

    #[test]
    fn song_match_ok_rejects_wrong_artist_cover() {
        use lyrics::{AlbumCover, CoverKind};
        let refs = ["STAY (Explicit)"];
        // "Stay explicit" - 屈直林：歌手与查询不符，Single 封面也必须拒绝
        let wrong = AlbumCover {
            mid: "004LS1ro32YK1a".into(),
            name: String::new(),
            singer: "屈直林（Quandos-Lin）".into(),
            kind: CoverKind::Single,
        };
        assert!(!song_match_ok(&wrong, "The Kid LAROI/Justin Bieber", &refs),
            "歌手不符的单曲封面必须被拒绝");

        // 正确候选：STAY - The Kid LAROI/Justin Bieber，专辑名一致
        let right = AlbumCover {
            mid: "004OTM9c0OfNRw".into(),
            name: "STAY (Explicit)".into(),
            singer: "The Kid LAROI/Justin Bieber".into(),
            kind: CoverKind::Album,
        };
        assert!(song_match_ok(&right, "The Kid LAROI/Justin Bieber", &refs));

        // 多歌手子集（Eminem/Survivor vs Eminem）应放行
        let sub = AlbumCover {
            mid: "x".into(),
            name: "The Eminem Show (Clean)".into(),
            singer: "Eminem/Survivor".into(),
            kind: CoverKind::Album,
        };
        assert!(song_match_ok(&sub, "Eminem", &["The Eminem Show (Clean)"]));

        // 专辑名不符的 Album 候选拒绝（同歌手、不同专辑 = 不同版本）
        let wrong_album = AlbumCover {
            mid: "001sDxZZ0lzDJX".into(),
            name: "Suicide Squad: The Album".into(),
            singer: "Eminem".into(),
            kind: CoverKind::Album,
        };
        assert!(!song_match_ok(&wrong_album, "Eminem", &["The Eminem Show (Clean)"]));
    }

    #[test]
    fn album_pic_is_hd_detects_500_vs_150() {
        assert!(album_pic_is_hd(Path::new("T002R500x500M000004JAIMT1Pl1z9_1.jpg")));
        assert!(album_pic_is_hd(Path::new("T062R500x500M000002PE0rX0FxNck.jpg")));
        assert!(!album_pic_is_hd(Path::new("T002R150x150M000001rWGUJ3Gvypx_2.jpg")));
        assert!(!album_pic_is_hd(Path::new("T062R150x150M000002PE0rX0FxNck.jpg")));
        assert!(!album_pic_is_hd(Path::new("weird.txt")));
    }

    #[test]
    fn cover_cdn_url_uses_kind_prefix() {
        use lyrics::CoverKind;
        let album = cover_cdn_url(CoverKind::Album, "004JAIMT1Pl1z9");
        assert!(album.starts_with("https://y.gtimg.cn/music/photo_new/T002R800x800M000004JAIMT1Pl1z9.jpg"));
        let single = cover_cdn_url(CoverKind::Single, "002PE0rX0FxNck");
        assert!(single.starts_with("https://y.gtimg.cn/music/photo_new/T062R800x800M000002PE0rX0FxNck.jpg"));
    }
}

/// 缓存淘汰上限：超出后淘汰最久未访问的条目。
/// 每条含歌词文本 + QRC 逐字数据，按平均 8KB/条估算，128 条约 1MB 内存。
const LYRICS_CACHE_MAX_ENTRIES: usize = 128;
/// 在线歌词获取失败后的重试间隔。
/// 原 30s 太长：QQ 音乐客户端通常在播放开始 1~2s 后就把歌词写进本地缓存，
/// 失败重试时优先命中本地即可恢复（配合下方的本地重查轮询）。
const LYRICS_RETRY_DELAY_MS: u64 = 5_000;

/// 本地歌词重查间隔：QQ 音乐客户端是「播放后才滞后写入 qrc」的，
/// 切歌瞬间本地常查不到（本地查找只在切歌时做一次），随后 1~2s 才落盘。
/// 在线获取期间按此间隔重查本地，命中即用本地逐字歌词覆盖。
const LYRIC_LOCAL_RESCAN_INTERVAL_MS: u64 = 1_500;

/// 本地歌词重查最大次数（1.5s × 8 = 12s，覆盖客户端写入延迟）
const LYRIC_LOCAL_RESCAN_MAX_TRIES: u32 = 8;

#[derive(Clone, Copy)]
enum LyricsFetchState {
    Loading,
    Ready,
    RetryAfter(u64),
}

/// 歌词/封面数据来源（日志与缓存标识：让用户明确看到用的是本地还是在线）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LyricsSource {
    Local,
    Online,
    None,
}

struct LyricsCacheEntry {
    lyrics: String,
    trans: String,
    qrc_raw: String,
    qrc_data: Vec<QrcLine>,
    album_pic_url: String,
    /// 高清封面 URL：album_pic_url 为低清本地图(150x150)时携带 800x800 在线地址
    album_pic_hd_url: String,
    /// 本地缓存文件名中解析出的专辑名（QQ 音乐索引时的原始名），
    /// 比依赖 SMTC 报告的 album 更可靠，用作在线 album_mid 解析的搜索词。
    local_album: String,
    /// 最后访问时间戳（毫秒），用于 LRU 淘汰
    last_accessed: u64,
    fetch_state: LyricsFetchState,
    source: LyricsSource,
}

impl LyricsCacheEntry {
    fn loading() -> Self {
        Self {
            lyrics: String::new(),
            trans: String::new(),
            qrc_raw: String::new(),
            qrc_data: Vec::new(),
            album_pic_url: String::new(),
            album_pic_hd_url: String::new(),
            local_album: String::new(),
            last_accessed: 0,
            fetch_state: LyricsFetchState::Loading,
            source: LyricsSource::None,
        }
    }
}

/// 后台歌词缓存 — 由主循环读取、后台任务写入 (使用 HashMap 支持多歌曲缓存，防止切歌竞态)
/// 超过 LYRICS_CACHE_MAX_ENTRIES 时淘汰最久未访问的条目（近似 LRU）。
struct LyricsCache {
    entries: HashMap<String, LyricsCacheEntry>,
}

impl LyricsCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn begin_fetch(&mut self, title: &str, artist: &str) -> bool {
        let key = format!("{}|{}", title, artist);
        let now = current_timestamp_ms();

        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_accessed = now;
            return match entry.fetch_state {
                LyricsFetchState::RetryAfter(retry_at) if now >= retry_at => {
                    entry.fetch_state = LyricsFetchState::Loading;
                    true
                }
                LyricsFetchState::Loading
                | LyricsFetchState::Ready
                | LyricsFetchState::RetryAfter(_) => false,
            };
        }

        self.insert_entry(title, artist, LyricsCacheEntry::loading());
        true
    }

    fn mark_retry(&mut self, title: &str, artist: &str) {
        let key = format!("{}|{}", title, artist);
        if let Some(entry) = self.entries.get_mut(&key) {
            let now = current_timestamp_ms();
            entry.last_accessed = now;
            entry.fetch_state = LyricsFetchState::RetryAfter(
                now.saturating_add(LYRICS_RETRY_DELAY_MS),
            );
        }
    }

    fn get_entry(&mut self, title: &str, artist: &str) -> Option<&LyricsCacheEntry> {
        let key = format!("{}|{}", title, artist);
        let now = current_timestamp_ms();
        let entry = self.entries.get_mut(&key)?;
        entry.last_accessed = now;
        Some(entry)
    }

    /// 只读访问条目（不更新 LRU 时间戳），用于在 read guard 下读取字段。
    fn peek_entry(&self, title: &str, artist: &str) -> Option<&LyricsCacheEntry> {
        let key = format!("{}|{}", title, artist);
        self.entries.get(&key)
    }

    fn insert_entry(&mut self, title: &str, artist: &str, mut entry: LyricsCacheEntry) {
        let key = format!("{}|{}", title, artist);
        entry.last_accessed = current_timestamp_ms();

        if self.entries.len() >= LYRICS_CACHE_MAX_ENTRIES && !self.entries.contains_key(&key) {
            self.evict_oldest();
        }

        self.entries.insert(key, entry);
    }

    /// 部分更新：覆盖缓存中某首歌的封面（主图 + 高清升级地址）
    fn update_album_pic_url(&mut self, title: &str, artist: &str, url: String, hd_url: String) {
        let key = format!("{}|{}", title, artist);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.album_pic_url = url;
            entry.album_pic_hd_url = hd_url;
            entry.last_accessed = current_timestamp_ms();
        }
    }

    /// 淘汰 last_accessed 最小的条目
    fn evict_oldest(&mut self) {
        if let Some((oldest_key, _)) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_accessed)
            .map(|(k, v)| (k.clone(), v.last_accessed))
        {
            self.entries.remove(&oldest_key);
        }
    }
}

/// 缓存 QQ 音乐本地缓存根目录，只探测一次避免每帧扫盘
fn get_cache_root() -> Option<PathBuf> {
    CACHED_CACHE_ROOT
        .get_or_init(local_qrc::auto_detect_cache_root)
        .clone()
}

/// 获取歌词缓存目录（QQMusicLyricNew）
fn get_lyric_cache_dir() -> Option<PathBuf> {
    get_cache_root().map(|root| root.join("QQMusicLyricNew"))
}

/// 获取封面图缓存目录（QQMusicPicture）
fn get_picture_cache_dir() -> Option<PathBuf> {
    get_cache_root().map(|root| root.join("QQMusicPicture"))
}

/// 清洗专辑名：去掉中英文括号内容及前后空格，用于 SmartBox 模糊匹配。
/// 例：`ONE PIECE SUPER BEST (海贼王10周年 SUPER BEST)` → `ONE PIECE SUPER BEST`
fn clean_album_name(raw: &str) -> String {
    let mut s = raw.to_string();
    while let Some(start) = s.find('(') {
        if let Some(end) = s[start..].find(')') {
            s.replace_range(start..start + end + 1, "");
        } else {
            break;
        }
    }
    while let Some(start) = s.find('（') {
        if let Some(end) = s[start..].find('）') {
            let end_byte = start + end + '）'.len_utf8();
            s.replace_range(start..end_byte, "");
        } else {
            break;
        }
    }
    s.trim().to_string()
}

/// 歌手名一致性校验：normalize（小写 + 标点折叠）后互相包含即视为一致。
/// 严格匹配——歌手判断不能放宽（防止标题模糊命中其他歌手的翻唱）。
fn artist_names_match(a: &str, b: &str) -> bool {
    let na = local_qrc::normalize(a);
    let nb = local_qrc::normalize(b);
    !na.is_empty() && !nb.is_empty() && (na.contains(&nb) || nb.contains(&na))
}

/// 专辑名一致性校验：normalize 后互相包含即视为一致；
/// 或单词重叠度足够高（处理同一专辑的不同命名变体，
/// 如 "World War Joy (Explicit)" vs "World War Joy...Takeaway (Explicit)"）。
fn album_names_match(a: &str, b: &str) -> bool {
    let na = local_qrc::normalize(a);
    let nb = local_qrc::normalize(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    if na.contains(&nb) || nb.contains(&na) {
        return true;
    }
    // 单词重叠：过滤单字母词后，共同词数占较短者比例足够高即视为同一专辑
    let wa: std::collections::HashSet<String> = na
        .split(' ')
        .filter(|w| w.chars().count() > 1)
        .map(String::from)
        .collect();
    let wb: std::collections::HashSet<String> = nb
        .split(' ')
        .filter(|w| w.chars().count() > 1)
        .map(String::from)
        .collect();
    if wa.is_empty() || wb.is_empty() {
        return false;
    }
    let shared = wa.intersection(&wb).count();
    let min_len = wa.len().min(wb.len());
    // 0.8 阈值：变体命名（"World War Joy...Takeaway (Explicit)" vs
    // "World War Joy (Explicit)"，重叠 1.0）放行；相似但不同的专辑
    // （"World War Joy" vs "World War III"，重叠 0.75）拒绝
    shared >= 2 && (shared as f64 / min_len as f64) >= 0.8
}

/// 歌曲候选身份校验：只有候选歌曲与播放中歌曲一致时才采信其封面。
///
/// 1. 歌手必须匹配（normalize 互相包含）；候选或查询歌手为空时跳过。
///    防止标题模糊匹配命中别的歌（如 "STAY (Explicit)" 命中 "Stay explicit"，
///    歌手完全不同，其 vs 单曲封面也是错的）。
/// 2. Album 类候选：专辑名必须与参考专辑（本地 QRC 文件名 / SMTC）一致。
///    Single 类（无专辑，vs 单曲封面）在歌手通过后直接采信。
fn song_match_ok(cover: &lyrics::AlbumCover, query_artist: &str, refs: &[&str]) -> bool {
    if !cover.singer.is_empty() && !query_artist.is_empty() {
        if !artist_names_match(&cover.singer, query_artist) {
            return false;
        }
    }
    if cover.kind == lyrics::CoverKind::Album {
        if refs.is_empty() {
            return true;
        }
        return refs.iter().any(|r| album_names_match(r, &cover.name));
    }
    true
}

/// 本地封面文件是否为高清（R500x500）。
/// 仅 R150x150 时视为低清：调用方应以低清 data URI 即时显示，
/// 并携带 800x800 在线地址（album_pic_hd_url）供前端预加载后升级。
fn album_pic_is_hd(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains("R500x500"))
        .unwrap_or(false)
}

/// 按封面类型生成 800x800 在线 CDN URL（T002 专辑 / T062 单曲）
fn cover_cdn_url(kind: lyrics::CoverKind, mid: &str) -> String {
    let prefix = match kind {
        lyrics::CoverKind::Album => "T002",
        lyrics::CoverKind::Single => "T062",
    };
    format!("https://y.gtimg.cn/music/photo_new/{}R800x800M000{}.jpg?max_age=2592000", prefix, mid)
}

/// 优先使用本地缓存的专辑封面图，找不到则返回原始在线 URL。
///
/// 解析顺序：
///  0. SMTC 专辑名可用时，先解析「实际播放专辑」的封面——歌词搜索匹配到的
///     可能是不同版本（单曲版/合辑版，如 High Hopes 的 Pray for the Wicked
///     vs Summer 2018），其专辑封面与正在播放的不一致。专辑名搜索
///     （候选名逐条验证）→ legacy 兜底（强制歌手校验，防止通用专辑名
///     匹配到别的歌手的同名专辑）；本地优先，无本地图直接采用其 CDN URL。
///  1. 原在线 URL 的 album_mid 查本地（SMTC 专辑名为空时的兜底）。
///  2. 都未命中 → 原在线 URL。
///
/// 覆盖采用判定（matched_album 为歌词匹配版本的专辑名）：
///  解析出的 mid 与原文 URL mid 不同时，若歌词版本与 SMTC 专辑一致
///  （"Unstoppable" 场景：The Score 有两张同名专辑，legacy 排序可能
///  命中错版 002vztFj10OO9E）→ 原 URL（歌词精确匹配版本）更权威，回退；
///  不一致（合辑版场景）→ 解析结果才是实际播放专辑，采用。
///
/// 返回 (主图, 高清升级地址)：
///  - 本地命中 500x500 → (data URI, "")：已高清，无需升级
///  - 本地仅 150x150 → (data URI, 800x800 在线 URL)：先即时显示低清，
///    前端预加载高清后平滑升级（避免直接上在线 URL 的加载空白）
///  - 无本地图 → (在线 URL, "")：主图即在线高清图
async fn resolve_album_pic_url(
    online_url: &str,
    album_name: &str,
    matched_album: &str,
    title: &str,
    artist: &str,
    fetcher: &LyricFetcher,
) -> (String, String) {
    if online_url.is_empty() {
        return (String::new(), String::new());
    }

    // 只处理 QQ 音乐 CDN 的在线 URL（data:image base64 是 SMTC 缩略图，不替换）
    if !online_url.contains("y.gtimg.cn") {
        return (online_url.to_string(), String::new());
    }

    let Some(pic_dir) = get_picture_cache_dir() else {
        return (online_url.to_string(), String::new());
    };

    // 本地图 → data URI；返回 (uri, 该 mid 的高清在线 URL)。
    // 本地只有 150x150 时主图仍是 data URI（即时显示），高清地址交给前端升级。
    fn local_to_uri(pic_dir: &Path, mid: &str, hd_url: &str) -> Option<(String, String)> {
        let local_path = local_qrc::find_album_pic(pic_dir, mid)?;
        let data = std::fs::read(&local_path).ok()?;
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let uri = format!("data:image/jpeg;base64,{}", STANDARD.encode(&data));
        let hd = if album_pic_is_hd(&local_path) { String::new() } else { hd_url.to_string() };
        Some((uri, hd))
    }

    // 第 0 步：SMTC 专辑名解析"实际播放专辑"的封面（正确性优先——
    // 歌词匹配到的版本可能来自不同专辑，如合辑版 vs 原版）。
    if !album_name.is_empty() {
        let mut variants: Vec<String> = vec![album_name.to_string()];
        let cleaned = clean_album_name(album_name);
        if cleaned != album_name {
            variants.push(cleaned);
        }
        // 0a. SmartBox 专辑搜索（候选名去括号验证 + 歌手校验，
        //     通用专辑名会命中别的歌手的同名专辑，如 "Way Back" → "Way Back Home"）
        let mut correct_mid: Option<String> = None;
        for variant in &variants {
            if let Some(am) = fetcher.search_album_mid_by_name(variant, artist).await {
                correct_mid = Some(am);
                break;
            }
        }
        // 0b. legacy 歌曲搜索按专辑名兜底（三重校验：专辑名 + 歌名 + 歌手）
        if correct_mid.is_none() {
            if let Some(songmid) = fetcher.find_song_by_album_name(&variants, title, artist, album_name).await {
                if let Ok(cover) = fetcher.get_album_cover(&songmid).await {
                    if cover.kind == lyrics::CoverKind::Album && album_names_match(album_name, &cover.name) {
                        correct_mid = Some(cover.mid);
                    }
                }
            }
        }
        if let Some(mid) = correct_mid.as_ref() {
            // 采用判定：解析结果与原 URL 不同时，若歌词匹配版本与 SMTC 专辑一致，
            // 则原 URL（歌词精确匹配的专辑）更权威，拒绝覆盖（同名同歌手双专辑
            // 场景，如 The Score 的 Unstoppable 有 001wHjwD0IWIRm 与 002vztFj10OO9E
            // 两张同名专辑，legacy 排序可能命中错版）。
            let online_mid = local_qrc::extract_album_mid_from_url(online_url);
            if online_mid.as_deref() != Some(mid.as_str()) {
                if !matched_album.is_empty() && album_names_match(matched_album, album_name) {
                    if debug_mode_enabled() {
                        eprintln!("[lyrics] 封面: 歌词版本专辑'{}'与SMTC'{}'一致,拒绝覆盖解析结果 {}", matched_album, album_name, mid);
                    }
                    correct_mid = None;
                } else if debug_mode_enabled() {
                    eprintln!("[lyrics] 封面: 歌词版本'{}'≠SMTC'{}',采用解析结果 {}", matched_album, album_name, mid);
                }
            }
        }
        if let Some(mid) = correct_mid {
            let hd_url = format!("https://y.gtimg.cn/music/photo_new/T002R800x800M000{}.jpg?max_age=2592000", mid);
            if let Some(res) = local_to_uri(&pic_dir, &mid, &hd_url) {
                return res;
            }
            return (hd_url, String::new());
        }
    }

    // 第 1 步（兜底）：用在线 URL 中的 album_mid 查本地
    if let Some(album_mid) = local_qrc::extract_album_mid_from_url(online_url) {
        if let Some(res) = local_to_uri(&pic_dir, &album_mid, online_url) {
            return res;
        }
    }

    (online_url.to_string(), String::new())
}

/// 从本地 QQ 音乐缓存目录查找歌词（QRC 优先，LRC 兜底），返回完整的缓存条目。
/// 此函数包含文件 I/O 与 DES 解密，仅应在 spawn_blocking 中调用。
fn lookup_local_lyrics(title: &str, artist: &str) -> LyricsCacheEntry {
    let mut entry = LyricsCacheEntry::loading();

    let Some(cache_dir) = get_lyric_cache_dir() else {
        return entry;
    };

    // QRC 优先（逐字歌词）
    if let Some(qrc_file) = local_qrc::find_qrc_file(&cache_dir, title, artist) {
        // 记录本地文件名中的专辑名，供后续在线专辑封面解析复用
        if let Some(fname) = qrc_file.file_name().and_then(|n| n.to_str()) {
            entry.local_album = local_qrc::parse_lyric_filename(fname).2;
        }
        match qrc::decode_qrc_from_file(&qrc_file) {
            Ok(xml) => {
                entry.qrc_raw = "[local]".to_string();
                if let Ok(lines) = qrc::parse_qrc_xml(&xml) {
                    entry.qrc_data = lines;
                }
                if entry.lyrics.is_empty() {
                    entry.lyrics = qrc::extract_lrc_from_xml(&xml).unwrap_or_default();
                }
                if let Some(trans_file) = local_qrc::find_qrc_trans_file(&qrc_file) {
                    if let Ok(trans_xml) = qrc::decode_qrc_from_file(&trans_file) {
                        entry.trans = trans_xml;
                    }
                }
            }
            Err(e) => {
                eprintln!("  ⚠ QRC解密失败: {:?} — {}", qrc_file.file_name().unwrap_or_default(), e);
            }
        }
    }

    // LRC 兜底（普通歌词，无逐字）
    if entry.lyrics.is_empty() {
        if let Some(lrc_file) = local_qrc::find_lrc_file(&cache_dir, title, artist) {
            let lrc_raw = match qrc::decode_qrc_from_file(&lrc_file) {
                Ok(decrypted) => decrypted,
                Err(_) => std::fs::read_to_string(&lrc_file).unwrap_or_default(),
            };
            entry.lyrics = qrc::extract_lrc_from_xml(&lrc_raw).unwrap_or(lrc_raw);
            if let Some(trans_lrc_file) = local_qrc::find_lrc_trans_file(&lrc_file) {
                let trans_raw = match qrc::decode_qrc_from_file(&trans_lrc_file) {
                    Ok(decrypted) => decrypted,
                    Err(_) => std::fs::read_to_string(&trans_lrc_file).unwrap_or_default(),
                };
                entry.trans = qrc::extract_lrc_from_xml(&trans_raw).unwrap_or(trans_raw);
            }
        } else {
            // LRC 也未找到：打印诊断，帮助判断是文件名匹配问题还是真的没有文件
            if let Ok(entries) = std::fs::read_dir(&cache_dir) {
                let norm_title = local_qrc::normalize(title);
                let mut lrc_candidates: Vec<String> = Vec::new();
                for e in entries.flatten() {
                    let fname = match e.file_name().to_str() {
                        Some(n) => n.to_string(), None => continue,
                    };
                    if !fname.ends_with("_qm.lrc") { continue; }
                    let base = fname.trim_end_matches("_qm.lrc");
                    let parts: Vec<&str> = base.splitn(4, " - ").collect();
                    if parts.len() >= 2 && local_qrc::normalize(parts[1]).contains(&norm_title) {
                        lrc_candidates.push(fname);
                    }
                }
                if !lrc_candidates.is_empty() {
                    eprintln!("  ℹ LRC候选(未匹配): {}", lrc_candidates.join(", "));
                }
            }
        }
    }

    entry
}

// Function to run the main monitor loop in a background thread.
async fn run_monitor(app_handle: Option<tauri::AppHandle>, args: Cli, config: Config) -> Result<()> {

    // 设置Ctrl+C处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;
    
    if !args.quiet && config.settings.debug_mode {
        println!("🎵 QQMusic Reader v{} (SMTC Mode)", env!("CARGO_PKG_VERSION"));
        println!("========================================");
        println!("⚠️  正在使用 Windows 系统媒体控制接口");
        println!("========================================\n");
    }
    DEBUG_ENABLED.store(config.settings.debug_mode, Ordering::SeqCst);

    // 初始化歌词获取器和后台缓存
    let lyric_fetcher = Arc::new(LyricFetcher::with_debug(config.settings.debug_mode));
    let lyrics_cache = Arc::new(RwLock::new(LyricsCache::new()));

    // WASAPI 音频会话门控：实时检测 QQ音乐是否真的在出声
    audio_session::AudioGate::ensure_com_init();
    let mut audio_gate = audio_session::AudioGate::new();

    // 初始化数据广播通道并启动服务
    let (tx, rx) = tokio::sync::watch::channel(Arc::new(SongInfo::default()));
    if config.settings.enable_server {
        let port = config.settings.server_port;
        if config.settings.debug_mode {
            // 在 TUI 接管前打印服务器信息，避免与渲染竞态
            println!("🚀 本地同步服务已启动: http://127.0.0.1:{}", port);
            println!("📡 WebSocket 接口: ws://127.0.0.1:{}/ws", port);
            println!("📄 当前状态接口: http://127.0.0.1:{}/api/current", port);
        }
        tokio::spawn(async move {
            server::start_server(port, rx).await;
        });
    }





    let mut last_song_state: Option<LastSongState> = None;
    let mut last_gui_snapshot: Option<SongInfo> = None;
    let mut stale_song_total: Option<u64> = None;
    let mut last_logged_lyric_key = String::new();

    // 主循环
    while running.load(Ordering::SeqCst) {
        let loop_start = tokio::time::Instant::now();
        // Read active config dynamically in each loop iteration
        let config = {
            if let Some(cfg_lock) = CONFIG.get() {
                cfg_lock.read().await.clone()
            } else {
                config.clone()
            }
        };
        let loop_result: Result<()> = async {
            // 使用 SMTC 读取媒体信息（含会话源过滤：只接受 QQ Music，过滤其他音源）
            let mut current_song_info = match smtc::get_current_media_info().await {
            Ok(info) => {
                if let Some(mut info) = info {
                    // 后台歌词加载：切歌时后台请求，不阻塞 TUI
                    let quiet = args.quiet;
                    let debug = config.settings.debug_mode;
                    let song_key = format!("{}|{}", info.title, info.artist);
                    let should_fetch = lyrics_cache
                        .write()
                        .await
                        .begin_fetch(&info.title, &info.artist);

                    if should_fetch {
                        // 标记已记录：避免同一首歌在获取完成前被缓存命中路径重复输出
                        last_logged_lyric_key = song_key;
                        let cache = lyrics_cache.clone();
                        let fetcher = lyric_fetcher.clone();
                        let t = info.title.clone();
                        let a = info.artist.clone();
                        let album = info.album.clone();
                        let max_attempts = config.settings.max_retries.saturating_add(1);
                        tokio::spawn(async move {
                            let has_local_qrc;
                            // 1. 本地歌词快速查找并立即插入缓存（~50ms，不阻塞在线获取）
                            {
                                let t2 = t.clone();
                                let a2 = a.clone();
                                let mut local = tokio::task::spawn_blocking(move || lookup_local_lyrics(&t2, &a2))
                                    .await
                                    .unwrap_or_else(|_| LyricsCacheEntry::loading());

                                let local_qrc = local.qrc_data.len();
                                let local_lrc = !local.lyrics.is_empty();
                                let local_trans = !local.trans.is_empty();
                                if debug {
                                    if local_qrc > 0 || local_lrc || local_trans {
                                        let mut parts: Vec<String> = Vec::new();
                                        if local_qrc > 0 { parts.push(format!("QRC {}行", local_qrc)); }
                                        if local_lrc { parts.push("歌词".to_string()); }
                                        if local_trans { parts.push("翻译".to_string()); }
                                        eprintln!("[lyrics] 本地: ✓ {}", parts.join(" + "));
                                    } else {
                                        eprintln!("[lyrics] 本地: ✗ 无数据");
                                        // 诊断：显示文件名格式 + 模糊匹配候选
                                        eprintln!("[lyrics]   搜索词: artist='{}' title='{}'", local_qrc::normalize(&a), local_qrc::normalize(&t));
                                        if let Some(ref dir) = get_lyric_cache_dir() {
                                            if let Ok(entries) = std::fs::read_dir(dir) {
                                                let mut qrc_count = 0u32;
                                                let mut best_candidate: Option<(String, u32)> = None;
                                                let norm_title = local_qrc::normalize(&t);
                                                let norm_artist = local_qrc::normalize(&a);
                                                for entry in entries.flatten() {
                                                    let fname = match entry.file_name().to_str() {
                                                        Some(n) => n.to_string(),
                                                        None => continue,
                                                    };
                                                    if !fname.ends_with("_qm.qrc") { continue; }
                                                    qrc_count += 1;
                                                    // 计算与当前歌曲的匹配度
                                                    let base = fname.trim_end_matches("_qm.qrc");
                                                    let parts: Vec<&str> = base.splitn(4, " - ").collect();
                                                    if parts.len() >= 2 {
                                                        let f_artist = local_qrc::normalize(parts[0]);
                                                        let f_title = local_qrc::normalize(parts[1]);
                                                        let mut score: u32 = 0;
                                                        if f_title == norm_title { score += 100; }
                                                        else if f_title.contains(&norm_title) || norm_title.contains(&f_title) { score += 60; }
                                                        if f_artist == norm_artist { score += 50; }
                                                        else if f_artist.contains(&norm_artist) || norm_artist.contains(&f_artist) { score += 30; }
                                                        if score > 0 && (best_candidate.is_none() || score > best_candidate.as_ref().unwrap().1) {
                                                            best_candidate = Some((fname, score));
                                                        }
                                                    }
                                                }
                                                eprintln!("[lyrics]   目录QRC总数: {} | 最佳候选: {}", qrc_count,
                                                    best_candidate.map_or("无部分匹配".to_string(), |(f, s)| format!("'{}' (分数:{})", f, s)));
                                            }
                                        }
                                    }
                                }

                                has_local_qrc = !local.qrc_data.is_empty();

                                if !local.lyrics.is_empty() || has_local_qrc {
                                    local.fetch_state = if has_local_qrc {
                                        LyricsFetchState::Ready
                                    } else {
                                        LyricsFetchState::Loading
                                    };
                                    local.source = LyricsSource::Local;
                                    let mut w = cache.write().await;
                                    w.insert_entry(&t, &a, local);
                                    if debug { eprintln!("[lyrics] Local cache populated for '{} - {}'", t, a); }
                                }
                            }

                            // 1.5. 本地歌词重查轮询：QQ 音乐客户端是播放后才滞后写入
                            // qrc 的，切歌瞬间本地常查不到；在线请求期间每 1.5s 重查本地，
                            // 命中即用本地逐字歌词覆盖缓存（仅切歌瞬间本地无 QRC 时启动）。
                            if !has_local_qrc {
                                let cache_rescan = cache.clone();
                                let t_rescan = t.clone();
                                let a_rescan = a.clone();
                                tokio::spawn(async move {
                                    for _ in 0..LYRIC_LOCAL_RESCAN_MAX_TRIES {
                                        tokio::time::sleep(Duration::from_millis(
                                            LYRIC_LOCAL_RESCAN_INTERVAL_MS,
                                        ))
                                        .await;

                                        // 已切到别的歌：停止轮询（旧歌不必再补缓存）
                                        let still_current = match LATEST_SONG_INFO.get() {
                                            Some(lock) => {
                                                let s = lock.read().await;
                                                s.title == t_rescan && s.artist == a_rescan
                                            }
                                            None => false,
                                        };
                                        if !still_current {
                                            break;
                                        }

                                        // 已经是本地 QRC：无需再扫
                                        {
                                            let guard = cache_rescan.read().await;
                                            if let Some(e) = guard
                                                .entries
                                                .get(&format!("{}|{}", t_rescan, a_rescan))
                                            {
                                                if e.source == LyricsSource::Local
                                                    && !e.qrc_data.is_empty()
                                                {
                                                    break;
                                                }
                                            }
                                        }

                                        let t2 = t_rescan.clone();
                                        let a2 = a_rescan.clone();
                                        let mut local = tokio::task::spawn_blocking(move || {
                                            lookup_local_lyrics(&t2, &a2)
                                        })
                                        .await
                                        .unwrap_or_else(|_| LyricsCacheEntry::loading());
                                        if local.qrc_data.is_empty() && local.lyrics.is_empty() {
                                            continue;
                                        }
                                        local.fetch_state = LyricsFetchState::Ready;
                                        local.source = LyricsSource::Local;
                                        cache_rescan.write().await.insert_entry(
                                            &t_rescan,
                                            &a_rescan,
                                            local,
                                        );
                                        if debug {
                                            eprintln!(
                                                "[lyrics] Local rescan hit for '{} - {}'",
                                                t_rescan, a_rescan
                                            );
                                        }
                                        break;
                                    }
                                });
                            }

                            // 封面策略（用户 2026-10：远程搜索有法律风险，只能最后手段）：
                            // SMTC 缩略图是 QQ 音乐客户端主动推给系统的当前封面（零远程
                            // 请求、与客户端显示一致），本地缓存图同理；两者都存在时
                            // 跳过一切远程封面解析（专辑搜索 / CDN 图）。
                            let has_local_art = match LATEST_SONG_INFO.get() {
                                Some(lock) => {
                                    let s = lock.read().await;
                                    s.title == t
                                        && s.artist == a
                                        && s.album_pic_url.starts_with("data:image")
                                }
                                None => false,
                            };

                            // 2. 在线歌词获取（已停用：用户 2026-10 要求只用本地歌词，
                            //    本地搜不到就不显示歌词；保留实现便于将来恢复）
                            if false && !has_local_qrc {
                            let mut attempt = 0u32;
                            let online_result = loop {
                                attempt += 1;
                                match fetcher.fetch_lyrics(&t, &a).await {
                                    Ok(result) => break Ok(result),
                                    Err(error) if attempt < max_attempts => {
                                        let backoff_ms = 250u64
                                            .saturating_mul(1u64 << (attempt - 1).min(3));
                                        if debug {
                                            eprintln!(
                                                "[lyrics] attempt {}/{} failed, retrying in {}ms: {}",
                                                attempt, max_attempts, backoff_ms, error
                                            );
                                        }
                                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                                    }
                                    Err(error) => break Err(error),
                                }
                            };

                            match online_result {
                                Ok((l, trans, q, pic_url, matched_album)) => {
                                    // 已有 SMTC/本地封面 → 不发起远程封面解析（远程仅最后手段）
                                    let (resolved_pic_url, resolved_hd_url) = if has_local_art {
                                        (String::new(), String::new())
                                    } else {
                                        resolve_album_pic_url(&pic_url, &album, &matched_album, &t, &a, &fetcher).await
                                    };
                                    let mut entry = LyricsCacheEntry {
                                        lyrics: l,
                                        trans,
                                        qrc_raw: q,
                                        qrc_data: Vec::new(),
                                        album_pic_url: resolved_pic_url,
                                        album_pic_hd_url: resolved_hd_url,
                                        local_album: String::new(),
                                        last_accessed: 0,
                                        fetch_state: LyricsFetchState::Ready,
                                        source: LyricsSource::None,
                                    };

                                    // 在线 QRC 解密
                                    if !entry.qrc_raw.is_empty() {
                                        if debug { eprintln!("[QRC] Raw data: {} bytes", entry.qrc_raw.len()); }
                                        match qrc::decode_qrc(&entry.qrc_raw) {
                                            Ok(xml) => {
                                                if debug { eprintln!("[QRC] Decrypted XML: {} bytes", xml.len()); }
                                                match qrc::parse_qrc_xml(&xml) {
                                                    Ok(lines) => {
                                                        if debug { eprintln!("[QRC] Parsed {} lines from XML", lines.len()); }
                                                        entry.qrc_data = lines;
                                                    }
                                                    Err(err) => {
                                                        if !quiet { eprintln!("[QRC] XML parse failed for '{} - {}': {}", t, a, err); }
                                                    }
                                                }
                                                if entry.lyrics.is_empty() {
                                                    entry.lyrics = qrc::extract_lrc_from_xml(&xml).unwrap_or_default();
                                                    if debug && !entry.lyrics.is_empty() { eprintln!("[QRC] Extracted LRC from XML: {} chars", entry.lyrics.len()); }
                                                }
                                                if entry.qrc_data.is_empty() && !entry.lyrics.is_empty() {
                                                    let parsed = qrc::parse_qrc_text(&entry.lyrics);
                                                    if !parsed.is_empty() {
                                                        if debug { eprintln!("[QRC] Text fallback parsed {} lines", parsed.len()); }
                                                        entry.qrc_data = parsed;
                                                    }
                                                }
                                            },
                                            Err(err) => {
                                                if !quiet { eprintln!("[QRC] Decode failed for '{} - {}': {}", t, a, err); }
                                            }
                                        }
                                    }

                                    // 在线数据未覆盖的字段回填本地缓存
                                    if entry.lyrics.is_empty() || entry.qrc_data.is_empty() || entry.trans.is_empty() {
                                        let mut cached = cache.write().await;
                                        if let Some(cur) = cached.get_entry(&t, &a) {
                                            if entry.lyrics.is_empty() { entry.lyrics = cur.lyrics.clone(); }
                                            if entry.qrc_data.is_empty() { entry.qrc_data = cur.qrc_data.clone(); }
                                            if entry.trans.is_empty() { entry.trans = cur.trans.clone(); }
                                        }
                                    }

                                    // 复用本地缓存条目中的专辑名（优于依赖 SMTC album）
                                    if entry.local_album.is_empty() {
                                        let mut cached = cache.write().await;
                                        if let Some(cur) = cached.get_entry(&t, &a) {
                                            entry.local_album = cur.local_album.clone();
                                        }
                                    }

                                    // 汇总在线结果
                                    if !quiet {
                                        let has_lyric = !entry.lyrics.is_empty() || !entry.qrc_data.is_empty();
                                        let art_label = if entry.album_pic_url.starts_with("data:") {
                                            "本地"
                                        } else if !entry.album_pic_url.is_empty() {
                                            "在线"
                                        } else {
                                            "无"
                                        };
                                        eprintln!("[歌词] {} - {} | 歌词: {} | 封面: {}",
                                            a, t,
                                            if has_lyric { "在线" } else { "无" },
                                            art_label);
                                    }

                                    entry.source = LyricsSource::Online;
                                    let mut w = cache.write().await;
                                    w.insert_entry(&t, &a, entry);
                                    if debug { eprintln!("[lyrics] ✓ Background fetch complete for '{} - {}'", t, a); }
                                }
                                Err(e) => {
                                    if !quiet {
                                        eprintln!("[歌词] {} - {} | 歌词: 无 | 封面: 无 (失败,稍后重试)", a, t);
                                    }
                                    if debug { eprintln!("[lyrics] 在线获取失败: {}", e); }
                                    let mut cached = cache.write().await;
                                    cached.mark_retry(&t, &a);
                                }
                            }
                            } else if !has_local_qrc {
                                // 在线歌词已停用：本地无歌词时把条目标记 Ready（空歌词），
                                // 前端显示「无歌词/纯音乐」占位；本地重查轮询随后命中会覆盖。
                                let mut cached = cache.write().await;
                                if let Some(entry) = cached
                                    .entries
                                    .get_mut(&format!("{}|{}", t, a))
                                {
                                    entry.fetch_state = LyricsFetchState::Ready;
                                }
                                if debug {
                                    eprintln!(
                                        "[lyrics] 在线歌词已停用，本地无歌词: '{} - {}'",
                                        t, a
                                    );
                                }
                            } else {
                                // 本地有 QRC 但缺少封面：解析封面 mid → 优先查本地 QQMusicPicture → 在线 CDN 兜底
                                //
                                // 1) 取封面 mid：
                                //    a. 多策略搜歌曲 songmid → get_album_cover（解决 "(Explicit)" 等括号后缀
                                //       导致单策略搜不到的问题，与 fetch_lyrics 策略对齐）；
                                //       有可对照专辑名时校验一致性，防止匹配到同名翻唱/合辑版本
                                //    b. 失败/被拒则用专辑名搜 SmartBox 专辑（候选名逐条验证，
                                //       候选为本地文件名专辑名（QQ 音乐索引原始名，比 SMTC album
                                //       更可靠）与 SMTC album，均清洗去括号后再搜）
                                // 2) 有了封面 mid：本地命中→base64 data URI（与在线路径一致，
                                //    避免 file:// 在 webview 受限）；否则返回在线 CDN URL
                                //    （T002 专辑封面 / T062 无专辑歌曲的单曲封面）
                                let get_pic = async {
                                    // 已有 SMTC/本地封面 → 不做远程解析（远程仅最后手段）
                                    if has_local_art {
                                        return None;
                                    }
                                    // 从本地缓存条目取文件名解析的专辑名（QQ 音乐索引时的原始名，
                                    // 比 SMTC 报告的 album 更可靠，用作校验与专辑搜索的参照）
                                    let local_album = {
                                        let cached = cache.read().await;
                                        cached.peek_entry(&t, &a).map(|e| e.local_album.clone()).unwrap_or_default()
                                    };
                                    let mut cover: Option<lyrics::AlbumCover> = None;
                                    // 无专辑单曲(vs 封面)兜底：1a 命中的候选无专辑信息时先记住，
                                    // 等 1b/1c 按专辑名解析失败后再回退——避免 vs 单曲封面
                                    // 直接胜出、错失正确专辑（如 BIGBANG 一天一天：smartbox 命中
                                    // 无专辑单曲版，但正在播放的是 Stand Up 专辑版）。
                                    let mut single_fallback: Option<lyrics::AlbumCover> = None;

                                    // 1a) 多策略搜歌曲 → 歌曲详情取封面 mid。
                                    //     候选必须通过身份校验（歌手一致 + 有专辑时专辑名与
                                    //     本地文件名/SMTC 一致），防止标题模糊匹配命中别的歌
                                    //     （如 "STAY (Explicit)" 命中 "Stay explicit" 的翻唱，
                                    //     其 vs 单曲封面是错歌的封面）。
                                    if let Some(songmid) = fetcher.search_song_mid(&t, &a).await {
                                        if let Ok(c) = fetcher.get_album_cover(&songmid).await {
                                            let refs: Vec<&str> = [local_album.as_str(), album.as_str()]
                                                .into_iter()
                                                .filter(|s| !s.is_empty())
                                                .collect();
                                            if song_match_ok(&c, &a, &refs) {
                                                if c.kind == lyrics::CoverKind::Single && !refs.is_empty() {
                                                    // 候选无专辑(vs 单曲封面)但播放中歌曲有专辑名可对照：
                                                    // 多半是不同版本，降级为兜底，优先按专辑名解析正确专辑
                                                    if debug { eprintln!("[lyrics] 候选为无专辑单曲(vs封面), 优先按专辑名解析"); }
                                                    single_fallback = Some(c);
                                                } else {
                                                    cover = Some(c);
                                                }
                                            } else if debug {
                                                eprintln!("[lyrics] 封面: 歌曲身份不符('{}' - '{}'), 尝试专辑名搜索", c.singer, c.name);
                                            }
                                        }
                                    }

                                    // 1b) 歌曲搜索失败或被校验拒绝 → 用专辑名搜专辑（候选：本地文件名
                                    //     专辑 + SMTC album，各自清洗去括号后作为额外候选）
                                    let mut album_variants: Vec<String> = Vec::new();
                                    for name in [local_album.clone(), album.clone()].into_iter().filter(|s| !s.is_empty()) {
                                        let cleaned = clean_album_name(&name);
                                        album_variants.push(cleaned.clone());
                                        if !cleaned.is_empty() && cleaned != name {
                                            album_variants.push(name);
                                        }
                                    }
                                    if cover.is_none() {
                                        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                                        for variant in album_variants.iter() {
                                            let v = variant.trim().to_string();
                                            if v.is_empty() || !seen.insert(v.clone()) { continue; }
                                            if let Some(am) = fetcher.search_album_mid_by_name(&v, &a).await {
                                                cover = Some(lyrics::AlbumCover { mid: am, name: String::new(), singer: String::new(), kind: lyrics::CoverKind::Album });
                                                break;
                                            }
                                        }
                                    }

                                    // 1c) SmartBox 专辑搜索索引不到的专辑（实测 "The Eminem Show" 返回 0 条）：
                                    //     legacy 歌曲搜索按专辑名找候选歌（结果自带专辑名，可与参考专辑
                                    //     校验），命中后走歌曲详情取封面，同样过身份校验
                                    if cover.is_none() && !album_variants.is_empty() {
                                        let ref_name = if !local_album.is_empty() { local_album.as_str() } else { album.as_str() };
                                        if let Some(songmid) = fetcher.find_song_by_album_name(&album_variants, &t, &a, ref_name).await {
                                            if let Ok(c) = fetcher.get_album_cover(&songmid).await {
                                                let refs: Vec<&str> = [local_album.as_str(), album.as_str()]
                                                    .into_iter()
                                                    .filter(|s| !s.is_empty())
                                                    .collect();
                                                if song_match_ok(&c, &a, &refs) {
                                                    if debug { eprintln!("[lyrics] 封面: legacy 专辑搜索命中 '{}'", c.name); }
                                                    cover = Some(c);
                                                }
                                            }
                                        }
                                    }

                                    // 1d) 专辑名解析均未命中 → 回退 1a 的无专辑单曲(vs)封面兜底
                                    if cover.is_none() {
                                        cover = single_fallback;
                                    }

                                    // 2) 封面 mid → 本地图片优先（T002 专辑 / T062 单曲），否则在线 URL。
                                    //    返回 (主图, 高清升级地址)：本地仅 150x150 时主图用低清 data URI
                                    //    即时显示，高清 800x800 在线地址交给前端预加载后平滑升级；
                                    //    本地 500x500 或直接在线 URL 时主图即高清，无升级地址。
                                    if let Some(c) = cover {
                                        if let Some(pic_dir) = get_cache_root().map(|r| r.join("QQMusicPicture")) {
                                            if let Some(local_pic) = local_qrc::find_album_pic(&pic_dir, &c.mid) {
                                                if let Ok(data) = std::fs::read(&local_pic) {
                                                    use base64::{Engine as _, engine::general_purpose::STANDARD};
                                                    let data_uri = format!("data:image/jpeg;base64,{}", STANDARD.encode(&data));
                                                    let is_hd = album_pic_is_hd(&local_pic);
                                                    if debug { eprintln!("[lyrics] 封面: ✓ 本地{}", if is_hd { "" } else { " (低清,将升级高清)" }); }
                                                    let hd_url = if is_hd {
                                                        String::new()
                                                    } else {
                                                        cover_cdn_url(c.kind, &c.mid)
                                                    };
                                                    return Some((data_uri, hd_url));
                                                }
                                            }
                                        }
                                        let pic_url = cover_cdn_url(c.kind, &c.mid);
                                        if debug { eprintln!("[lyrics] 封面: ✓ 在线"); }
                                        return Some((pic_url, String::new()));
                                    }
                                    None
                                };

                                if let Some((pic_url, hd_url)) = get_pic.await {
                                    let art_label = if pic_url.starts_with("data:") { "本地" } else { "在线" };
                                    let mut w = cache.write().await;
                                    w.update_album_pic_url(&t, &a, pic_url, hd_url);
                                    if !quiet {
                                        eprintln!("[歌词] {} - {} | 歌词: 本地 | 封面: {}", a, t, art_label);
                                    }
                                } else if !quiet {
                                    eprintln!("[歌词] {} - {} | 歌词: 本地 | 封面: 无", a, t);
                                }
                            }
                        });
                    } else {
                        if !quiet && song_key != last_logged_lyric_key {
                            let (lyric_label, art_label) = {
                                let cached = lyrics_cache.read().await;
                                match cached.peek_entry(&info.title, &info.artist) {
                                    Some(e) => {
                                        let l = match e.source {
                                            LyricsSource::Local => "本地",
                                            LyricsSource::Online => "在线",
                                            LyricsSource::None => "无",
                                        };
                                        let a = if e.album_pic_url.starts_with("data:") {
                                            "本地"
                                        } else if !e.album_pic_url.is_empty() {
                                            "在线"
                                        } else {
                                            "无"
                                        };
                                        (l, a)
                                    }
                                    None => ("无", "无"),
                                }
                            };
                            eprintln!("[歌词] {} - {} | 歌词: {} | 封面: {} (缓存)",
                                info.artist, info.title, lyric_label, art_label);
                            last_logged_lyric_key = song_key;
                        }
                    }

                    // 从缓存中获取歌词 + QRC 数据
                    {
                        let mut cached = lyrics_cache.write().await;
                        if let Some(entry) = cached.get_entry(&info.title, &info.artist) {
                            info.lyrics = entry.lyrics.clone();
                            info.trans = entry.trans.clone();
                            info.qrc_raw = entry.qrc_raw.clone();
                            info.qrc_data = entry.qrc_data.clone();
                            if !entry.album_pic_url.is_empty() {
                                // 已有本地图（SMTC 缩略图 / 本地缓存图，data URI）时，
                                // 不让在线 CDN 图覆盖它——本地来源优先，远程仅最后手段。
                                // 但 entry 本身是本地图（data URI）时允许覆盖（可能是更高清版本）。
                                let info_is_local_art = info.album_pic_url.starts_with("data:image");
                                let entry_is_local_art = entry.album_pic_url.starts_with("data:image");
                                if !info_is_local_art || entry_is_local_art {
                                    info.album_pic_url = entry.album_pic_url.clone();
                                }
                            }
                            if !entry.album_pic_hd_url.is_empty() {
                                info.album_pic_hd_url = entry.album_pic_hd_url.clone();
                            }
                        }
                    }

                    if info.lyrics.is_empty() && !info.trans.is_empty() {
                        info.lyrics = info.trans.clone();
                    }

                    info
                } else {
                    SongInfo {
                        title: "No music playing".to_string(),
                        artist: String::new(),
                        album: String::new(),
                        lyrics: String::new(),
                        trans: String::new(),
                        qrc_raw: String::new(),
                        qrc_data: Vec::new(),
                        current_time: 0,
                        total_time: 0,
                        current_time_ms: 0,
                        total_time_ms: 0,
                        progress_percent: 0.0,
                        is_playing: false,
                        album_pic_url: String::new(),
                        album_pic_hd_url: String::new(),
                        server_ts: 0,
                    }
                }
            },
            Err(e) => {
                if !args.quiet {
                    eprintln!("Error reading SMTC info: {}", e);
                }
                SongInfo {
                    title: "ERROR".to_string(),
                    artist: String::new(),
                    album: String::new(),
                    lyrics: format!("System Media Control Error: {}", e),
                    trans: String::new(),
                    qrc_raw: String::new(),
                    qrc_data: Vec::new(),
                    current_time: 0,
                    total_time: 0,
                    current_time_ms: 0,
                    total_time_ms: 0,
                    progress_percent: 0.0,
                    is_playing: false,
                    album_pic_url: String::new(),
                    album_pic_hd_url: String::new(),
                    server_ts: 0,
                }
            }
        };

        // 检查歌曲是否有变化 (用于切歌修正与文件输出)
        let song_changed = match &last_song_state {
            Some(last) => last.title != current_song_info.title || last.artist != current_song_info.artist,
            None => true,
        };

        // SMTC 切歌瞬间修正：media properties（title/artist）先更新，
        // timeline（Position/EndTime）可能滞后 1~2 秒仍报旧歌数据。
        //
        // 只靠 song_changed 一帧归零不够：后续帧 title 未变但 timeline 仍旧，
        // 前端会收到"新歌名 + 旧进度"错配——旧歌词继续渲染，进度来回跳。
        //
        // 解法：用上一首歌的 total_time_ms 做锚点——若当前 total 与旧歌
        // 相同（±500ms），判定 timeline 未更新，强制归零。total 变化后清除追踪。
        if song_changed {
            if let Some(old) = &last_song_state {
                if old.total_time_ms > 0 {
                    stale_song_total = Some(old.total_time_ms);
                }
            }
            current_song_info.current_time_ms = 0;
            current_song_info.current_time = 0;
            current_song_info.progress_percent = 0.0;
            current_song_info.total_time_ms = 0;
            current_song_info.total_time = 0;
        } else if let Some(old_total) = stale_song_total {
            if current_song_info.total_time_ms == 0
                || current_song_info.total_time_ms == old_total
            {
                current_song_info.current_time_ms = 0;
                current_song_info.current_time = 0;
                current_song_info.progress_percent = 0.0;
                current_song_info.total_time_ms = 0;
                current_song_info.total_time = 0;
            } else {
                stale_song_total = None;
            }
        }

        // —— WASAPI 音频会话门控：用「是否真实出声」修正 SMTC 声称的状态 ——
        // - 播放中持续静音：判定停滞，冻结在当前位置（QQ 恢复后锚定修正）
        // - 会话停止流但 SMTC 未报：提前按暂停显示
        // 恢复播放的位置冻结在 smtc.rs 的 resume hold 里处理（感知锚定状态）。
        // 找不到 QQ 音频会话时 AudioGate 内部回退为透传。
        if song_changed {
            audio_gate.reset();
        }
        // 暂停淡出窗口内由 smtc 的淡出状态机全权处理 is_playing，
        // 门控的 inactive 判定会在淡出收尾前抢先把状态翻成暂停，
        // 与仍在前进的淡出报进打架（前端「已暂停但位置还在走」）
        if current_song_info.is_valid() && !smtc::pause_fade_active().await {
            match audio_gate.update(current_song_info.is_playing) {
                audio_session::GateDecision::Pass => {}
                audio_session::GateDecision::ForcePause => {
                    current_song_info.is_playing = false;
                }
            }
        }

        // SMTC positions are drift-corrected via LastUpdatedTime in smtc.rs.
        // Apply the user-configurable offset for fine-tuning.
        // 注意：playing 和 pause 都加 offset，避免 play→pause 切换瞬间 display 因
        // offset 消失而回退（~200ms），造成歌词行瞬间变灰再回正的闪烁。
        // 例外：切歌后 timeline 归零窗口（total==0）内不套偏移——占位值保持 0，
        // 否则前端插值基线从 200ms 起跑，timeline 就绪时被拉回真实位置
        // （回跳近 500ms，叠加前端"淡出尾音"钉回逻辑 = 歌词开头疯狂闪烁）。
        let smtc_offset_ms = config.settings.smtc_offset_ms;
        let adjusted = if current_song_info.total_time_ms > 0 {
            apply_signed_offset(current_song_info.current_time_ms, smtc_offset_ms)
        } else {
            current_song_info.current_time_ms
        };
        let display_time_ms = if current_song_info.total_time_ms > 0 {
            adjusted.min(current_song_info.total_time_ms)
        } else {
            adjusted
        };

        current_song_info.current_time_ms = display_time_ms;
        current_song_info.current_time = display_time_ms / 1000;
        if current_song_info.total_time_ms > 0 {
            current_song_info.progress_percent =
                (display_time_ms as f64 / current_song_info.total_time_ms as f64 * 100.0) as f32;
        }

        let current_song_info = Arc::new(current_song_info);

        if let Some(latest) = LATEST_SONG_INFO.get() {
            *latest.write().await = Arc::clone(&current_song_info);
        }

        // WebSocket/API 保持完整 SongInfo 协议；watch 通道仅克隆 Arc，
        // 避免每个轮询周期深拷贝整份逐字歌词。
        let _ = tx.send(Arc::clone(&current_song_info));

        // Tauri 主通道只在静态内容变化时发送完整快照；普通轮询仅发送
        // 数十字节的进度事件，前端仍按原轮询频率接收并进行 60fps 插值。
        //
        // 后台托盘模式（GUI_VISIBLE=false 且状态栏歌词未启用）：跳过 GUI
        // 事件推送，省去 IPC 与序列化开销。歌词条启用时保持推送（其窗口
        // 独立于主窗口显示）。API（watch 通道/HTTP）不受影响，仍每轮发送。
        if let Some(ref handle) = app_handle {
            use tauri::Emitter;
            let gui_needed = GUI_VISIBLE.load(Ordering::SeqCst)
                || LYRIC_BAR_ENABLED.load(Ordering::SeqCst);
            if gui_needed {
                let static_changed = last_gui_snapshot
                    .as_ref()
                    .map(|previous| !same_gui_payload(previous, current_song_info.as_ref()))
                    .unwrap_or(true);

                if static_changed {
                    if handle.emit("song-info", current_song_info.as_ref()).is_ok() {
                        last_gui_snapshot = Some(current_song_info.as_ref().clone());
                    }
                } else {
                    let progress = PlaybackProgress::from(current_song_info.as_ref());
                    let _ = handle.emit("playback-progress", progress);
                }
            } else if last_gui_snapshot.is_some() {
                // 后台期间不更新快照；恢复可见后第一轮强制发全量
                last_gui_snapshot = None;
            }
        }

        // 写入文件
        last_song_state = Some(LastSongState::from(current_song_info.as_ref()));

            Ok(())
        }.await;

        if let Err(e) = loop_result {
            if config.settings.debug_mode && !args.quiet {
                eprintln!("Critical loop error: {}", e);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        } else {
            let elapsed = loop_start.elapsed();
            // 后台省电：主窗口隐藏、状态栏歌词未启用且同步服务关闭时降频
            // 轮询（无任何实时消费方）。歌词条或同步服务（WebSocket/HTTP
            // API）启用时绝不降频；SMTC 状态变化事件仍会在降频期间立即
            // 唤醒处理。
            let backend_interval = config.settings.update_interval_ms;
            let current_interval = if !GUI_VISIBLE.load(Ordering::SeqCst)
                && !LYRIC_BAR_ENABLED.load(Ordering::SeqCst)
                && !config.settings.enable_server
            {
                Duration::from_millis(backend_interval.max(1000))
            } else {
                Duration::from_millis(backend_interval)
            };
            if elapsed < current_interval {
                // 除周期 tick 外，SMTC 事件（暂停/恢复/seek/切歌）随时
                // 唤醒主循环做下一轮，把状态切换延迟从轮询周期压到 ~10ms 级
                let state_notify = smtc::state_changed_notify();
                tokio::select! {
                    _ = tokio::time::sleep(current_interval - elapsed) => {}
                    _ = state_notify.notified() => {}
                }
            }
        }
    }


    Ok(())
}

/// 应用命令行参数覆盖配置
fn apply_cli_overrides(mut config: Config, args: &Cli) -> Config {
    if args.debug {
        config.settings.debug_mode = true;
    }
    
    // CLI --interval 如果指定，则覆盖配置文件
    if let Some(interval) = args.interval {
        config.settings.update_interval_ms = interval;
    }

    // CLI --offset 如果指定，则覆盖配置文件
    if let Some(offset) = args.offset {
        config.settings.smtc_offset_ms = offset;
    }
    
    if args.no_server {
        config.settings.enable_server = false;
    }

    if args.port != 3000 {
        config.settings.server_port = args.port;
    }

    if args.retries != 3 {
        config.settings.max_retries = args.retries;
    }

    
    config
}

/// 在字符索引处拆分字符串（支持中文字符）
fn split_str_at_char(s: &str, char_idx: usize) -> (&str, &str) {
    for (ci, (i, _)) in s.char_indices().enumerate() {
        if ci == char_idx {
            return (&s[..i], &s[i..]);
        }
    }
    (s, "")
}

/// 渲染带逐字进度高亮的 QRC 歌词
fn render_qrc_line(line: &QrcLine, current_time_ms: u64, debug: bool) -> String {
    use crossterm::style::Stylize;

    // 计算行的有效结束时间：取行时长终点与每个字终点中的最大值，防止行时长小于字时长导致提前截断
    let line_end_ms = {
        let dur_end = if line.duration_ms > 0 {
            line.start_time_ms + line.duration_ms
        } else {
            0
        };
        let word_end = line.words.last()
            .map(|w| w.start_time_ms + w.duration_ms)
            .unwrap_or(0);
        let end = dur_end.max(word_end);
        if end > 0 { end } else { line.start_time_ms + 5000 }
    };

    if debug {
        eprintln!("[render] cur={} line_start={} line_end={} line_dur={} words={} content={:?}",
            current_time_ms, line.start_time_ms, line_end_ms, line.duration_ms,
            line.words.len(), line.content);
    }

    // 如果还没唱到这行，全灰
    if current_time_ms < line.start_time_ms {
        if debug { eprintln!("[render] -> BEFORE (dark_grey)"); }
        return line.content.clone().dark_grey().to_string();
    }

    // 如果这行已经唱完了，全黄
    if current_time_ms >= line_end_ms {
        if debug { eprintln!("[render] -> AFTER (yellow bold)"); }
        return line.content.clone().yellow().bold().to_string();
    }

    // 没有逐字数据时，按行整体进度显示
    if line.words.is_empty() {
        let progress = ((current_time_ms - line.start_time_ms) as f64
            / (line_end_ms - line.start_time_ms) as f64)
            .clamp(0.0, 1.0);
        let char_count = line.content.chars().count();
        let split_idx = ((char_count as f64) * progress).ceil() as usize;
        let (done, todo) = split_str_at_char(&line.content, split_idx);
        if debug { eprintln!("[render] -> EMPTY_WORDS progress={:.2}", progress); }
        return format!("{}{}", done.yellow().bold(), todo.dark_grey());
    }

    // 正在唱这一行：逐字渲染，当前字按进度分高低亮
    // word.start_time_ms 已是基于歌曲开头的绝对时间
    let mut result = String::new();
    for word in &line.words {
        let word_start_abs = word.start_time_ms;
        let word_dur = if word.duration_ms > 0 { word.duration_ms } else { 200 };
        let word_end_abs = word_start_abs + word_dur;

        if current_time_ms >= word_end_abs {
            if debug { eprintln!("[render]   word={:?} start={} end={} -> FINISHED", word.content, word_start_abs, word_end_abs); }
            // 这个字已经唱完，显示黄色
            result.push_str(&word.content.clone().yellow().bold().to_string());
        } else if current_time_ms >= word_start_abs && current_time_ms < word_end_abs {
            // 正在唱的字：按时间进度拆分为"已唱"和"未唱"两部分
            let progress = ((current_time_ms - word_start_abs) as f64 / word_dur as f64)
                .clamp(0.0, 1.0);
            let char_count = word.content.chars().count();
            let split_idx = ((char_count as f64) * progress).ceil() as usize;
            let (done, todo) = split_str_at_char(&word.content, split_idx);
            if debug { eprintln!("[render]   word={:?} start={} end={} -> IN_PROGRESS progress={:.2} split={}", word.content, word_start_abs, word_end_abs, progress, split_idx); }
            result.push_str(&done.yellow().bold().to_string());
            result.push_str(&todo.dark_grey().to_string());
        } else {
            if debug { eprintln!("[render]   word={:?} start={} end={} -> PENDING", word.content, word_start_abs, word_end_abs); }
            // 还没唱到的字，显示暗色
            result.push_str(&word.content.clone().dark_grey().to_string());
        }
    }

    result
}

/// 渲染内容并补空格到指定可见宽度
/// 解决 Stylize 的 ANSI 转义码被 `{:<w}` 计入宽度的问题
fn pad_line(text: &str, styled: String, width: usize) -> String {
    let visible = text.chars().count();
    if visible < width {
        format!("{}{}\n", styled, " ".repeat(width - visible))
    } else {
        format!("{}\n", styled)
    }
}

/// 对已包含 ANSI 转义码的字符串按可见宽度补空格
fn pad_styled(styled: &str, width: usize) -> String {
    // 粗略计算可见宽度：去掉 \x1b[... 转义序列后数可见字符
    // 不引入 regex 依赖，通过简单状态机跳过 ANSI 序列
    let mut visible = 0;
    let mut in_escape = false;
    for c in styled.chars() {
        if in_escape {
            if c == 'm' || c == 'H' { in_escape = false; }
            continue;
        }
        if c == '\x1b' { in_escape = true; continue; }
        visible += 1;
    }
    if visible < width {
        format!("{}{}", styled, " ".repeat(width - visible))
    } else {
        styled.to_string()
    }
}

/// 渲染内容并补空格到指定可见宽度
/// 解决 Stylize 的 ANSI 转义码被 `{:<w}` 计入宽度的问题
/// 恢复并聚焦主窗口（托盘单击 / 菜单"显示"共用）。
///
/// Windows 前台焦点锁：从托盘回调中 set_focus 常被系统拒绝
/// （后台进程抢前台限制），因此立即恢复一次，再延迟重试聚焦，
/// 确保窗口真正获得键盘焦点。
fn show_main_window(app: &tauri::AppHandle) {
    use tauri::Manager;
    // 恢复前台模式：主循环立即恢复全量 GUI 推送与正常轮询频率
    GUI_VISIBLE.store(true, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        use tauri::Manager;
        std::thread::sleep(std::time::Duration::from_millis(80));
        if let Some(w) = handle.get_webview_window("main") {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    });
}

#[tokio::main]
// Function to launch the Tauri GUI and background monitor loop.
async fn main() -> Result<()> {
    // 解析命令行参数
    let args = Cli::parse_args();
    
    // 处理帮助和版本信息
    if args.help {
        Cli::show_help();
        return Ok(());
    }
    
    if args.version {
        Cli::show_version();
        return Ok(());
    }

    // 加载配置
    let config = Config::get_config();
    
    // 应用命令行参数覆盖配置
    let config = apply_cli_overrides(config, &args);

    // Initialize global config lock
    CONFIG.set(RwLock::new(config.clone()))
        .map_err(|_| anyhow::anyhow!("Failed to initialize global config OnceLock"))?;
    LATEST_SONG_INFO
        .set(RwLock::new(Arc::new(SongInfo::default())))
        .map_err(|_| anyhow::anyhow!("Failed to initialize latest song state"))?;

    let monitor_args = args.clone();
    let monitor_config = config.clone();

    // 启动 Tauri 窗口
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_app_config, save_app_config, get_current_song_info, media_toggle_play_pause, media_next, media_previous, window_hidden, set_lyric_bar_enabled, set_lyric_bar_side, set_lyric_bar_gap, set_lyric_bar_width, set_lyric_bar_width_preview, get_taskbar_light_theme, get_frontend_prefs, save_frontend_prefs, hide_mini_player])
        .setup(move |app| {
            // —— 系统托盘（后台模式）——
            // 窗口最小化/隐藏到托盘后，托盘小图标保留在任务栏通知区域，
            // 单击托盘图标或菜单"显示"恢复窗口，"退出"结束整个应用。
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
            use tauri::Manager;

            let show_item = MenuItem::with_id(app, "show", "显示 QQMusic Monitor", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().expect("missing default window icon").clone())
                .tooltip("QQMusic Monitor")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // 左键单击恢复窗口；右键交给系统弹出菜单
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            // 状态栏歌词条右键菜单（鼠标穿透窗口无法直接接收右键，用低级鼠标钩子）
            spawn_lyric_bar_menu_hooks(app.handle().clone());

            // 主窗口关闭 = 退出整个应用（同时销毁歌词条，避免其阻止进程退出）
            if let Some(main_win) = app.get_webview_window("main") {
                let handle = app.handle().clone();
                main_win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        handle.exit(0);
                    }
                });
            }

            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("Failed to build tokio runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async {
                    if let Err(e) = run_monitor(Some(app_handle), monitor_args, monitor_config).await {
                        eprintln!("Monitor loop error: {:?}", e);
                    }
                });
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    Ok(())
}

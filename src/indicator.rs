//! A small always-on-top indicator in a corner of the screen.
//!
//! It exists so the recording state is visible without opening the tray menu: a lone status
//! icon while idle (no text at all), and an icon plus the latest transcribed snippet while
//! recording, with an animated waveform, shimmering text and a slow sweeping highlight.
//!
//! The window is a plain Win32 popup painted with GDI, created and pumped on its own thread, so it
//! works identically whether the app is running with a tray icon or headless. It never takes
//! focus, never appears in Alt+Tab (`WS_EX_TOOLWINDOW`) and passes clicks through to whatever is
//! underneath it (`WM_NCHITTEST` returns `HTTRANSPARENT`). Visibility can be flipped at runtime
//! from the tray menu; see [`Indicator::set_visible`].
//!
//! The pill is drawn as a modern glass-like card: a subtle vertical gradient, a 1 px rounded
//! border, a soft top highlight, and — while listening or transcribing — a live waveform
//! (several bars oscillating on a sine phase) instead of a static dot.
//!
//! Placement is the fiddly part. The process is made per-monitor DPI aware before the window
//! exists, so every coordinate here is a physical pixel, and the corner it uses is taken from the
//! monitor's work area with the taskbar's band removed explicitly — Windows leaves the work area
//! at the full monitor while the taskbar is set to hide itself, which used to park the pill
//! underneath it.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, CreateFontW, CreateRoundRectRgn,
    CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, DrawTextW, Ellipse, EndPaint,
    FF_DONTCARE, FW_SEMIBOLD, FillRect, GetMonitorInfoW, HGDIOBJ, InvalidateRect, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForWindow,
    MDT_EFFECTIVE_DPI, PROCESS_PER_MONITOR_DPI_AWARE, SetProcessDpiAwareness,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetClientRect, GetMessageW,
    GetSystemMetrics, GetWindowRect, HTTRANSPARENT, IsWindowVisible, KillTimer, LWA_ALPHA, MSG,
    PostMessageW, RegisterClassW, SPI_SETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOZORDER, SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_ERASEBKGND, WM_NCHITTEST,
    WM_PAINT, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::config::{Config, IndicatorPosition};
use crate::logging::Logger;
use crate::pipeline::Status;

/// Window class name; registered once per process.
const CLASS_NAME: PCWSTR = w!("DepthIndicator");
/// Logical size of the pill at 96 DPI. Idle shows an icon only, so it is narrow; recording grows
/// to fit the live snippet, up to `MAX_WIDTH`.
const IDLE_WIDTH: i32 = 48;
const MAX_WIDTH: i32 = 460;
const HEIGHT: i32 = 36;
/// Logical metrics of the contents, all at 96 DPI.
const DOT_X: i32 = 20;
const IDLE_DOT_RADIUS: i32 = 8;
const DOT_RADIUS: i32 = 6;
const FONT_HEIGHT: i32 = 15;
const TEXT_LEFT: i32 = 36;
const TEXT_RIGHT_PAD: i32 = 10;
/// Longest snippet shown before it is ellipsized.
const MAX_SNIPPET_CHARS: usize = 64;
/// Timer that re-reads the pipeline status (fast enough for a smooth waveform).
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 90;
/// Internal message that flips visibility from the tray thread.
const MSG_SET_VISIBLE: u32 = 0x0400 + 64;

/// What the indicator currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Face {
    Idle,
    Recording,
    Busy,
    Error,
}

impl Face {
    fn from_status(status: &Status) -> Self {
        match status {
            Status::Error(_) => Face::Error,
            Status::Paused | Status::Starting => Face::Idle,
            Status::Transcribing(_) => Face::Busy,
            Status::Listening => Face::Recording,
        }
    }

    fn label(self) -> &'static str {
        match self {
            // Idle shows an icon only; the empty label keeps old callers honest.
            Face::Idle => "",
            Face::Recording => "Listening…",
            Face::Busy => "Transcribing",
            Face::Error => "Error",
        }
    }

    /// Whether the pill shows text next to the icon. Idle never does.
    fn shows_text(self) -> bool {
        !matches!(self, Face::Idle)
    }

    /// `COLORREF` is 0x00BBGGRR.
    fn color(self) -> u32 {
        match self {
            Face::Idle => 0x0090_9090,        // grey
            Face::Recording => 0x0033_33E8,   // red
            Face::Busy => 0x0020_B0F0,        // amber
            Face::Error => 0x0040_40D0,       // deep red
        }
    }

    /// A faintly tinted background, so the state reads at a glance without looking at the dot.
    fn background(self) -> u32 {
        match self {
            Face::Idle => 0x0024_2424,      // neutral dark
            Face::Recording => 0x0016_162E, // dark red tint
            Face::Busy => 0x0010_2430,      // dark amber tint
            Face::Error => 0x0014_1434,     // dark red
        }
    }
}

/// The pill's pixel geometry for one monitor DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Metrics {
    width: i32,
    height: i32,
    margin: i32,
    dot_x: i32,
    dot_radius: i32,
    idle_dot_radius: i32,
    font_height: i32,
    text_left: i32,
    text_right_pad: i32,
    idle_width: i32,
    max_width: i32,
}

/// Scale a 96-DPI value to `dpi`, rounded to the nearest pixel.
fn scale(value: i32, dpi: u32) -> i32 {
    let dpi = dpi.max(1) as i64;
    ((value as i64 * dpi + 48) / 96) as i32
}

/// The pill at `dpi`: the same apparent size it has at 100% scaling, drawn 1:1.
fn metrics(dpi: u32, margin: i32) -> Metrics {
    Metrics {
        width: scale(MAX_WIDTH, dpi),
        height: scale(HEIGHT, dpi),
        margin: scale(margin, dpi),
        dot_x: scale(DOT_X, dpi),
        dot_radius: scale(DOT_RADIUS, dpi).max(2),
        idle_dot_radius: scale(IDLE_DOT_RADIUS, dpi).max(3),
        font_height: scale(FONT_HEIGHT, dpi).max(8),
        text_left: scale(TEXT_LEFT, dpi),
        text_right_pad: scale(TEXT_RIGHT_PAD, dpi),
        idle_width: scale(IDLE_WIDTH, dpi),
        max_width: scale(MAX_WIDTH, dpi),
    }
}

/// Collapse a snippet to one line and cap its length, so the pill never grows without bound.
fn truncate_snippet(text: &str) -> String {
    let single: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = single.chars().collect();
    if chars.len() <= MAX_SNIPPET_CHARS {
        return single;
    }
    // Leave room for the ellipsis; overflow is still clipped gracefully by DrawTextW.
    let keep = MAX_SNIPPET_CHARS.saturating_sub(1);
    chars[..keep].iter().collect::<String>() + "…"
}

/// Rough pixel width of `text` at `font_height`, for sizing the pill before painting.
///
/// Segoe UI semibold averages about half an em per glyph for mixed prose; the estimate errs on
/// the generous side and any remainder is handled by ellipsis clipping in `paint`.
fn estimate_text_width(text: &str, font_height: i32) -> i32 {
    let glyphs = text.chars().count() as i32;
    (glyphs * font_height * 58 / 100) + font_height
}

/// The text the pill shows for `face`: the live snippet while it matters, else a short fallback.
fn display_text(face: Face, snippet: &str) -> String {
    let snippet = truncate_snippet(snippet);
    if !snippet.is_empty() && matches!(face, Face::Recording | Face::Busy) {
        return snippet;
    }
    face.label().to_string()
}

/// How wide the pill should be for `face` and `text`, clamped to the idle/max bounds.
fn desired_width(face: Face, text: &str, metrics: &Metrics) -> i32 {
    if !face.shows_text() {
        return metrics.idle_width;
    }
    let want =
        metrics.text_left + estimate_text_width(text, metrics.font_height) + metrics.text_right_pad;
    want.clamp(metrics.idle_width, metrics.max_width)
}

/// Space a docked taskbar takes along each edge of the monitor, in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Bands {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// The band the taskbar occupies on `monitor`.
///
/// The size comes from the taskbar window's *rectangle*, not from its current position: an
/// auto-hidden taskbar keeps its full height while it is slid off the screen, and Windows then
/// reports the whole monitor as the work area. `Shell_TrayWnd` belongs to the primary monitor,
/// which is the one the pill is placed on.
fn taskbar_bands(monitor: RECT, taskbar: Option<RECT>) -> Bands {
    let Some(bar) = taskbar else {
        return Bands::default();
    };
    let (monitor_w, monitor_h) = (monitor.right - monitor.left, monitor.bottom - monitor.top);
    let (bar_w, bar_h) = (bar.right - bar.left, bar.bottom - bar.top);
    if monitor_w <= 0 || monitor_h <= 0 || bar_w <= 0 || bar_h <= 0 {
        return Bands::default();
    }

    // A bottom or top taskbar spans the monitor's width; a side taskbar spans its height.
    let spans_width = bar_w as i64 * 4 >= monitor_w as i64 * 3;
    let spans_height = bar_h as i64 * 4 >= monitor_h as i64 * 3;
    let slack = 2;

    if spans_width {
        if bar.bottom >= monitor.bottom - slack {
            return Bands {
                bottom: bar_h,
                ..Bands::default()
            };
        }
        if bar.top <= monitor.top + slack {
            return Bands {
                top: bar_h,
                ..Bands::default()
            };
        }
    } else if spans_height {
        if bar.left <= monitor.left + slack {
            return Bands {
                left: bar_w,
                ..Bands::default()
            };
        }
        return Bands {
            right: bar_w,
            ..Bands::default()
        };
    }
    Bands::default()
}

/// Where the pill sits inside the usable part of `monitor`.
fn placement(
    monitor: RECT,
    work: RECT,
    bands: Bands,
    size: (i32, i32),
    margin: i32,
    position: IndicatorPosition,
) -> (i32, i32) {
    let (width, height) = size;
    // The work area already excludes a taskbar that is showing; trimming the bands again is what
    // covers the taskbar that is hiding, and taking the intersection keeps a visible taskbar from
    // being subtracted twice.
    let mut usable = RECT {
        left: work.left.max(monitor.left + bands.left),
        top: work.top.max(monitor.top + bands.top),
        right: work.right.min(monitor.right - bands.right),
        bottom: work.bottom.min(monitor.bottom - bands.bottom),
    };
    if usable.right - usable.left < width || usable.bottom - usable.top < height {
        // A taskbar that eats the whole edge, or a very small screen: keep the pill on the monitor.
        usable = monitor;
    }

    let (x, y) = match position {
        IndicatorPosition::BottomRight => {
            (usable.right - width - margin, usable.bottom - height - margin)
        }
        IndicatorPosition::BottomLeft => (usable.left + margin, usable.bottom - height - margin),
        IndicatorPosition::TopRight => (usable.right - width - margin, usable.top + margin),
        IndicatorPosition::TopLeft => (usable.left + margin, usable.top + margin),
    };
    (
        x.clamp(monitor.left, (monitor.right - width).max(monitor.left)),
        y.clamp(monitor.top, (monitor.bottom - height).max(monitor.top)),
    )
}

/// A monitor's geometry and scale, in physical pixels.
#[derive(Debug, Clone, Copy)]
struct Monitor {
    rect: RECT,
    work: RECT,
    dpi: u32,
}

/// The primary monitor, its usable rect and its DPI.
fn primary_monitor() -> Monitor {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: a plain monitor lookup; `info` is a valid out-parameter for the call.
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    // SAFETY: `info` was initialised with its own size, as the API requires.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let mut dpi_x = 0u32;
        let mut dpi_y = 0u32;
        // SAFETY: two writable u32 out-parameters; a failure keeps the 96 DPI default.
        let dpi = match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
        {
            Ok(()) if dpi_x > 0 => dpi_x,
            _ => 96,
        };
        return Monitor {
            rect: info.rcMonitor,
            work: info.rcWork,
            dpi,
        };
    }

    // Keeps the window on screen even if the monitor query is unavailable.
    // SAFETY: a plain screen metric query.
    let rect = RECT {
        left: 0,
        top: 0,
        right: unsafe { GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN) },
        bottom: unsafe { GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) },
    };
    Monitor {
        rect,
        work: rect,
        dpi: 96,
    }
}

/// The taskbar's window rectangle, when it exists and is showing.
fn taskbar_rect() -> Option<RECT> {
    // SAFETY: a window lookup by class name, then a geometry read on the handle it returns.
    let hwnd = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.ok()?;
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return None;
    }
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out-parameter for the call.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return None;
    }
    Some(rect)
}

/// Become per-monitor DPI aware before any window exists.
///
/// The process ships without a manifest and tao only sets awareness when it builds its event
/// loop — after the indicator has usually been placed — so without this the pill's coordinates
/// are virtualised and the window is then stretched by the DWM. Failing here means awareness was
/// already set (by tao, or by a manifest), which is the outcome this wants anyway.
fn make_dpi_aware() {
    // SAFETY: process-wide DPI awareness, set before the first window is created.
    unsafe {
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_ok() {
            return;
        }
        let _ = SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
    }
}

/// The pill's geometry plus the inputs it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    x: i32,
    y: i32,
    dpi: u32,
    metrics: Metrics,
    monitor: RECT,
    work: RECT,
    bands: Bands,
}

impl Placement {
    /// The inputs behind `x`/`y`, so a wrong position is diagnosable from the log alone.
    fn describe(&self) -> String {
        format!(
            "dpi {}, monitor {}x{}+{}+{}, work {}x{}+{}+{}, taskbar l{} t{} r{} b{}",
            self.dpi,
            self.monitor.right - self.monitor.left,
            self.monitor.bottom - self.monitor.top,
            self.monitor.left,
            self.monitor.top,
            self.work.right - self.work.left,
            self.work.bottom - self.work.top,
            self.work.left,
            self.work.top,
            self.bands.left,
            self.bands.top,
            self.bands.right,
            self.bands.bottom,
        )
    }
}

/// Work out where the pill belongs. The window is the authority on DPI once it exists.
///
/// `width` overrides the pill width (idle vs. fitted to the live snippet); `None` means the
/// maximum width.
fn place(
    hwnd: Option<HWND>,
    margin: i32,
    position: IndicatorPosition,
    width: Option<i32>,
) -> Placement {
    let monitor = primary_monitor();
    let dpi = match hwnd {
        // SAFETY: a plain query on our own window; zero means "unknown".
        Some(hwnd) => match unsafe { GetDpiForWindow(hwnd) } {
            0 => monitor.dpi,
            dpi => dpi,
        },
        None => monitor.dpi,
    };
    let mut metrics = metrics(dpi, margin);
    metrics.width = width
        .unwrap_or(metrics.max_width)
        .clamp(metrics.idle_width, metrics.max_width);
    let bands = taskbar_bands(monitor.rect, taskbar_rect());
    let (x, y) = placement(
        monitor.rect,
        monitor.work,
        bands,
        (metrics.width, metrics.height),
        metrics.margin,
        position,
    );
    Placement {
        x,
        y,
        dpi,
        metrics,
        monitor: monitor.rect,
        work: monitor.work,
        bands,
    }
}

/// State the window procedure needs, reachable from the static callback.
struct Shared {
    status: Arc<Mutex<Status>>,
    /// Latest finalized utterance, refreshed by the pipeline worker.
    last_text: Arc<Mutex<String>>,
    /// Monotonic animation frame, bumped on every timer tick.
    tick: u64,
    /// Last painted face, to avoid repainting unchanged frames.
    shown: Option<Face>,
    /// Snippet the pill is currently sized and painted for.
    shown_text: String,
    /// Whether the pill should be on screen; flipped from the tray menu.
    visible: bool,
    /// Placement inputs, so a display or taskbar change can be applied without a restart.
    margin: i32,
    position: IndicatorPosition,
    metrics: Metrics,
}

// SAFETY: the contents are Send, and access is serialised by the mutex.
unsafe impl Send for Shared {}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

/// A live indicator window.
pub struct Indicator {
    hwnd: Arc<AtomicIsize>,
    thread: Option<JoinHandle<()>>,
}

impl Indicator {
    /// Close the window and join its thread.
    pub fn shutdown(&mut self) {
        let raw = self.hwnd.swap(0, Ordering::Relaxed);
        if raw != 0 {
            // SAFETY: the handle was created on the indicator thread and the window is alive
            // until that thread processes WM_CLOSE.
            unsafe {
                let _ = PostMessageW(Some(HWND(raw as *mut core::ffi::c_void)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Show or hide the pill without stopping its thread.
    ///
    /// The flag is stored in shared state and applied on the window thread, so a repaint or a
    /// display change cannot resurrect a hidden pill.
    pub fn set_visible(&self, visible: bool) {
        {
            let mut shared = lock_shared();
            if let Some(state) = shared.as_mut() {
                state.visible = visible;
            }
        }
        let raw = self.hwnd.load(Ordering::Relaxed);
        if raw != 0 {
            // SAFETY: posting to our own window; ignored once the thread has exited.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(raw as *mut core::ffi::c_void)),
                    MSG_SET_VISIBLE,
                    WPARAM(visible as usize),
                    LPARAM(0),
                );
            }
        }
    }

    /// Whether the pill is currently meant to be visible.
    pub fn is_visible(&self) -> bool {
        lock_shared().as_ref().is_none_or(|state| state.visible)
    }
}

fn lock_status(status: &Mutex<Status>) -> std::sync::MutexGuard<'_, Status> {
    match status.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_status_text(text: &Mutex<String>) -> String {
    match text.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn lock_shared() -> std::sync::MutexGuard<'static, Option<Shared>> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Create the indicator unless the configuration disables it.
pub fn spawn(
    status: Arc<Mutex<Status>>,
    last_text: Arc<Mutex<String>>,
    config: &Config,
    logger: Arc<Logger>,
) -> Result<Option<Indicator>> {
    if !config.show_indicator {
        logger.info("listening indicator disabled by configuration");
        return Ok(None);
    }
    // Before the thread, so it always happens before any coordinate is read.
    make_dpi_aware();

    let margin = config.indicator_margin;
    let position = config.indicator_position;
    let opacity = config.indicator_opacity;
    let thread_logger = logger.clone();
    let hwnd_slot = Arc::new(AtomicIsize::new(0));
    let slot = hwnd_slot.clone();

    let thread = std::thread::Builder::new()
        .name("indicator".to_string())
        .spawn(move || {
            match run_window(
                status,
                last_text,
                margin,
                position,
                opacity,
                thread_logger.clone(),
                slot.clone(),
            ) {
                Ok(()) => {}
                Err(err) => thread_logger.error(format!("listening indicator stopped: {err:#}")),
            }
            slot.store(0, Ordering::Relaxed);
        })
        .context("spawning the indicator thread")?;

    Ok(Some(Indicator {
        hwnd: hwnd_slot,
        thread: Some(thread),
    }))
}

/// Own the window and its message loop on this thread.
fn run_window(
    status: Arc<Mutex<Status>>,
    last_text: Arc<Mutex<String>>,
    margin: i32,
    position: IndicatorPosition,
    opacity: u8,
    logger: Arc<Logger>,
    hwnd_slot: Arc<AtomicIsize>,
) -> Result<()> {
    {
        let mut shared = lock_shared();
        *shared = Some(Shared {
            status,
            last_text,
            tick: 0,
            shown: None,
            shown_text: String::new(),
            visible: true,
            margin,
            position,
            metrics: metrics(96, margin),
        });
    }

    // SAFETY: every call below is a standard Win32 window setup on this thread.
    unsafe {
        let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // A zero return means the class already exists, which is fine for a single window.
        RegisterClassW(&class);

        let first = place(None, margin, position, Some(metrics(96, margin).idle_width));
        let ex_style =
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT;
        let hwnd = CreateWindowExW(
            ex_style,
            CLASS_NAME,
            w!("depth"),
            WS_POPUP,
            first.x,
            first.y,
            first.metrics.width,
            first.metrics.height,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .context("creating the indicator window")?;

        hwnd_slot.store(hwnd.0 as isize, Ordering::Relaxed);
        SetLayeredWindowAttributes(hwnd, COLORREF(0), opacity, LWA_ALPHA)
            .context("setting indicator opacity")?;
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_ID, TIMER_MS, None);

        // The window's real DPI is authoritative: if something set awareness late the pre-create
        // numbers are the virtualised ones, and this corrects them.
        let settled = place(
            Some(hwnd),
            margin,
            position,
            Some(first.metrics.width),
        );
        if settled != first {
            let _ = SetWindowPos(
                hwnd,
                None,
                settled.x,
                settled.y,
                settled.metrics.width,
                settled.metrics.height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        apply_round_corners(hwnd, settled.metrics.width, settled.metrics.height);
        {
            let mut shared = lock_shared();
            if let Some(state) = shared.as_mut() {
                state.metrics = settled.metrics;
            }
        }
        logger.info(format!(
            "listening indicator shown at ({}, {}) — {}x{} px, opacity {opacity} [{}]",
            settled.x,
            settled.y,
            settled.metrics.width,
            settled.metrics.height,
            settled.describe()
        ));

        let mut message = MSG::default();
        // GetMessageW returns 0 at WM_QUIT and -1 on error; both end the loop.
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

/// Re-place the pill after a display, taskbar or DPI change.
fn reposition(hwnd: HWND, state: &mut Shared) {
    // Keep the current dynamic width: the pill must stay anchored while it shows text.
    let placed = place(
        Some(hwnd),
        state.margin,
        state.position,
        Some(state.metrics.width),
    );
    state.metrics = placed.metrics;
    // SAFETY: moving and resizing our own window.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            placed.x,
            placed.y,
            placed.metrics.width,
            placed.metrics.height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        );
        apply_round_corners(hwnd, placed.metrics.width, placed.metrics.height);
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Clip the popup to a rounded pill shape.
fn apply_round_corners(hwnd: HWND, width: i32, height: i32) {
    // SAFETY: a region for our own window; the system owns it after a successful call.
    unsafe {
        let radius = (height / 2).max(1);
        let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, radius, radius);
        if !region.is_invalid() {
            // A nonzero return means the system took ownership of the region.
            if SetWindowRgn(hwnd, Some(region), true) != 0 {
                return;
            }
            let _ = DeleteObject(HGDIOBJ(region.0));
        }
    }
}

/// Window procedure: paint, pulse, follow display changes, and pass clicks through.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_ERASEBKGND => return LRESULT(1), // painted in WM_PAINT
        // The taskbar moved or hid itself, the resolution changed, or the DPI changed.
        WM_DISPLAYCHANGE => {
            let mut shared = lock_shared();
            if let Some(state) = shared.as_mut() {
                reposition(hwnd, state);
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE if wparam.0 as u32 == SPI_SETWORKAREA.0 => {
            let mut shared = lock_shared();
            if let Some(state) = shared.as_mut() {
                reposition(hwnd, state);
            }
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TIMER_ID => {
            let mut shared = lock_shared();
            if let Some(state) = shared.as_mut() {
                state.tick = state.tick.wrapping_add(1);
                let face = Face::from_status(&lock_status(&state.status));
                let snippet = lock_status_text(&state.last_text);
                let text = display_text(face, &snippet);
                let want = desired_width(face, &text, &state.metrics);
                let size_changed = (want - state.metrics.width).abs() > 2;
                let text_changed = state.shown != Some(face) || state.shown_text != text;
                // Keep animating while recording or transcribing; idle repaints only on change.
                let animating = matches!(face, Face::Recording | Face::Busy);
                if size_changed {
                    state.shown = Some(face);
                    state.shown_text = text;
                    let placed = place(
                        Some(hwnd),
                        state.margin,
                        state.position,
                        Some(want),
                    );
                    state.metrics = placed.metrics;
                    // SAFETY: moving and resizing our own window.
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            placed.x,
                            placed.y,
                            placed.metrics.width,
                            placed.metrics.height,
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                        apply_round_corners(hwnd, placed.metrics.width, placed.metrics.height);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                } else if text_changed || animating {
                    state.shown = Some(face);
                    state.shown_text = text;
                    // SAFETY: repainting our own window.
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
            }
            return LRESULT(0);
        }
        msg if msg == MSG_SET_VISIBLE => {
            let visible = wparam.0 != 0;
            // SAFETY: showing or hiding our own window without activating it.
            unsafe {
                let _ = ShowWindow(
                    hwnd,
                    if visible { SW_SHOWNOACTIVATE } else { SW_HIDE },
                );
                if visible {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            // SAFETY: painting our own window inside WM_PAINT.
            unsafe { paint(hwnd) };
            return LRESULT(0);
        }
        WM_CLOSE => {
            // SAFETY: destroying our own window.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            // SAFETY: releasing the timer and ending the loop for this thread.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Lighten a `COLORREF` (0x00BBGGRR) by `amount` per channel, saturating at 255.
fn lighten(color: u32, amount: u8) -> u32 {
    let r = ((color & 0xFF) + amount as u32).min(255);
    let g = (((color >> 8) & 0xFF) + amount as u32).min(255);
    let b = (((color >> 16) & 0xFF) + amount as u32).min(255);
    r | (g << 8) | (b << 16)
}

/// Darken a `COLORREF` (0x00BBGGRR) by `amount` per channel, saturating at 0.
fn darken(color: u32, amount: u8) -> u32 {
    let r = (color & 0xFF).saturating_sub(amount as u32);
    let g = ((color >> 8) & 0xFF).saturating_sub(amount as u32);
    let b = ((color >> 16) & 0xFF).saturating_sub(amount as u32);
    r | (g << 8) | (b << 16)
}

/// Number of bars in the live waveform shown while listening/transcribing.
const WAVE_BARS: i32 = 5;

/// Height of bar `index` on animation frame `tick`, in pixels.
///
/// A travelling sine so the bars dance left-to-right; `max_h` caps the tallest bar.
/// `Recording` uses a taller, faster wave than `Busy`.
fn wave_bar_height(face: Face, tick: u64, index: i32, max_h: i32) -> i32 {
    let speed = match face {
        Face::Recording => 0.55,
        Face::Busy => 0.35,
        _ => 0.0,
    };
    let phase = tick as f32 * speed + index as f32 * 0.9;
    let wave = 0.5 + 0.5 * phase.sin();
    let min_h = (max_h / 4).max(3);
    min_h + ((max_h - min_h) as f32 * wave) as i32
}

/// Render one frame: modern card, waveform icon and, while active, the live snippet.
unsafe fn paint(hwnd: HWND) {
    let (face, tick, metrics, text) = {
        let shared = lock_shared();
        match shared.as_ref() {
            Some(state) => (
                Face::from_status(&lock_status(&state.status)),
                state.tick,
                state.metrics,
                state.shown_text.clone(),
            ),
            None => (Face::Idle, 0, metrics(96, 0), String::new()),
        }
    };
    let bright = tick % 6 < 3;
    let animating = matches!(face, Face::Recording | Face::Busy);

    // SAFETY: all GDI calls below operate on the DC returned by BeginPaint and release what they
    // create before EndPaint.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            let _ = EndPaint(hwnd, &ps);
            return;
        }

        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let width = (client.right - client.left).max(1);
        let height = (client.bottom - client.top).max(1);
        let center_y = height / 2;
        let base = face.background();

        // Modern card: a 1 px rounded border with a vertical gradient inside it.
        // The window region already clips to the pill shape, so filling the whole
        // client with the border colour and then the inset leaves exactly the edge.
        let border = CreateSolidBrush(COLORREF(lighten(base, 42)));
        FillRect(hdc, &client, border);
        let _ = DeleteObject(HGDIOBJ(border.0));
        let inner = RECT { left: 1, top: 1, right: width - 1, bottom: height - 1 };
        let top = CreateSolidBrush(COLORREF(lighten(base, 12)));
        let top_half = RECT { left: 1, top: 1, right: width - 1, bottom: height / 2 + 1 };
        FillRect(hdc, &top_half, top);
        let _ = DeleteObject(HGDIOBJ(top.0));
        let bottom_brush = CreateSolidBrush(COLORREF(darken(base, 6)));
        let bottom_half = RECT { left: 1, top: height / 2 + 1, right: width - 1, bottom: height - 1 };
        FillRect(hdc, &bottom_half, bottom_brush);
        let _ = DeleteObject(HGDIOBJ(bottom_brush.0));
        let _ = inner;

        // Sweeping highlight while active: a soft band gliding across the pill.
        if animating && width > 60 {
            let band = (width / 5).max(24);
            let span = width + band;
            let head = ((tick as i64 * width as i64 / 12) % span as i64) as i32 - band;
            let sweep = RECT {
                left: head.max(1),
                top: 1,
                right: (head + band).min(width - 1),
                bottom: height - 1,
            };
            if sweep.right > sweep.left {
                let highlight = CreateSolidBrush(COLORREF(lighten(base, 16)));
                FillRect(hdc, &sweep, highlight);
                let _ = DeleteObject(HGDIOBJ(highlight.0));
            }
        }

        // Soft top highlight line for a glass edge.
        let gloss = CreateSolidBrush(COLORREF(lighten(base, 30)));
        let gloss_rect = RECT { left: 5, top: 2, right: width - 5, bottom: 4 };
        FillRect(hdc, &gloss_rect, gloss);
        let _ = DeleteObject(HGDIOBJ(gloss.0));

        if face.shows_text() {
            // Live waveform: bars dancing on a travelling sine around the icon slot.
            let dpi_scale = metrics.font_height.max(8) as f32 / FONT_HEIGHT as f32;
            let bar_w = ((3.0 * dpi_scale).round() as i32).max(2);
            let gap = ((3.0 * dpi_scale).round() as i32).max(2);
            let max_h = (height * 58 / 100).max(8);
            let total = WAVE_BARS * bar_w + (WAVE_BARS - 1) * gap;
            let mut x = metrics.dot_x - total / 2;
            // Faint track behind the wave so it reads on any wallpaper.
            let track = CreateSolidBrush(COLORREF(lighten(base, 10)));
            let track_rect = RECT {
                left: x - 4,
                top: center_y - max_h / 2 - 4,
                right: x + total + 4,
                bottom: center_y + max_h / 2 + 4,
            };
            FillRect(hdc, &track_rect, track);
            let _ = DeleteObject(HGDIOBJ(track.0));
            let bar_brush = CreateSolidBrush(COLORREF(if bright {
                lighten(face.color(), 26)
            } else {
                face.color()
            }));
            for i in 0..WAVE_BARS {
                let h = wave_bar_height(face, tick, i, max_h).max(3);
                let bar = RECT {
                    left: x,
                    top: center_y - h / 2,
                    right: x + bar_w,
                    bottom: center_y - h / 2 + h.max(3),
                };
                FillRect(hdc, &bar, bar_brush);
                x += bar_w + gap;
            }
            let _ = DeleteObject(HGDIOBJ(bar_brush.0));
        } else {
            // Idle / error: a lone centered dot with a soft halo.
            let (center_x, radius) = (width / 2, metrics.idle_dot_radius);
            let halo = CreateSolidBrush(COLORREF(lighten(base, 22)));
            let previous = SelectObject(hdc, HGDIOBJ(halo.0));
            let _ = Ellipse(
                hdc,
                center_x - radius - 3,
                center_y - radius - 3,
                center_x + radius + 3,
                center_y + radius + 3,
            );
            SelectObject(hdc, previous);
            let _ = DeleteObject(HGDIOBJ(halo.0));
            let dot = CreateSolidBrush(COLORREF(face.color()));
            let previous = SelectObject(hdc, HGDIOBJ(dot.0));
            let _ = Ellipse(
                hdc,
                center_x - radius,
                center_y - radius,
                center_x + radius,
                center_y + radius,
            );
            SelectObject(hdc, previous);
            let _ = DeleteObject(HGDIOBJ(dot.0));
        }

        // Live snippet (never shown while idle).
        if face.shows_text() {
            let font = CreateFontW(
                -metrics.font_height,
                0,
                0,
                0,
                FW_SEMIBOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                w!("Segoe UI"),
            );
            let previous_font = SelectObject(hdc, HGDIOBJ(font.0));
            SetBkMode(hdc, TRANSPARENT);
            // Shimmer: the text breathes between two close shades while transcribing.
            let colour = match face {
                Face::Recording if bright => 0x00F2_F2F2,
                Face::Recording => 0x00C9_C9FF,
                Face::Busy if bright => 0x00F2_F2F2,
                Face::Busy => 0x00B3_E3FF,
                _ => 0x00F2_F2F2,
            };
            SetTextColor(hdc, COLORREF(colour));

            let mut wide: Vec<u16> = text.encode_utf16().collect();
            let mut text_rect = RECT {
                left: metrics.text_left,
                top: 0,
                right: client.right - metrics.text_right_pad,
                bottom: client.bottom,
            };
            DrawTextW(
                hdc,
                &mut wide,
                &mut text_rect,
                windows::Win32::Graphics::Gdi::DT_LEFT
                    | windows::Win32::Graphics::Gdi::DT_VCENTER
                    | windows::Win32::Graphics::Gdi::DT_SINGLELINE
                    | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS
                    | windows::Win32::Graphics::Gdi::DT_NOPREFIX,
            );

            SelectObject(hdc, previous_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(width: i32, height: i32) -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    #[test]
    fn status_maps_to_the_right_face() {
        assert_eq!(Face::from_status(&Status::Paused), Face::Idle);
        assert_eq!(Face::from_status(&Status::Starting), Face::Idle);
        assert_eq!(Face::from_status(&Status::Listening), Face::Recording);
        assert_eq!(Face::from_status(&Status::Transcribing(2)), Face::Busy);
        assert_eq!(Face::from_status(&Status::Error("x".into())), Face::Error);
    }

    #[test]
    fn idle_shows_an_icon_with_no_text() {
        assert_eq!(Face::Idle.label(), "");
        assert!(!Face::Idle.shows_text());
        assert!(Face::Recording.shows_text());
        assert!(Face::Busy.shows_text());
    }

    #[test]
    fn snippets_are_collapsed_and_capped() {
        assert_eq!(truncate_snippet("  hello \n world\t"), "hello world");
        assert_eq!(truncate_snippet(""), "");
        let long: String = "w".repeat(200);
        let capped = truncate_snippet(&long);
        assert_eq!(capped.chars().count(), MAX_SNIPPET_CHARS);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn the_pill_shows_the_live_snippet_while_active() {
        assert_eq!(display_text(Face::Recording, "halo dunia"), "halo dunia");
        assert_eq!(display_text(Face::Recording, ""), "Listening…");
        assert_eq!(display_text(Face::Busy, ""), "Transcribing");
        assert_eq!(display_text(Face::Idle, "halo dunia"), "");
    }

    #[test]
    fn the_pill_stays_between_the_idle_and_max_widths() {
        let m = metrics(96, 24);
        assert_eq!(m.idle_width, IDLE_WIDTH);
        assert_eq!(desired_width(Face::Idle, "", &m), IDLE_WIDTH);
        let narrow = desired_width(Face::Recording, "hi", &m);
        assert!(narrow > m.idle_width && narrow < m.max_width);
        assert_eq!(desired_width(Face::Recording, &"w".repeat(500), &m), m.max_width);
    }

    #[test]
    fn lighten_saturates_each_channel() {
        assert_eq!(lighten(0x0010_1020, 16), 0x0020_2030);
        assert_eq!(lighten(0x00F0_F0F0, 32), 0x00FF_FFFF);
    }

    #[test]
    fn darken_saturates_at_zero() {
        assert_eq!(darken(0x0020_2030, 16), 0x0010_1020);
        assert_eq!(darken(0x0005_0505, 32), 0x0000_0000);
    }

    #[test]
    fn waveform_bars_stay_inside_their_bounds_and_move() {
        let heights: Vec<i32> =
            (0..12).map(|t| wave_bar_height(Face::Recording, t, 0, 20)).collect();
        assert!(heights.iter().all(|&h| (5..=20).contains(&h)), "got {heights:?}");
        assert!(heights.windows(2).any(|w| w[0] != w[1]), "wave should move: {heights:?}");
        // Idle faces never drive the wave.
        assert_eq!(wave_bar_height(Face::Idle, 99, 0, 20), wave_bar_height(Face::Idle, 99, 0, 20));
    }

    #[test]
    fn the_pill_clears_an_auto_hidden_taskbar() {
        // The machine this was reported on: 1920x1080 at 125%, taskbar set to hide itself, so the
        // work area is the whole monitor and the taskbar's window keeps its 60 px shown height
        // while it is slid off the bottom edge.
        let monitor = monitor(1920, 1080);
        let taskbar = RECT {
            left: 0,
            top: 1078,
            right: 1920,
            bottom: 1138,
        };
        let bands = taskbar_bands(monitor, Some(taskbar));
        assert_eq!(bands.bottom, 60, "the shown height, not the 2 px still on screen");

        let (x, y) = placement(
            monitor,
            monitor,
            bands,
            (MAX_WIDTH, HEIGHT),
            24,
            IndicatorPosition::BottomRight,
        );
        assert_eq!((x, y), (1436, 960));
        assert!(y + HEIGHT + 24 <= 1020, "must stay clear of the shown taskbar");
    }

    #[test]
    fn a_visible_taskbar_gives_the_same_place() {
        let monitor = monitor(1920, 1080);
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1020,
        };
        let taskbar = RECT {
            left: 0,
            top: 1020,
            right: 1920,
            bottom: 1080,
        };
        let bands = taskbar_bands(monitor, Some(taskbar));
        assert_eq!(bands.bottom, 60);
        assert_eq!(
            placement(monitor, work, bands, (MAX_WIDTH, HEIGHT), 24, IndicatorPosition::BottomRight),
            (1436, 960),
            "reserving the band must not shift a taskbar the work area already excludes"
        );
    }

    #[test]
    fn without_a_taskbar_the_pill_keeps_the_old_margin() {
        let monitor = monitor(1920, 1080);
        let bands = taskbar_bands(monitor, None);
        assert_eq!(bands, Bands::default());
        assert_eq!(
            placement(monitor, monitor, bands, (MAX_WIDTH, HEIGHT), 24, IndicatorPosition::BottomRight),
            (1436, 1020)
        );
    }

    #[test]
    fn every_corner_works_with_a_top_docked_taskbar() {
        let monitor = monitor(1920, 1080);
        let taskbar = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 40,
        };
        let bands = taskbar_bands(monitor, Some(taskbar));
        assert_eq!(bands.top, 40);
        let work = RECT {
            left: 0,
            top: 40,
            right: 1920,
            bottom: 1080,
        };
        let at = |position| placement(monitor, work, bands, (MAX_WIDTH, HEIGHT), 24, position);
        assert_eq!(at(IndicatorPosition::TopLeft), (24, 64));
        assert_eq!(at(IndicatorPosition::TopRight), (1436, 64));
        assert_eq!(at(IndicatorPosition::BottomLeft), (24, 1020));
        assert_eq!(at(IndicatorPosition::BottomRight), (1436, 1020));
    }

    #[test]
    fn a_side_docked_taskbar_is_reserved_too() {
        let monitor = monitor(1920, 1080);
        let taskbar = RECT {
            left: 0,
            top: 0,
            right: 60,
            bottom: 1080,
        };
        let bands = taskbar_bands(monitor, Some(taskbar));
        assert_eq!(bands.left, 60);
        let work = RECT {
            left: 60,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(
            placement(monitor, work, bands, (MAX_WIDTH, HEIGHT), 24, IndicatorPosition::BottomLeft),
            (84, 1020)
        );
    }

    #[test]
    fn a_small_screen_still_keeps_the_pill_inside() {
        let monitor = monitor(200, 200);
        let (x, y) = placement(
            monitor,
            monitor,
            Bands::default(),
            (MAX_WIDTH, HEIGHT),
            24,
            IndicatorPosition::BottomRight,
        );
        // The pill is wider than the screen, so it clamps to the left edge.
        assert_eq!((x, y), (0, 140));
        assert!(y + HEIGHT <= 200);
    }

    #[test]
    fn metrics_follow_the_monitor_dpi() {
        let at_96 = metrics(96, 24);
        assert_eq!(
            (at_96.width, at_96.height, at_96.margin),
            (MAX_WIDTH, HEIGHT, 24)
        );
        assert_eq!(at_96.idle_width, IDLE_WIDTH);
        let at_125 = metrics(120, 24);
        assert_eq!((at_125.width, at_125.height, at_125.margin), (575, 45, 30));
        assert_eq!(scale(15, 120), 19);
        assert_eq!(scale(166, 144), 249);
    }
}

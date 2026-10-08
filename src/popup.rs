//! Success toast + transcription result popup.
//!
//! Flow: the user stops listening, the queued audio drains, and the pipeline settles on
//! `Paused`. The tray watches for that edge and calls [`show_ready_toast`]: a small modern
//! card ("It's done — click to open the transcript") that dismisses itself after a few seconds.
//! Clicking it opens [`open_result`]: a larger card with the session Markdown, a flick
//! light/dark switch in the header, and footer buttons to copy the text, open the full
//! viewer, or close.
//!
//! Each window owns its thread and message loop (like the viewer), so the tray keeps
//! working while one is shown. Both windows are painted with GDI in the same palette as
//! the viewer, so the theme switch feels familiar.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, CreateFontW, CreateRoundRectRgn,
    CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, DrawTextW, Ellipse, EndPaint,
    FF_DONTCARE, FW_BOLD, FW_NORMAL, FW_SEMIBOLD, FillRect, GetMonitorInfoW, HGDIOBJ,
    InvalidateRect, MONITORINFO, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, KillTimer,
    LWA_ALPHA, PostQuitMessage, RegisterClassW, SCROLLINFO, SB_BOTTOM, SB_LINEDOWN, SB_LINEUP,
    SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION, SB_THUMBTRACK, SB_TOP, SB_VERT, SIF_PAGE, SIF_POS,
    SIF_RANGE, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNORMAL, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes,
    SetTimer, SetWindowLongPtrW, GetWindowLongPtrW, GWLP_USERDATA, SetWindowPos, ShowWindow,
    TranslateMessage, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEWHEEL,
    WM_NCDESTROY, WM_PAINT, WM_TIMER, WM_VSCROLL, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

use crate::config::ViewerTheme;
use crate::logging::Logger;

/// Window classes; registered once per process.
const TOAST_CLASS: PCWSTR = w!("DepthToast");
const RESULT_CLASS: PCWSTR = w!("DepthResult");
static TOAST_REGISTERED: AtomicBool = AtomicBool::new(false);
static RESULT_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Toast timers and behaviour.
const TIMER_TOAST: usize = 11;
const TIMER_MS: u32 = 100;
/// Ticks before the toast dismisses itself (~6.5 s).
const TOAST_LIFETIME: u32 = 65;
/// Slide-in frames.
const TOAST_SLIDE: u32 = 6;
/// Toast logical size.
const TOAST_W: i32 = 372;
const TOAST_H: i32 = 96;

/// Result timers and layout (logical pixels at 96 DPI).
const TIMER_RESULT: usize = 12;
const RESULT_W: i32 = 580;
const RESULT_H: i32 = 480;
const HEADER_H: i32 = 56;
const FOOTER_H: i32 = 62;
const PADDING: i32 = 18;
const TITLE_FONT: i32 = 15;
const BODY_FONT: i32 = 14;
const SMALL_FONT: i32 = 12;
const LINE_GAP: i32 = 5;
/// Ticks the "Copied" feedback stays on the Copy button (~1.5 s).
const COPIED_TICKS: u32 = 15;

/// Flick switch size (logical).
const SWITCH_W: i32 = 52;
const SWITCH_H: i32 = 26;
const KNOB: i32 = 20;

/// Palette shared with the viewer (kept in sync by eye, not by import, so the
/// popup never depends on the viewer's private items).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Theme {
    bg: u32,
    header: u32,
    title: u32,
    body: u32,
    stamp: u32,
    rule: u32,
    accent: u32,
}

const DARK: Theme = Theme {
    bg: 0x001E_1E1E,
    header: 0x0026_2626,
    title: 0x00F2_F2F2,
    body: 0x00E8_E8E8,
    stamp: 0x0088_8888,
    rule: 0x0044_4444,
    accent: 0x003B_A55C,
};

const LIGHT: Theme = Theme {
    bg: 0x00FA_FAFA,
    header: 0x00EF_EFEF,
    title: 0x001A_1A1A,
    body: 0x0021_2121,
    stamp: 0x006E_6E6E,
    rule: 0x00D4_D4D4,
    accent: 0x002E_7D43,
};

fn theme_for(mode: ViewerTheme) -> Theme {
    match mode {
        ViewerTheme::Dark => DARK,
        ViewerTheme::Light => LIGHT,
        ViewerTheme::System => DARK,
    }
}

fn is_light(mode: ViewerTheme) -> bool {
    theme_for(mode) == LIGHT
}

/// Collapse `markdown` to one line and return its tail (the latest words), capped at
/// `max_chars` with a leading ellipsis when truncated.
pub fn preview_tail(markdown: &str, max_chars: usize) -> String {
    let single: String = markdown.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = single.chars().collect();
    if chars.len() <= max_chars || max_chars < 2 {
        return single;
    }
    format!("…{}", chars[chars.len() - (max_chars - 1)..].iter().collect::<String>())
}

/// One unwrapped result row: `title` rows paint bold (headers), the rest paint normal.
fn result_rows(raw: &str) -> Vec<(bool, String)> {
    let mut rows = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            rows.push((false, String::new()));
        } else if trimmed.starts_with("## ") {
            rows.push((true, trimmed[3..].replace("**", "").trim().to_string()));
        } else if trimmed.starts_with("# ") {
            rows.push((true, trimmed[2..].replace("**", "").trim().to_string()));
        } else if trimmed.starts_with("**") {
            // `**HH:MM:SS** body` -> `HH:MM:SS   body`, like the viewer.
            let rest = &trimmed[2..];
            match rest.find("**") {
                Some(end) if rest[..end].contains(':') => {
                    let time = rest[..end].trim();
                    let body = rest[end + 2..].replace("**", "").trim().to_string();
                    rows.push((false, format!("{time}   {body}")));
                }
                _ => rows.push((false, trimmed.replace("**", ""))),
            }
        } else {
            rows.push((false, trimmed.replace("**", "")));
        }
    }
    if rows.is_empty() {
        rows.push((false, "(empty transcript)".to_string()));
    }
    rows
}

/// Greedy word wrap of one row into lines of at most `per_line` chars.
fn wrap_row(text: &str, per_line: usize) -> Vec<String> {
    let per_line = per_line.max(12);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        let add = if current.is_empty() { wlen } else { wlen + 1 };
        if len + add > per_line && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            len = 0;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        len += add;
        if len >= per_line {
            lines.push(std::mem::take(&mut current));
            len = 0;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// X offset of the switch knob inside `track` at `progress` (0 = dark/left, 1 = light/right).
fn switch_knob_x(track: &RECT, progress: f32, knob_w: i32) -> i32 {
    let travel = (track.right - track.left - knob_w - 6).max(0);
    track.left + 3 + (travel as f32 * progress.clamp(0.0, 1.0)) as i32
}

/// Track rect of the flick switch: right-aligned in a `view_w`-wide header.
fn switch_rect(view_w: i32, header_h: i32, pad: i32) -> RECT {
    RECT {
        left: view_w - pad - SWITCH_W,
        top: (header_h - SWITCH_H) / 2,
        right: view_w - pad,
        bottom: (header_h - SWITCH_H) / 2 + SWITCH_H,
    }
}

/// Footer buttons (Copy, Open transcript, Close), right-aligned above the bottom edge.
fn footer_buttons(view_w: i32, view_h: i32, pad: i32, dpi: f32) -> [RECT; 3] {
    let s = |v: i32| ((v as f32 * dpi).round() as i32).max(1);
    let (bw, bh, gap) = (s(104), s(32), s(10));
    let full_w = s(120);
    let y = view_h - pad - bh;
    let close = RECT { left: view_w - pad - bw, top: y, right: view_w - pad, bottom: y + bh };
    let full = RECT {
        left: close.left - gap - full_w,
        top: y,
        right: close.left - gap,
        bottom: y + bh,
    };
    let copy = RECT {
        left: full.left - gap - bw,
        top: y,
        right: full.left - gap,
        bottom: y + bh,
    };
    [copy, full, close]
}

fn point_in(rect: &RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

fn scale(logical: i32, dpi: f32) -> i32 {
    ((logical as f32 * dpi).round() as i32).max(1)
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

/// State for one toast window, freed on `WM_NCDESTROY`.
struct Toast {
    preview: String,
    note: String,
    theme: Theme,
    theme_mode: ViewerTheme,
    raw: String,
    file_name: String,
    logger: Arc<Logger>,
    tick: u32,
    x0: i32,
    x1: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// Show the "It's done" toast. Clicking it opens the result popup with
/// `raw_markdown`; otherwise it dismisses itself after a few seconds.
pub fn show_ready_toast(
    raw_markdown: &str,
    file_name: &str,
    theme_mode: ViewerTheme,
    logger: Arc<Logger>,
) {
    let raw = raw_markdown.to_string();
    let file_name = file_name.to_string();
    let preview = preview_tail(&raw, 88);
    let lines = raw.lines().filter(|l| !l.trim().is_empty()).count();
    let note = format!("{lines} line{} • click to open the transcript", if lines == 1 { "" } else { "s" });
    std::thread::Builder::new()
        .name("depth-toast".to_string())
        .spawn(move || {
            if let Err(err) = run_toast(&preview, &note, &raw, &file_name, theme_mode, logger.clone()) {
                logger.error(format!("result toast stopped: {err:#}"));
            }
        })
        .ok();
}

fn work_area() -> RECT {
    // SAFETY: a plain monitor query with a valid out-parameter.
    unsafe {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            return info.rcWork;
        }
    }
    RECT { left: 0, top: 0, right: 1280, bottom: 720 }
}

fn run_toast(
    preview: &str,
    note: &str,
    raw: &str,
    file_name: &str,
    theme_mode: ViewerTheme,
    logger: Arc<Logger>,
) -> anyhow::Result<()> {
    // SAFETY: standard Win32 window setup on this thread.
    unsafe {
        if !TOAST_REGISTERED.swap(true, Ordering::Relaxed) {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(toast_proc),
                hInstance: instance.into(),
                lpszClassName: TOAST_CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
        }
        let instance = GetModuleHandleW(None)?;
        let area = work_area();
        let (w, h) = (TOAST_W, TOAST_H);
        let x1 = area.right - w - 20;
        let y = area.bottom - h - 20 - 52;
        let x0 = x1 + 48;
        let caption = "Depth — It's done — click to open the transcript";
        let caption_w: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            TOAST_CLASS,
            PCWSTR(caption_w.as_ptr()),
            WS_POPUP,
            x0,
            y,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 238, LWA_ALPHA)?;
        let region = CreateRoundRectRgn(0, 0, w + 1, h + 1, 20, 20);
        if !region.is_invalid() {
            if SetWindowRgn(hwnd, Some(region), true) == 0 {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(
                    windows::Win32::Graphics::Gdi::HGDIOBJ(region.0),
                );
            }
        }
        let mut toast = Box::new(Toast {
            preview: preview.to_string(),
            note: note.to_string(),
            theme: theme_for(theme_mode),
            theme_mode,
            raw: raw.to_string(),
            file_name: file_name.to_string(),
            logger: logger.clone(),
            tick: 0,
            x0,
            x1,
            y,
            w,
            h,
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, toast.as_mut() as *mut Toast as isize);
        std::mem::forget(toast);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_TOAST, TIMER_MS, None);
        logger.info("result toast shown; click it to open the transcript".to_string());
        let mut message = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn with_toast(hwnd: HWND, f: impl FnOnce(&mut Toast)) {
    // SAFETY: the pointer was stored on creation and freed on WM_NCDESTROY.
    unsafe {
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if raw != 0 {
            f(&mut *(raw as *mut Toast));
        }
    }
}

unsafe extern "system" fn toast_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_TIMER => {
            let mut done = false;
            let mut slide_to: Option<i32> = None;
            with_toast(hwnd, |toast| {
                toast.tick += 1;
                if toast.tick <= TOAST_SLIDE {
                    let t = toast.tick as f32 / TOAST_SLIDE as f32;
                    slide_to = Some((toast.x0 as f32 + (toast.x1 - toast.x0) as f32 * t) as i32);
                }
                if toast.tick >= TOAST_LIFETIME {
                    done = true;
                }
            });
            if let Some(x) = slide_to {
                // SAFETY: moving our own window.
                unsafe {
                    with_toast(hwnd, |toast| {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            x,
                            toast.y,
                            toast.w,
                            toast.h,
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    });
                }
            }
            if done {
                // SAFETY: destroying our own window.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
                }
            } else {
                // SAFETY: repainting our own window.
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            // Click anywhere: open the result popup, then go away.
            let (raw, file_name, theme_mode, logger) = {
                let mut out = (String::new(), String::new(), ViewerTheme::Dark, Arc::new(Logger::disabled()));
                with_toast(hwnd, |toast| {
                    out = (toast.raw.clone(), toast.file_name.clone(), toast.theme_mode, toast.logger.clone());
                });
                out
            };
            logger.info("result toast clicked; opening the transcript".to_string());
            open_result(&file_name, &raw, theme_mode, logger);
            // SAFETY: destroying our own window.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_PAINT => {
            // SAFETY: painting our own window inside WM_PAINT.
            unsafe { paint_toast(hwnd) };
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
            // SAFETY: ending the loop for this thread.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_TOAST);
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: the last message for the window; free the state.
            unsafe {
                let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if raw != 0 {
                    let _ = Box::from_raw(raw as *mut Toast);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Draw one run of text and return the x where it ends.
unsafe fn draw_run(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    text: &str,
    x: i32,
    y: i32,
    right: i32,
    height: i32,
    font_height: i32,
    weight: i32,
    color: u32,
    font_name: PCWSTR,
) -> i32 {
    use windows::Win32::Graphics::Gdi::{
        DT_CALCRECT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    };
    if text.is_empty() {
        return x;
    }
    // SAFETY: the caller holds a valid paint DC; the font is released below.
    unsafe {
        let font = CreateFontW(
            -font_height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            font_name,
        );
        let previous = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(color));
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut rect = RECT { left: x, top: y, right, bottom: y + height };
        DrawTextW(
            hdc,
            &mut wide,
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_CALCRECT,
        );
        let measured = rect.right - rect.left;
        rect.right = right;
        DrawTextW(
            hdc,
            &mut wide,
            &mut rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SelectObject(hdc, previous);
        let _ = DeleteObject(HGDIOBJ(font.0));
        x + measured
    }
}

unsafe fn paint_toast(hwnd: HWND) {
    // SAFETY: all GDI calls operate on the DC returned by BeginPaint.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let (theme, preview, note) = {
            let mut snap = (DARK, String::new(), String::new());
            with_toast(hwnd, |toast| {
                snap = (toast.theme, toast.preview.clone(), toast.note.clone());
            });
            snap
        };
        let width = (client.right - client.left).max(1);
        let height = (client.bottom - client.top).max(1);

        // Card: border fill, then the inset body.
        let border = CreateSolidBrush(COLORREF(0x0044_4444));
        FillRect(hdc, &client, border);
        let _ = DeleteObject(HGDIOBJ(border.0));
        let body = RECT { left: 1, top: 1, right: width - 1, bottom: height - 1 };
        let card = CreateSolidBrush(COLORREF(theme.bg));
        FillRect(hdc, &body, card);
        let _ = DeleteObject(HGDIOBJ(card.0));

        // Success badge: green disc with a check mark.
        let (cx, cy, r) = (34, height / 2, 15);
        let disc = CreateSolidBrush(COLORREF(theme.accent));
        let prev = SelectObject(hdc, HGDIOBJ(disc.0));
        let _ = Ellipse(hdc, cx - r, cy - r, cx + r, cy + r);
        SelectObject(hdc, prev);
        let _ = DeleteObject(HGDIOBJ(disc.0));
        draw_run(hdc, "✓", cx - 8, cy - 12, cx + 8, 24, 17, FW_BOLD.0 as i32, 0x00FF_FFFF, w!("Segoe UI"));

        let tx = 58;
        let right = width - 14;
        draw_run(hdc, "It's done — transcription ready", tx, 8, right, 22, TITLE_FONT, FW_SEMIBOLD.0 as i32, theme.title, w!("Segoe UI"));
        draw_run(hdc, &preview, tx, 30, right, 22, BODY_FONT - 1, FW_NORMAL.0 as i32, theme.body, w!("Segoe UI"));
        draw_run(hdc, &note, tx, 54, right, 20, SMALL_FONT, FW_NORMAL.0 as i32, theme.stamp, w!("Segoe UI"));

        let _ = EndPaint(hwnd, &ps);
    }
}

// ---------------------------------------------------------------------------
// Result popup
// ---------------------------------------------------------------------------

/// One wrapped visual line.
#[derive(Debug, Clone)]
struct Line {
    title: bool,
    text: String,
}

/// State for one result window, freed on `WM_NCDESTROY`.
struct ResultWin {
    raw: String,
    file_name: String,
    theme: Theme,
    theme_mode: ViewerTheme,
    logger: Arc<Logger>,
    lines: Vec<Line>,
    dpi_scale: f32,
    padding: i32,
    header_h: i32,
    footer_h: i32,
    scroll: i32,
    content_h: i32,
    view_h: i32,
    view_w: i32,
    switch_anim: f32,
    switch_target: f32,
    copied_until: u32,
    tick: u32,
}

/// Open the result popup for `raw_markdown`. Copy-ready: the Copy button (or Ctrl+C)
/// puts the full Markdown on the clipboard.
pub fn open_result(
    file_name: &str,
    raw_markdown: &str,
    theme_mode: ViewerTheme,
    logger: Arc<Logger>,
) {
    let file_name = file_name.to_string();
    let raw = raw_markdown.to_string();
    let thread_logger = logger.clone();
    if std::thread::Builder::new()
        .name("depth-result".to_string())
        .spawn(move || {
            if let Err(err) = run_result(&file_name, &raw, theme_mode, thread_logger.clone()) {
                thread_logger.error(format!("result popup stopped: {err:#}"));
            }
        })
        .is_err()
    {
        logger.error("cannot open the result popup".to_string());
    }
}

/// Suggested file name for handoffs (viewer export reuses it).
pub fn suggested_name(path: Option<&PathBuf>) -> String {
    path.as_ref()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "transcript.md".to_string())
}

fn run_result(
    file_name: &str,
    raw: &str,
    theme_mode: ViewerTheme,
    logger: Arc<Logger>,
) -> anyhow::Result<()> {
    // SAFETY: standard Win32 window setup on this thread.
    unsafe {
        if !RESULT_REGISTERED.swap(true, Ordering::Relaxed) {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(result_proc),
                hInstance: instance.into(),
                lpszClassName: RESULT_CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);
        }
        let instance = GetModuleHandleW(None)?;
        let area = work_area();
        let dpi_hint = 96u32;
        let dpi_scale = dpi_hint as f32 / 96.0;
        let (win_w, win_h) = (
            (RESULT_W as f32 * dpi_scale) as i32,
            (RESULT_H as f32 * dpi_scale) as i32,
        );
        let x = area.left + ((area.right - area.left - win_w) / 2).max(0);
        let y = area.top + ((area.bottom - area.top - win_h) / 2).max(0);
        let caption = format!("It's done — {file_name} — open the transcript");
        let caption_w: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = CreateWindowExW(
            Default::default(),
            RESULT_CLASS,
            PCWSTR(caption_w.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VSCROLL,
            x,
            y,
            win_w,
            win_h,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let window_dpi = GetDpiForWindow(hwnd);
        let dpi_scale = if window_dpi == 0 { dpi_scale } else { window_dpi as f32 / 96.0 };
        let theme = theme_for(theme_mode);
        let start = if is_light(theme_mode) { 1.0 } else { 0.0 };
        let mut win = Box::new(ResultWin {
            raw: raw.to_string(),
            file_name: file_name.to_string(),
            theme,
            theme_mode,
            logger: logger.clone(),
            lines: Vec::new(),
            dpi_scale,
            padding: scale(PADDING, dpi_scale),
            header_h: scale(HEADER_H, dpi_scale),
            footer_h: scale(FOOTER_H, dpi_scale),
            scroll: 0,
            content_h: 0,
            view_h: 0,
            view_w: 0,
            switch_anim: start,
            switch_target: start,
            copied_until: 0,
            tick: 0,
        });
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        relayout_result(&mut win, client.right - client.left);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, win.as_mut() as *mut ResultWin as isize);
        std::mem::forget(win);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        update_result_scrollbar(hwnd);
        SetTimer(Some(hwnd), TIMER_RESULT, TIMER_MS, None);
        logger.info(format!("result popup opened: {file_name}"));
        let mut message = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn line_height(kind_title: bool, dpi: f32) -> i32 {
    let font = if kind_title { TITLE_FONT } else { BODY_FONT };
    scale(font, dpi) + scale(LINE_GAP, dpi)
}

fn relayout_result(win: &mut ResultWin, view_w: i32) {
    win.view_w = view_w.max(1);
    let content_w = (view_w - win.padding * 2).max(120);
    let avg = (scale(BODY_FONT, win.dpi_scale) * 55 / 100).max(1);
    let per_line = (content_w / avg).max(12) as usize;
    let mut lines = Vec::new();
    for (title, row) in result_rows(&win.raw) {
        for wrapped in wrap_row(&row, if title { per_line } else { per_line }) {
            lines.push(Line { title, text: wrapped });
        }
    }
    win.lines = lines;
    let body: i32 = win.lines.iter().map(|l| line_height(l.title, win.dpi_scale)).sum();
    win.content_h = win.header_h + win.padding + body + win.padding + win.footer_h;
}

fn with_result(hwnd: HWND, f: impl FnOnce(&mut ResultWin)) {
    // SAFETY: the pointer was stored on creation and freed on WM_NCDESTROY.
    unsafe {
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if raw != 0 {
            f(&mut *(raw as *mut ResultWin));
        }
    }
}

fn update_result_scrollbar(hwnd: HWND) {
    with_result(hwnd, |win| {
        let mut client = RECT::default();
        // SAFETY: a plain geometry query on our own window.
        unsafe {
            let _ = GetClientRect(hwnd, &mut client);
        }
        win.view_h = client.bottom - client.top;
        win.view_w = client.right - client.left;
        win.scroll = win.scroll.clamp(0, (win.content_h - win.view_h).max(0));
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: win.content_h.max(1),
            nPage: win.view_h.max(1) as u32,
            nPos: win.scroll,
            ..Default::default()
        };
        // SAFETY: updating our own window's scrollbar.
        unsafe {
            SetScrollInfo(hwnd, SB_VERT, &info, true);
        }
    });
}

fn scroll_result_by(hwnd: HWND, delta: i32) {
    with_result(hwnd, |win| {
        win.scroll = (win.scroll + delta).clamp(0, (win.content_h - win.view_h).max(0));
    });
    update_result_scrollbar(hwnd);
    // SAFETY: repainting our own window.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn do_result_copy(hwnd: HWND) {
    let mut problem = String::new();
    let mut chars = 0usize;
    with_result(hwnd, |win| {
        match crate::viewer::copy_text_to_clipboard(&win.raw) {
            Ok(()) => {
                chars = win.raw.chars().count();
                win.copied_until = win.tick + COPIED_TICKS;
            }
            Err(err) => problem = format!("{err:#}"),
        }
    });
    with_result(hwnd, |win| {
        if chars > 0 {
            win.logger.info(format!("copied the result popup ({chars} chars) to the clipboard"));
        } else {
            win.logger.warn(format!("copy failed: {problem}"));
        }
    });
    // SAFETY: repainting our own window (the Copy button shows "Copied ✓").
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn do_result_toggle_theme(hwnd: HWND) {
    with_result(hwnd, |win| {
        win.theme_mode = match win.theme_mode {
            ViewerTheme::Dark => ViewerTheme::Light,
            _ => ViewerTheme::Dark,
        };
        win.theme = theme_for(win.theme_mode);
        win.switch_target = if is_light(win.theme_mode) { 1.0 } else { 0.0 };
        win.logger.info(format!("result popup theme: {}", win.theme_mode.key()));
    });
    // SAFETY: repainting our own window.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn do_result_full_viewer(hwnd: HWND) {
    let (name, raw, mode, logger) = {
        let mut out = (String::new(), String::new(), ViewerTheme::Dark, Arc::new(Logger::disabled()));
        with_result(hwnd, |win| {
            out = (win.file_name.clone(), win.raw.clone(), win.theme_mode, win.logger.clone());
        });
        out
    };
    crate::viewer::open_text(&name, &raw, mode, logger);
}

unsafe extern "system" fn result_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TIMER => {
            let mut needs_paint = false;
            with_result(hwnd, |win| {
                win.tick = win.tick.wrapping_add(1);
                // Ease the flick switch toward its target.
                if (win.switch_anim - win.switch_target).abs() > 0.01 {
                    let step = 0.22;
                    if win.switch_anim < win.switch_target {
                        win.switch_anim = (win.switch_anim + step).min(win.switch_target);
                    } else {
                        win.switch_anim = (win.switch_anim - step).max(win.switch_target);
                    }
                    needs_paint = true;
                } else if win.switch_anim != win.switch_target {
                    win.switch_anim = win.switch_target;
                    needs_paint = true;
                }
            });
            if needs_paint {
                // SAFETY: repainting our own window.
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            let hit = {
                let mut out = String::new();
                with_result(hwnd, |win| {
                    let sw = switch_rect(win.view_w.max(1), win.header_h, win.padding);
                    if point_in(&sw, x, y) {
                        out = "switch".to_string();
                        return;
                    }
                    let buttons = footer_buttons(
                        win.view_w.max(1),
                        win.view_h.max(1),
                        win.padding,
                        win.dpi_scale,
                    );
                    if point_in(&buttons[0], x, y) {
                        out = "copy".to_string();
                    } else if point_in(&buttons[1], x, y) {
                        out = "full".to_string();
                    } else if point_in(&buttons[2], x, y) {
                        out = "close".to_string();
                    }
                });
                out
            };
            match hit.as_str() {
                "switch" => do_result_toggle_theme(hwnd),
                "copy" => do_result_copy(hwnd),
                "full" => do_result_full_viewer(hwnd),
                "close" => {
                    // SAFETY: destroying our own window.
                    unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let key = (wparam.0 & 0xFFFF) as u32;
            // SAFETY: reading the async modifier state for our own shortcuts.
            let ctrl = unsafe {
                use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
                GetKeyState(VK_CONTROL.0 as i32) < 0
            };
            if ctrl && key == 0x43 {
                do_result_copy(hwnd);
                return LRESULT(0);
            }
            if key == 0x1B {
                // SAFETY: destroying our own window.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
                }
                return LRESULT(0);
            }
            return LRESULT(0);
        }
        WM_VSCROLL => {
            let code = (wparam.0 & 0xFFFF) as i32;
            let pos = ((wparam.0 >> 16) & 0xFFFF) as i32;
            with_result(hwnd, |win| {
                let step = line_height(false, win.dpi_scale);
                let max = (win.content_h - win.view_h).max(0);
                match code {
                    x if x == SB_LINEUP.0 as i32 => win.scroll = (win.scroll - step).max(0),
                    x if x == SB_LINEDOWN.0 as i32 => win.scroll = (win.scroll + step).min(max),
                    x if x == SB_PAGEUP.0 as i32 => win.scroll = (win.scroll - win.view_h).max(0),
                    x if x == SB_PAGEDOWN.0 as i32 => win.scroll = (win.scroll + win.view_h).min(max),
                    x if x == SB_TOP.0 as i32 => win.scroll = 0,
                    x if x == SB_BOTTOM.0 as i32 => win.scroll = max,
                    x if x == SB_THUMBTRACK.0 as i32 || x == SB_THUMBPOSITION.0 as i32 => {
                        win.scroll = pos.clamp(0, max)
                    }
                    _ => {}
                }
            });
            update_result_scrollbar(hwnd);
            // SAFETY: repainting our own window.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as u16) as i16 as i32;
            let step = {
                let mut s = 24;
                with_result(hwnd, |win| s = line_height(false, win.dpi_scale));
                s.max(1)
            };
            scroll_result_by(hwnd, -delta * step * 3 / 120);
            return LRESULT(0);
        }
        WM_PAINT => {
            // SAFETY: painting our own window inside WM_PAINT.
            unsafe { paint_result(hwnd) };
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
            // SAFETY: ending the loop for this thread.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_RESULT);
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: the last message for the window; free the state.
            unsafe {
                let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if raw != 0 {
                    let _ = Box::from_raw(raw as *mut ResultWin);
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                }
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Paint the flick switch track + knob. Shared look with the viewer header.
unsafe fn paint_switch(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    track: &RECT,
    progress: f32,
    light: bool,
) {
    // SAFETY: the caller holds a valid paint DC.
    unsafe {
        let track_w = track.right - track.left;
        let track_h = track.bottom - track.top;
        // Track: dark grey in dark mode, green-tinted in light mode.
        let track_color = if light { 0x0043_9B5F } else { 0x0044_4444 };
        let brush = CreateSolidBrush(COLORREF(track_color));
        // Rounded track: centre bar + two end discs.
        FillRect(
            hdc,
            &RECT { left: track.left + track_h / 2, top: track.top, right: track.right - track_h / 2, bottom: track.bottom },
            brush,
        );
        let prev = SelectObject(hdc, HGDIOBJ(brush.0));
        let _ = Ellipse(hdc, track.left, track.top, track.left + track_h, track.bottom);
        let _ = Ellipse(hdc, track.right - track_h, track.top, track.right, track.bottom);
        SelectObject(hdc, prev);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        // Glyph: moon on the left (dark), sun on the right (light).
        let glyph = if light { "☀" } else { "☾" };
        let gx = if light { track.right - track_h + 3 } else { track.left + 3 };
        draw_run(
            hdc,
            glyph,
            gx,
            track.top + 2,
            gx + track_h - 6,
            track_h - 4,
            track_h - 9,
            FW_NORMAL.0 as i32,
            0x00FF_FFFF,
            w!("Segoe UI"),
        );
        // Knob slides left <-> right.
        let knob_x = switch_knob_x(track, progress, KNOB);
        let ky = track.top + (track_h - KNOB) / 2;
        let knob = CreateSolidBrush(COLORREF(0x00FF_FFFF));
        let prev = SelectObject(hdc, HGDIOBJ(knob.0));
        let _ = Ellipse(hdc, knob_x, ky, knob_x + KNOB, ky + KNOB);
        SelectObject(hdc, prev);
        let _ = DeleteObject(HGDIOBJ(knob.0));
        let _ = track_w;
    }
}

/// Paint one footer button; `active` draws the accent fill.
unsafe fn paint_button(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    rect: &RECT,
    label: &str,
    theme: &Theme,
    active: bool,
) {
    // SAFETY: the caller holds a valid paint DC.
    unsafe {
        let fill = CreateSolidBrush(COLORREF(if active { theme.accent } else { theme.header }));
        FillRect(hdc, rect, fill);
        let _ = DeleteObject(HGDIOBJ(fill.0));
        let color = if active { 0x00FF_FFFF } else { theme.title };
        draw_run(
            hdc,
            label,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom - rect.top,
            SMALL_FONT + 2,
            FW_SEMIBOLD.0 as i32,
            color,
            w!("Segoe UI"),
        );
    }
}

unsafe fn paint_result(hwnd: HWND) {
    // SAFETY: all GDI calls operate on the DC returned by BeginPaint.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let snapshot = {
            let mut snap = None;
            with_result(hwnd, |win| {
                snap = Some((
                    win.theme,
                    win.theme_mode,
                    win.padding,
                    win.header_h,
                    win.footer_h,
                    win.scroll,
                    win.dpi_scale,
                    win.view_w.max(1),
                    win.view_h.max(1),
                    win.switch_anim,
                    win.tick,
                    win.copied_until,
                    win.file_name.clone(),
                    win.lines.clone(),
                ));
            });
            snap
        };
        let Some((
            theme,
            theme_mode,
            padding,
            header_h,
            footer_h,
            scroll,
            dpi,
            view_w,
            view_h,
            anim,
            tick,
            copied_until,
            file_name,
            lines,
        )) = snapshot
        else {
            let _ = EndPaint(hwnd, &ps);
            return;
        };

        // Body background.
        let bg = CreateSolidBrush(COLORREF(theme.bg));
        FillRect(hdc, &client, bg);
        let _ = DeleteObject(HGDIOBJ(bg.0));

        // Header card.
        let header_rect = RECT { left: 0, top: 0, right: view_w, bottom: header_h };
        let header_brush = CreateSolidBrush(COLORREF(theme.header));
        FillRect(hdc, &header_rect, header_brush);
        let _ = DeleteObject(HGDIOBJ(header_brush.0));
        let rule = CreateSolidBrush(COLORREF(theme.rule));
        let rule_rect = RECT { left: 0, top: header_h - 1, right: view_w, bottom: header_h };
        FillRect(hdc, &rule_rect, rule);
        let _ = DeleteObject(HGDIOBJ(rule.0));

        draw_run(
            hdc,
            "It's done — open the transcript",
            padding,
            4,
            view_w - padding - SWITCH_W - 12,
            header_h / 2,
            scale(TITLE_FONT, dpi),
            FW_SEMIBOLD.0 as i32,
            theme.title,
            w!("Segoe UI"),
        );
        draw_run(
            hdc,
            &file_name,
            padding,
            4 + header_h / 2,
            view_w - padding - SWITCH_W - 12,
            header_h / 2 - 4,
            scale(SMALL_FONT, dpi),
            FW_NORMAL.0 as i32,
            theme.stamp,
            w!("Segoe UI"),
        );
        let sw = switch_rect(view_w, header_h, padding);
        paint_switch(hdc, &sw, anim, is_light(theme_mode));

        // Footer card.
        let footer_top = view_h - footer_h;
        let footer_rect = RECT { left: 0, top: footer_top, right: view_w, bottom: view_h };
        let footer_brush = CreateSolidBrush(COLORREF(theme.header));
        FillRect(hdc, &footer_rect, footer_brush);
        let _ = DeleteObject(HGDIOBJ(footer_brush.0));
        let rule2 = CreateSolidBrush(COLORREF(theme.rule));
        let rule2_rect = RECT { left: 0, top: footer_top, right: view_w, bottom: footer_top + 1 };
        FillRect(hdc, &rule2_rect, rule2);
        let _ = DeleteObject(HGDIOBJ(rule2.0));
        let buttons = footer_buttons(view_w, view_h, padding, dpi);
        let copied = tick <= copied_until;
        paint_button(hdc, &buttons[0], if copied { "Copied ✓" } else { "Copy" }, &theme, copied);
        paint_button(hdc, &buttons[1], "Open transcript", &theme, false);
        paint_button(hdc, &buttons[2], "Close", &theme, false);

        // Body lines between header and footer, honouring the scroll offset.
        let mut y = header_h + padding / 2 - scroll;
        let bottom = footer_top - 4;
        for line in &lines {
            let h = line_height(line.title, dpi);
            if y + h >= header_h && y <= bottom {
                let weight = if line.title { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 };
                let color = if line.title { theme.title } else { theme.body };
                let height = if line.title { scale(TITLE_FONT, dpi) } else { scale(BODY_FONT, dpi) };
                draw_run(
                    hdc,
                    &line.text,
                    padding,
                    y,
                    view_w - padding,
                    h,
                    height,
                    weight,
                    color,
                    w!("Segoe UI"),
                );
            }
            y += h;
            if y > bottom {
                break;
            }
        }

        let _ = EndPaint(hwnd, &ps);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_takes_the_tail_and_caps_its_length() {
        assert_eq!(preview_tail("halo dunia", 88), "halo dunia");
        let long = "kata ".repeat(60);
        let capped = preview_tail(&long, 88);
        assert_eq!(capped.chars().count(), 88);
        assert!(capped.starts_with('…'));
        assert!(preview_tail("a\nb\tc", 88) == "a b c");
    }

    #[test]
    fn result_rows_keep_headers_and_fold_timestamps() {
        let rows = result_rows("# Title\n\n## Session 10:00\n\n**10:00:01** halo\n");
        assert!(rows.iter().any(|(t, s)| *t && s.contains("Title")));
        assert!(rows.iter().any(|(t, s)| !t && s.starts_with("10:00:01")));
    }

    #[test]
    fn wrap_keeps_every_word() {
        let lines = wrap_row("alpha beta gamma delta", 8);
        assert!(lines.len() > 1);
        assert_eq!(lines.join(" "), "alpha beta gamma delta");
    }

    #[test]
    fn the_switch_knob_travels_the_track() {
        let track = RECT { left: 0, top: 0, right: 52, bottom: 26 };
        let left = switch_knob_x(&track, 0.0, KNOB);
        let right = switch_knob_x(&track, 1.0, KNOB);
        assert_eq!(left, 3);
        assert!(right > left);
        assert_eq!(right + KNOB, 49);
        assert_eq!(switch_knob_x(&track, 0.5, KNOB), (3 + right) / 2);
    }

    #[test]
    fn footer_buttons_fit_and_do_not_overlap() {
        let [copy, full, close] = footer_buttons(580, 480, 18, 1.0);
        assert!(copy.left >= 0 && close.right <= 580);
        assert!(copy.right <= full.left && full.right <= close.left);
        assert_eq!(copy.top, full.top);
        assert_eq!(full.top, close.top);
    }

    #[test]
    fn suggested_name_falls_back_to_a_markdown_file() {
        assert_eq!(suggested_name(None), "transcript.md");
        let path = PathBuf::from("C:\\x\\transcript-2025-01-01.md");
        assert_eq!(suggested_name(Some(&path)), "transcript-2025-01-01.md");
    }
}

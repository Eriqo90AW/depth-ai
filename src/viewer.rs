//! A small built-in Markdown viewer for the daily transcripts.
//!
//! "View transcript" opens this window instead of handing the `.md` file to whatever viewer
//! Windows has registered: a normal overlapped window (taskbar button, Alt+Tab entry) painted
//! with GDI in the app's dark theme. It renders what the writer actually produces — the day
//! title, `## Session` headers, `**HH:MM:SS**` utterance lines and plain paragraphs — with
//! word wrap and scrolling. Tables, images and nested markup render as plain text.
//!
//! Each viewer owns its thread and message loop, so several transcripts can be open at once and
//! the tray keeps working while one is shown.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, DrawTextW, Ellipse, EndPaint, FF_DONTCARE, FW_BOLD,
    FW_NORMAL, FW_SEMIBOLD, FillRect, GetMonitorInfoW, HGDIOBJ, InvalidateRect, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, MDT_EFFECTIVE_DPI, GetDpiForMonitor};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR, VK_UP,
};
use windows::Win32::UI::Controls::SetScrollInfo;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, KillTimer,
    SCROLLINFO, SB_VERT, SIF_PAGE, SIF_POS, SIF_RANGE, SetWindowLongPtrW, GetWindowLongPtrW, GWLP_USERDATA,
    PostQuitMessage, RegisterClassW, SB_BOTTOM, SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP,
    SB_THUMBPOSITION, SB_THUMBTRACK, SB_TOP, SW_SHOWNORMAL, SWP_NOZORDER, SetTimer, SetWindowPos, ShowWindow,
    TranslateMessage, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_SETTINGCHANGE,
    WM_DESTROY, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WM_SIZE,
    WM_TIMER, WM_VSCROLL, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

use crate::config::ViewerTheme;
use crate::logging::Logger;

/// Window class name; registered once per process.
const CLASS_NAME: PCWSTR = w!("DepthViewer");
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Logical metrics at 96 DPI.
const PADDING: i32 = 20;
const TITLE_FONT: i32 = 22;
const SESSION_FONT: i32 = 16;
const BODY_FONT: i32 = 14;
const LINE_GAP_EXTRA: i32 = 4;
const PARAGRAPH_GAP: i32 = 10;

/// Header bar holding the file name and the flick theme switch (logical).
const HEADER_H: i32 = 52;
/// Flick switch size (logical).
const SWITCH_W: i32 = 52;
const SWITCH_H: i32 = 26;
const SWITCH_KNOB: i32 = 20;
/// Timer that eases the switch knob toward its target.
const TIMER_SWITCH: usize = 7;
const TIMER_SWITCH_MS: u32 = 40;

/// Full palette for one viewer theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Theme {
    bg: u32,
    title: u32,
    session: u32,
    stamp: u32,
    body: u32,
    rule: u32,
}

/// The original dark theme (unchanged colours).
const DARK: Theme = Theme {
    bg: 0x001E_1E1E,
    title: 0x00F2_F2F2,
    session: 0x00F6_B564, // light blue
    stamp: 0x0088_8888,
    body: 0x00E8_E8E8,
    rule: 0x0044_4444,
};

/// Light theme: near-white background, dark text. Body contrast vs background
/// is ~15:1, well above the 4.5:1 readability bar.
const LIGHT: Theme = Theme {
    bg: 0x00FA_FAFA,
    title: 0x001A_1A1A,
    session: 0x000B_5CAD,
    stamp: 0x006E_6E6E,
    body: 0x0021_2121,
    rule: 0x00D4_D4D4,
};

/// Resolve a theme choice to a palette, reading the Windows app theme for `System`.
fn theme_for(mode: ViewerTheme) -> Theme {
    match mode {
        ViewerTheme::Dark => DARK,
        ViewerTheme::Light => LIGHT,
        ViewerTheme::System => {
            if system_prefers_light() {
                LIGHT
            } else {
                DARK
            }
        }
    }
}

/// True when the Windows "app theme" is light.
///
/// Reads `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`
/// (1 = light, 0 = dark). Any failure falls back to dark, matching the old viewer.
fn system_prefers_light() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegGetValueW, RRF_RT_DWORD};
    use windows::core::w;
    // SAFETY: plain registry read into a stack DWORD; missing keys just fall back to dark.
    unsafe {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut std::ffi::c_void),
            Some(&mut size as *mut u32),
        );
        status.is_ok() && value != 0
    }
}

/// One parsed Markdown row.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Title(String),
    Session(String),
    /// A `**HH:MM:SS** body` line; leftover `**` markers are stripped.
    Utterance { time: String, body: String },
    Rule,
    Blank,
    Body(String),
}

/// One wrapped visual line, ready to paint.
#[derive(Debug, Clone)]
struct VisualLine {
    kind: VisualKind,
    text: String,
    /// Extra left offset (the utterance body hangs under its timestamp).
    indent: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisualKind {
    Title,
    Session,
    Stamp,
    UtteranceBody,
    Body,
    Rule,
}

impl VisualKind {
    fn font_height(self, dpi_scale: f32) -> i32 {
        let logical = match self {
            VisualKind::Title => TITLE_FONT,
            VisualKind::Session => SESSION_FONT,
            VisualKind::Stamp | VisualKind::UtteranceBody | VisualKind::Body => BODY_FONT,
            VisualKind::Rule => BODY_FONT,
        };
        ((logical as f32 * dpi_scale).round() as i32).max(8)
    }

    fn color(self, theme: &Theme) -> u32 {
        match self {
            VisualKind::Title => theme.title,
            VisualKind::Session => theme.session,
            VisualKind::Stamp => theme.stamp,
            VisualKind::UtteranceBody | VisualKind::Body => theme.body,
            VisualKind::Rule => theme.rule,
        }
    }
}

/// Strip paired `**` markers; the writer only emits them around timestamps.
fn strip_bold(text: &str) -> String {
    text.replace("**", "")
}

/// Split a `**HH:MM:SS** rest` line into its parts.
fn split_utterance(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("**")?;
    let end = rest.find("**")?;
    let (time, body) = rest.split_at(end);
    if time.contains(':') {
        Some((time.trim().to_string(), strip_bold(body[2..].trim())))
    } else {
        None
    }
}

/// Parse the transcript Markdown into rows.
fn parse_markdown(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            rows.push(Row::Blank);
        } else if let Some(title) = trimmed.strip_prefix("# ") {
            rows.push(Row::Title(strip_bold(title.trim())));
        } else if let Some(session) = trimmed.strip_prefix("## ") {
            rows.push(Row::Session(strip_bold(session.trim())));
        } else if trimmed == "---" || trimmed == "***" {
            rows.push(Row::Rule);
        } else if let Some((time, body)) = split_utterance(trimmed) {
            rows.push(Row::Utterance { time, body });
        } else if let Some(body) = trimmed.strip_prefix("- ") {
            rows.push(Row::Body(format!("• {}", strip_bold(body.trim()))));
        } else {
            rows.push(Row::Body(strip_bold(trimmed)));
        }
    }
    if rows.is_empty() {
        rows.push(Row::Body("(empty transcript)".to_string()));
    }
    rows
}

/// Greedy word wrap of `text` into lines that fit `max_width` pixels (estimated).
fn wrap_text(text: &str, font_height: i32, max_width: i32) -> Vec<String> {
    let avg = (font_height * 55 / 100).max(1);
    let per_line = (max_width / avg).max(12) as usize;
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        let add = if current.is_empty() { word_len } else { word_len + 1 };
        if current_len + add > per_line && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        current_len += add;
        // A single unsplittable word still gets its own line.
        if current_len >= per_line {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
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

/// Lay rows out into visual lines for `content_width` pixels at `dpi_scale`.
fn layout_rows(rows: &[Row], content_width: i32, dpi_scale: f32) -> Vec<VisualLine> {
    let mut out = Vec::new();
    // Width of the timestamp column, so utterance bodies hang-indent past it.
    let stamp_w = (BODY_FONT as f32 * dpi_scale * 0.55 * 10.0) as i32 + 12;
    for row in rows {
        match row {
            Row::Title(text) => {
                let h = VisualKind::Title.font_height(dpi_scale);
                for line in wrap_text(text, h, content_width) {
                    out.push(VisualLine { kind: VisualKind::Title, text: line, indent: 0 });
                }
            }
            Row::Session(text) => {
                let h = VisualKind::Session.font_height(dpi_scale);
                for line in wrap_text(text, h, content_width) {
                    out.push(VisualLine { kind: VisualKind::Session, text: line, indent: 0 });
                }
            }
            Row::Utterance { time, body } => {
                out.push(VisualLine {
                    kind: VisualKind::Stamp,
                    text: time.clone(),
                    indent: 0,
                });
                let h = VisualKind::UtteranceBody.font_height(dpi_scale);
                let wrapped = wrap_text(body, h, (content_width - stamp_w).max(120));
                let mut first = true;
                for line in wrapped {
                    out.push(VisualLine {
                        kind: VisualKind::UtteranceBody,
                        text: line,
                        indent: if first { 0 } else { stamp_w },
                    });
                    first = false;
                }
            }
            Row::Rule => out.push(VisualLine {
                kind: VisualKind::Rule,
                text: String::from("─").repeat(24),
                indent: 0,
            }),
            Row::Blank => out.push(VisualLine {
                kind: VisualKind::Body,
                text: String::new(),
                indent: 0,
            }),
            Row::Body(text) => {
                let h = VisualKind::Body.font_height(dpi_scale);
                for line in wrap_text(text, h, content_width) {
                    out.push(VisualLine { kind: VisualKind::Body, text: line, indent: 0 });
                }
            }
        }
    }
    // Fold each stamp line into the utterance line that follows it.
    fold_stamps(out, stamp_w)
}

/// Merge `Stamp` + first `UtteranceBody` line pairs into one `"time  body"` line.
///
/// The painter draws the timestamp dim and the remainder bright by splitting on the separator.
fn fold_stamps(lines: Vec<VisualLine>, _stamp_w: i32) -> Vec<VisualLine> {
    const SEP: &str = "   ";
    let mut out: Vec<VisualLine> = Vec::with_capacity(lines.len());
    let mut pending_stamp: Option<String> = None;
    for line in lines {
        match line.kind {
            VisualKind::Stamp => {
                if let Some(stamp) = pending_stamp.take() {
                    out.push(VisualLine {
                        kind: VisualKind::UtteranceBody,
                        text: format!("{stamp}{SEP}"),
                        indent: 0,
                    });
                }
                pending_stamp = Some(line.text);
            }
            VisualKind::UtteranceBody if line.indent == 0 => {
                if let Some(stamp) = pending_stamp.take() {
                    out.push(VisualLine {
                        kind: VisualKind::UtteranceBody,
                        text: format!("{stamp}{SEP}{}", line.text),
                        indent: 0,
                    });
                } else {
                    out.push(line);
                }
            }
            _ => {
                if let Some(stamp) = pending_stamp.take() {
                    out.push(VisualLine {
                        kind: VisualKind::UtteranceBody,
                        text: format!("{stamp}{SEP}"),
                        indent: 0,
                    });
                }
                out.push(line);
            }
        }
    }
    if let Some(stamp) = pending_stamp.take() {
        out.push(VisualLine {
            kind: VisualKind::UtteranceBody,
            text: format!("{stamp}"),
            indent: 0,
        });
    }
    out
}

/// Split a folded `"time   body"` line back into its parts for two-tone painting.
fn split_folded(text: &str) -> (String, String) {
    match text.find("   ") {
        Some(at) => (text[..at].to_string(), text[at + 3..].to_string()),
        None => (text.to_string(), String::new()),
    }
}

/// Text for the Copy-All action: the original Markdown, so pasting elsewhere
/// keeps timestamps and headers intact (not the wrapped visual lines).
fn build_copy_all(raw: &str) -> String {
    raw.to_string()
}

/// Text for the Copy-Session action: the last `## Session` block including its
/// day title, or the whole document when no session header exists.
///
/// The trailing `## Full text` combined section is never treated as a session and
/// never rides along with the copy: only the session's own timestamped block is
/// returned.
fn build_copy_last_session(raw: &str) -> String {
    let mut title = "";
    let mut last_start: Option<usize> = None;
    let mut offset = 0;
    for line in raw.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") && title.is_empty() {
            title = line;
        }
        if trimmed.starts_with("## Session") {
            last_start = Some(offset);
        }
        offset += line.len();
    }
    match last_start {
        Some(start) => {
            let mut block = &raw[start..];
            // Stop before the combined section; it holds every session, not this one.
            if let Some(at) = block.find("\n## Full text\n") {
                block = &block[..at];
            }
            format!("{title}{block}")
        }
        None => raw.to_string(),
    }
}

/// Copy UTF-16 text to the Windows clipboard, retrying briefly when another
/// app holds it open. The system owns the handle after a successful set.
pub fn copy_text_to_clipboard(text: &str) -> anyhow::Result<()> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

    /// `CF_UNICODETEXT` (13). Kept as a literal so the viewer does not need the
    /// whole `Win32_System_Ole` API surface for one constant.
    const CF_UNICODETEXT: u32 = 13;

    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();
    // SAFETY: plain Win32 clipboard transfer on this thread. The block is
    // allocated once and reused across retries; only a persistent failure leaks
    // it (one small block, and the error is reported).
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes)
            .map_err(|e| anyhow::anyhow!("GlobalAlloc failed: {e:?}"))?;
        if mem.is_invalid() {
            return Err(anyhow::anyhow!("GlobalAlloc failed"));
        }
        let locked = GlobalLock(mem);
        if locked.is_null() {
            return Err(anyhow::anyhow!("GlobalLock failed"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), locked as *mut u16, wide.len());
        let _ = GlobalUnlock(mem);
        let mut last_err = "the clipboard is busy".to_string();
        for _ in 0..3 {
            if OpenClipboard(None).is_ok() {
                let placed = (|| -> anyhow::Result<()> {
                    EmptyClipboard().map_err(|e| anyhow::anyhow!("EmptyClipboard: {e:?}"))?;
                    SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0)))
                        .map_err(|e| anyhow::anyhow!("SetClipboardData: {e:?}"))?;
                    // Success: the system owns `mem` now; it must not be freed here.
                    let _ = mem;
                    Ok(())
                })();
                let _ = CloseClipboard();
                match placed {
                    Ok(()) => return Ok(()),
                    Err(err) => last_err = format!("{err:#}"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(60));
        }
        Err(anyhow::anyhow!("cannot copy to the clipboard: {last_err}"))
    }
}

/// Contrast ratio of two sRGB greys packed as `0x00RRGGBB` (WCAG formula).
/// Used by tests to keep both themes readable.
#[cfg(test)]
fn contrast_ratio(a: u32, b: u32) -> f32 {
    fn luminance(c: u32) -> f32 {
        let channel = |v: u32| {
            let s = (v as f32) / 255.0;
            if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel((c >> 16) & 0xFF)
            + 0.7152 * channel((c >> 8) & 0xFF)
            + 0.0722 * channel(c & 0xFF)
    }
    let (hi, lo) = if luminance(a) > luminance(b) {
        (luminance(a), luminance(b))
    } else {
        (luminance(b), luminance(a))
    };
    (hi + 0.05) / (lo + 0.05)
}

/// Per-window state, owned by the window thread and freed on `WM_NCDESTROY`.
struct Viewer {
    rows: Vec<Row>,
    lines: Vec<VisualLine>,
    /// The full Markdown as opened/exported; this is what Copy uses so pasted
    /// text keeps its original lines instead of the wrapped visual lines.
    raw: String,
    /// Suggested file name for the Export action (e.g. `transcript-2025-06-01.md`).
    file_name: String,
    theme: Theme,
    theme_mode: ViewerTheme,
    logger: Arc<Logger>,
    dpi_scale: f32,
    padding: i32,
    header_h: i32,
    scroll: i32,
    content_h: i32,
    view_h: i32,
    content_w: i32,
    /// Flick switch animation: 0 = dark/left, 1 = light/right.
    switch_anim: f32,
    switch_target: f32,
}

/// Track rect of the flick switch, right-aligned in a `view_w`-wide header.
fn switch_rect(view_w: i32, header_h: i32, padding: i32) -> RECT {
    RECT {
        left: view_w - padding - SWITCH_W,
        top: (header_h - SWITCH_H) / 2,
        right: view_w - padding,
        bottom: (header_h - SWITCH_H) / 2 + SWITCH_H,
    }
}

/// X offset of the switch knob inside `track` at `progress` (0 = left, 1 = right).
fn switch_knob_x(track: &RECT, progress: f32) -> i32 {
    let travel = (track.right - track.left - SWITCH_KNOB - 6).max(0);
    track.left + 3 + (travel as f32 * progress.clamp(0.0, 1.0)) as i32
}

fn switch_target_for(mode: ViewerTheme) -> f32 {
    match mode {
        ViewerTheme::Light => 1.0,
        ViewerTheme::Dark => 0.0,
        ViewerTheme::System => {
            if system_prefers_light() { 1.0 } else { 0.0 }
        }
    }
}

impl Viewer {
    fn line_height(&self, kind: VisualKind) -> i32 {
        kind.font_height(self.dpi_scale) + scale_gap(LINE_GAP_EXTRA, self.dpi_scale)
    }

    fn relayout(&mut self, view_w: i32) {
        self.content_w = (view_w - self.padding * 2).max(120);
        self.lines = layout_rows(&self.rows, self.content_w, self.dpi_scale);
        self.content_h = self.header_h + self.total_height() + self.padding * 2;
    }

    fn total_height(&self) -> i32 {
        self.lines
            .iter()
            .map(|l| {
                self.line_height(l.kind)
                    + if matches!(l.kind, VisualKind::Title | VisualKind::Session) {
                        scale_gap(PARAGRAPH_GAP, self.dpi_scale) / 2
                    } else {
                        0
                    }
            })
            .sum()
    }
}

fn scale_gap(logical: i32, dpi_scale: f32) -> i32 {
    ((logical as f32 * dpi_scale).round() as i32).max(1)
}

/// Open `path` in the built-in viewer on its own thread. Never blocks the caller.
pub fn open(path: &Path, logger: Arc<Logger>) {
    open_with_theme(path, ViewerTheme::Dark, logger);
}

/// Open `path` with an explicit initial theme. Never blocks the caller.
pub fn open_with_theme(path: &Path, theme_mode: ViewerTheme, logger: Arc<Logger>) {
    let path: PathBuf = path.to_path_buf();
    let thread_logger = logger.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("transcript-viewer".to_string())
        .spawn(move || {
            let raw = match std::fs::read(&path) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(err) => {
                    thread_logger.error(format!("cannot read {}: {err:#}", path.display()));
                    return;
                }
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "transcript.md".to_string());
            if let Err(err) = run_with_text(&name, &raw, theme_mode, thread_logger.clone()) {
                thread_logger.error(format!("transcript viewer stopped: {err:#}"));
            }
        }) {
        logger.error(format!("cannot open the transcript viewer: {err:#}"));
    }
}

/// Open in-memory Markdown (used when autosave is off) in the viewer.
/// `file_name` is only a suggestion for the Export action.
pub fn open_text(file_name: &str, markdown: &str, theme_mode: ViewerTheme, logger: Arc<Logger>) {
    let file_name = file_name.to_string();
    let markdown = markdown.to_string();
    let thread_logger = logger.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("transcript-viewer".to_string())
        .spawn(move || {
            if let Err(err) = run_with_text(&file_name, &markdown, theme_mode, thread_logger.clone()) {
                thread_logger.error(format!("transcript viewer stopped: {err:#}"));
            }
        }) {
        logger.error(format!("cannot open the transcript viewer: {err:#}"));
    }
}

/// Run the viewer window for already-loaded Markdown text.
fn run_with_text(
    file_name: &str,
    markdown: &str,
    theme_mode: ViewerTheme,
    logger: Arc<Logger>,
) -> anyhow::Result<()> {
    let raw = markdown.to_string();
    let rows = parse_markdown(&raw);
    let name = if file_name.is_empty() {
        "transcript.md".to_string()
    } else {
        file_name.to_string()
    };
    let theme = theme_for(theme_mode);

    // SAFETY: standard Win32 window setup on this thread.
    unsafe {
        if !CLASS_REGISTERED.swap(true, Ordering::Relaxed) {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            RegisterClassW(&class);
        }
        let instance = GetModuleHandleW(None)?;

        // Center a 760x560 window on the primary monitor's work area.
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = MonitorFromPoint(
            windows::Win32::Foundation::POINT { x: 0, y: 0 },
            MONITOR_DEFAULTTOPRIMARY,
        );
        let (area, dpi) = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let mut dx = 0u32;
            let mut dy = 0u32;
            let dpi = match GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) {
                Ok(()) if dx > 0 => dx,
                _ => 96,
            };
            (info.rcWork, dpi)
        } else {
            (
                RECT { left: 0, top: 0, right: 1280, bottom: 720 },
                96,
            )
        };
        let dpi_scale = dpi as f32 / 96.0;
        let (win_w, win_h) = (
            (760.0 * dpi_scale) as i32,
            (560.0 * dpi_scale) as i32,
        );
        let x = area.left + ((area.right - area.left - win_w) / 2).max(0);
        let y = area.top + ((area.bottom - area.top - win_h) / 2).max(0);

        let caption = format!("Transcript — {name}");
        let caption_wide: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = CreateWindowExW(
            Default::default(),
            CLASS_NAME,
            PCWSTR(caption_wide.as_ptr()),
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
        let mut viewer = Box::new(Viewer {
            rows,
            lines: Vec::new(),
            raw,
            file_name: name.clone(),
            theme,
            theme_mode,
            logger: logger.clone(),
            dpi_scale,
            padding: scale_gap(PADDING, dpi_scale),
            header_h: scale_gap(HEADER_H, dpi_scale),
            scroll: 0,
            content_h: 0,
            view_h: 0,
            content_w: 0,
            switch_anim: switch_target_for(theme_mode),
            switch_target: switch_target_for(theme_mode),
        });
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        viewer.content_w = (client.right - client.left - viewer.padding * 2).max(120);
        viewer.lines = layout_rows(&viewer.rows, viewer.content_w, viewer.dpi_scale);
        viewer.content_h = viewer.header_h + viewer.total_height() + viewer.padding * 2;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, viewer.as_mut() as *mut Viewer as isize);
        std::mem::forget(viewer);

        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        update_scrollbar(hwnd);
        SetTimer(Some(hwnd), TIMER_SWITCH, TIMER_SWITCH_MS, None);
        logger.info(format!("transcript viewer opened: {name}"));

        let mut message = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

fn with_viewer(hwnd: HWND, f: impl FnOnce(&mut Viewer)) {
    // SAFETY: the pointer was stored on window creation and freed on WM_NCDESTROY.
    unsafe {
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if raw != 0 {
            f(&mut *(raw as *mut Viewer));
        }
    }
}

fn update_scrollbar(hwnd: HWND) {
    with_viewer(hwnd, |viewer| {
        let mut client = RECT::default();
        // SAFETY: a plain geometry query on our own window.
        unsafe {
            let _ = GetClientRect(hwnd, &mut client);
        }
        viewer.view_h = client.bottom - client.top;
        viewer.scroll = viewer
            .scroll
            .clamp(0, (viewer.content_h - viewer.view_h).max(0));
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: viewer.content_h.max(1),
            nPage: viewer.view_h.max(1) as u32,
            nPos: viewer.scroll,
            ..Default::default()
        };
        // SAFETY: updating our own window's scrollbar.
        unsafe {
            SetScrollInfo(hwnd, SB_VERT, &info, true);
        }
    });
}

fn scroll_by(hwnd: HWND, delta: i32) {
    with_viewer(hwnd, |viewer| {
        viewer.scroll = (viewer.scroll + delta).clamp(0, (viewer.content_h - viewer.view_h).max(0));
    });
    update_scrollbar(hwnd);
    // SAFETY: repainting our own window.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Context-menu / shortcut command ids.
const CMD_COPY_ALL: u16 = 1001;
const CMD_COPY_SESSION: u16 = 1002;
const CMD_TOGGLE_THEME: u16 = 1003;
const CMD_EXPORT: u16 = 1004;
const CMD_CLOSE: u16 = 1005;

/// Copy the whole document to the clipboard.
fn do_copy_all(hwnd: HWND) {
    let mut done = false;
    let mut problem = String::new();
    with_viewer(hwnd, |viewer| {
        let text = build_copy_all(&viewer.raw);
        match copy_text_to_clipboard(&text) {
            Ok(()) => {
                done = true;
                viewer.logger.info(format!(
                    "copied {} chars to the clipboard",
                    text.chars().count()
                ));
            }
            Err(err) => problem = format!("{err:#}"),
        }
    });
    if !done {
        with_viewer(hwnd, |viewer| {
            viewer.logger.warn(format!("copy failed: {problem}"));
        });
    }
}

/// Copy only the last session block to the clipboard.
fn do_copy_session(hwnd: HWND) {
    with_viewer(hwnd, |viewer| {
        let text = build_copy_last_session(&viewer.raw);
        match copy_text_to_clipboard(&text) {
            Ok(()) => viewer.logger.info(format!(
                "copied the last session ({} chars) to the clipboard",
                text.chars().count()
            )),
            Err(err) => viewer.logger.warn(format!("copy failed: {err:#}")),
        }
    });
}

/// Flip between the dark and light palettes and repaint.
fn do_toggle_theme(hwnd: HWND) {
    with_viewer(hwnd, |viewer| {
        viewer.theme = if viewer.theme == DARK { LIGHT } else { DARK };
        viewer.theme_mode = if viewer.theme == DARK {
            ViewerTheme::Dark
        } else {
            ViewerTheme::Light
        };
        viewer.switch_target = switch_target_for(viewer.theme_mode);
        viewer
            .logger
            .info(format!("viewer theme: {}", viewer.theme_mode.key()));
    });
    // SAFETY: repainting our own window.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// Apply the Windows app theme when the viewer follows the system.
fn apply_system_theme(hwnd: HWND) {
    with_viewer(hwnd, |viewer| {
        if viewer.theme_mode == ViewerTheme::System {
            viewer.theme = theme_for(ViewerTheme::System);
            viewer.switch_target = switch_target_for(ViewerTheme::System);
        }
    });
    // SAFETY: repainting our own window.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// Ask where to write the document, then export the in-memory Markdown.
fn do_export(hwnd: HWND) {
    use windows::Win32::UI::Controls::Dialogs::{
        GetSaveFileNameW, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::core::{PCWSTR, PWSTR};

    let (suggested, text) = {
        let mut out = (String::new(), String::new());
        with_viewer(hwnd, |viewer| {
            out = (viewer.file_name.clone(), viewer.raw.clone());
        });
        out
    };
    if text.is_empty() {
        with_viewer(hwnd, |viewer| {
            viewer.logger.warn("nothing to export yet".to_string());
        });
        return;
    }
    // SAFETY: a standard Save dialog owned by our window; buffers outlive the call.
    unsafe {
        let filter: Vec<u16> = "Markdown files (*.md)\0*.md\0All files (*.*)\0*.*\0\0"
            .encode_utf16()
            .collect();
        let mut file: Vec<u16> = suggested
            .encode_utf16()
            .chain(std::iter::repeat(0))
            .take(260)
            .collect();
        let def_ext: Vec<u16> = "md\0".encode_utf16().collect();
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: hwnd,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(file.as_mut_ptr()),
            nMaxFile: file.len() as u32,
            lpstrDefExt: PCWSTR(def_ext.as_ptr()),
            Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST,
            ..Default::default()
        };
        if !GetSaveFileNameW(&mut ofn).as_bool() {
            return;
        }
        let end = file.iter().position(|&c| c == 0).unwrap_or(file.len());
        let path = String::from_utf16_lossy(&file[..end]);
        with_viewer(hwnd, |viewer| match std::fs::write(&path, &text) {
            Ok(()) => viewer.logger.info(format!("transcript exported to {path}")),
            Err(err) => viewer
                .logger
                .error(format!("cannot export the transcript to {path}: {err:#}")),
        });
    }
}

/// Right-click menu: Copy all / Copy session / Theme / Export / Close.
fn show_context_menu(hwnd: HWND) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, MF_STRING, TPM_RETURNCMD, TrackPopupMenu,
    };
    // SAFETY: a throwaway popup menu for our own window.
    unsafe {
        let menu = match CreatePopupMenu() {
            Ok(menu) => menu,
            Err(_) => return,
        };
        let item = |id: u16, text: &str| {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(wide.as_ptr()));
        };
        let theme_label = {
            let mut label = "Switch to light theme";
            with_viewer(hwnd, |viewer| {
                if viewer.theme == LIGHT {
                    label = "Switch to dark theme";
                }
            });
            label
        };
        item(CMD_COPY_ALL, "Copy all\tCtrl+C");
        item(CMD_COPY_SESSION, "Copy last session\tCtrl+Shift+C");
        item(CMD_TOGGLE_THEME, theme_label);
        item(CMD_EXPORT, "Save .md as…\tCtrl+S");
        item(CMD_CLOSE, "Close\tEsc");
        let mut cursor = POINT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor);
        // With TPM_RETURNCMD the return value is the chosen command id (0 = dismissed).
        let choice = TrackPopupMenu(menu, TPM_RETURNCMD, cursor.x, cursor.y, None, hwnd, None);
        if choice.as_bool() {
            run_command(hwnd, choice.0 as u16);
        }
        let _ = DestroyMenu(menu);
    }
}

/// Carry out a menu/shortcut command id.
fn run_command(hwnd: HWND, id: u16) {
    match id {
        CMD_COPY_ALL => do_copy_all(hwnd),
        CMD_COPY_SESSION => do_copy_session(hwnd),
        CMD_TOGGLE_THEME => do_toggle_theme(hwnd),
        CMD_EXPORT => do_export(hwnd),
        CMD_CLOSE => {
            // SAFETY: destroying our own window.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
            }
        }
        _ => {}
    }
}

/// Window procedure: paint the Markdown, scroll it, and free state on close.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SIZE => {
            with_viewer(hwnd, |viewer| {
                let mut client = RECT::default();
                // SAFETY: geometry query on our own window.
                unsafe {
                    let _ = GetClientRect(hwnd, &mut client);
                }
                viewer.relayout(client.right - client.left);
            });
            update_scrollbar(hwnd);
            // SAFETY: repainting our own window.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            return LRESULT(0);
        }
        WM_VSCROLL => {
            let code = (wparam.0 & 0xFFFF) as i32;
            let pos = ((wparam.0 >> 16) & 0xFFFF) as i32;
            with_viewer(hwnd, |viewer| {
                let line = viewer.line_height(VisualKind::Body);
                let max = (viewer.content_h - viewer.view_h).max(0);
                match code {
                    x if x == SB_LINEUP.0 as i32 => viewer.scroll = (viewer.scroll - line).max(0),
                    x if x == SB_LINEDOWN.0 as i32 => viewer.scroll = (viewer.scroll + line).min(max),
                    x if x == SB_PAGEUP.0 as i32 => {
                        viewer.scroll = (viewer.scroll - viewer.view_h).max(0)
                    }
                    x if x == SB_PAGEDOWN.0 as i32 => {
                        viewer.scroll = (viewer.scroll + viewer.view_h).min(max)
                    }
                    x if x == SB_TOP.0 as i32 => viewer.scroll = 0,
                    x if x == SB_BOTTOM.0 as i32 => viewer.scroll = max,
                    x if x == SB_THUMBTRACK.0 as i32 || x == SB_THUMBPOSITION.0 as i32 => {
                        viewer.scroll = pos.clamp(0, max)
                    }
                    _ => {}
                }
            });
            update_scrollbar(hwnd);
            // SAFETY: repainting our own window.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as u16) as i16 as i32;
            let step = with_viewer_line(hwnd);
            scroll_by(hwnd, -delta * step / 120);
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let key = (wparam.0 & 0xFFFF) as u32;
            // SAFETY: reading the async modifier state for our own shortcuts.
            let (ctrl, shift) = unsafe {
                use windows::Win32::UI::Input::KeyboardAndMouse::{
                    GetKeyState, VK_CONTROL, VK_SHIFT,
                };
                (
                    GetKeyState(VK_CONTROL.0 as i32) < 0,
                    GetKeyState(VK_SHIFT.0 as i32) < 0,
                )
            };
            // Shortcuts: Ctrl+C copy all, Ctrl+Shift+C copy session, Ctrl+S export,
            // T toggles the theme, Esc closes. Plain C/S/T keys keep scrolling.
            if ctrl && key == 0x43 {
                if shift {
                    do_copy_session(hwnd);
                } else {
                    do_copy_all(hwnd);
                }
                return LRESULT(0);
            }
            if ctrl && key == 0x53 {
                do_export(hwnd);
                return LRESULT(0);
            }
            if !ctrl && key == 0x54 {
                do_toggle_theme(hwnd);
                return LRESULT(0);
            }
            if key == 0x1B {
                run_command(hwnd, CMD_CLOSE);
                return LRESULT(0);
            }
            let step = with_viewer_line(hwnd);
            with_viewer(hwnd, |viewer| {
                let max = (viewer.content_h - viewer.view_h).max(0);
                if key == VK_UP.0 as u32 {
                    viewer.scroll = (viewer.scroll - step).max(0);
                } else if key == VK_DOWN.0 as u32 {
                    viewer.scroll = (viewer.scroll + step).min(max);
                } else if key == VK_PRIOR.0 as u32 {
                    viewer.scroll = (viewer.scroll - viewer.view_h).max(0);
                } else if key == VK_NEXT.0 as u32 {
                    viewer.scroll = (viewer.scroll + viewer.view_h).min(max);
                } else if key == VK_HOME.0 as u32 {
                    viewer.scroll = 0;
                } else if key == VK_END.0 as u32 {
                    viewer.scroll = max;
                }
            });
            update_scrollbar(hwnd);
            // SAFETY: repainting our own window.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            return LRESULT(0);
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            run_command(hwnd, id);
            return LRESULT(0);
        }
        WM_CONTEXTMENU => {
            show_context_menu(hwnd);
            return LRESULT(0);
        }
        WM_SETTINGCHANGE => {
            // The Windows app theme may have flipped while we follow the system.
            apply_system_theme(hwnd);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
            with_viewer(hwnd, |viewer| {
                if dpi > 0 {
                    viewer.dpi_scale = dpi as f32 / 96.0;
                    viewer.padding = scale_gap(PADDING, viewer.dpi_scale);
                    viewer.header_h = scale_gap(HEADER_H, viewer.dpi_scale);
                }
            });
            // SAFETY: applying the system-suggested rectangle for the new DPI.
            unsafe {
                let rect = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER,
                );
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            // Ease the flick switch knob toward its target; repaint only while moving.
            let mut moving = false;
            with_viewer(hwnd, |viewer| {
                if (viewer.switch_anim - viewer.switch_target).abs() > 0.01 {
                    let step = 0.25;
                    if viewer.switch_anim < viewer.switch_target {
                        viewer.switch_anim = (viewer.switch_anim + step).min(viewer.switch_target);
                    } else {
                        viewer.switch_anim = (viewer.switch_anim - step).max(viewer.switch_target);
                    }
                    moving = true;
                } else if viewer.switch_anim != viewer.switch_target {
                    viewer.switch_anim = viewer.switch_target;
                    moving = true;
                }
            });
            if moving {
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
            let on_switch = {
                let mut hit = false;
                with_viewer(hwnd, |viewer| {
                    let mut client = RECT::default();
                    // SAFETY: geometry query on our own window.
                    unsafe {
                        let _ = GetClientRect(hwnd, &mut client);
                    }
                    let sw = switch_rect(client.right - client.left, viewer.header_h, viewer.padding);
                    hit = x >= sw.left && x < sw.right && y >= sw.top && y < sw.bottom;
                });
                hit
            };
            if on_switch {
                do_toggle_theme(hwnd);
                return LRESULT(0);
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
                let _ = KillTimer(Some(hwnd), TIMER_SWITCH);
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: the last message for the window; free the per-window state.
            unsafe {
                let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if raw != 0 {
                    let _ = Box::from_raw(raw as *mut Viewer);
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

fn with_viewer_line(hwnd: HWND) -> i32 {
    let mut line = 20;
    with_viewer(hwnd, |viewer| {
        line = viewer.line_height(VisualKind::Body);
    });
    line.max(1)
}

/// Paint the header's file-name label, ellipsized before it reaches the switch.
unsafe fn paint_header_title(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    file_name: &str,
    theme: &Theme,
    view_w: i32,
    padding: i32,
    header_h: i32,
    dpi_scale: f32,
) {
    use windows::Win32::Graphics::Gdi::{DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER};
    // SAFETY: the caller holds a valid paint DC; the font is released below.
    unsafe {
        let height = scale_gap(SESSION_FONT, dpi_scale);
        let font = CreateFontW(
            -height,
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
        let previous = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(theme.session));
        let mut wide: Vec<u16> = file_name.encode_utf16().collect();
        let mut rect = RECT {
            left: padding,
            top: 0,
            right: view_w - padding - SWITCH_W - 12,
            bottom: header_h,
        };
        if rect.right > rect.left {
            DrawTextW(
                hdc,
                &mut wide,
                &mut rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }
        SelectObject(hdc, previous);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
}

/// Paint the flick theme switch: a rounded track with a sliding knob.
/// `progress` 0 = dark/left, 1 = light/right; the timer eases it toward the target.
unsafe fn paint_switch(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    track: &RECT,
    progress: f32,
    mode: ViewerTheme,
) {
    // SAFETY: the caller holds a valid paint DC.
    unsafe {
        let light = mode == ViewerTheme::Light;
        let track_color = if light { 0x0043_9B5F } else { 0x0044_4444 };
        let brush = CreateSolidBrush(COLORREF(track_color));
        let track_h = track.bottom - track.top;
        FillRect(
            hdc,
            &RECT {
                left: track.left + track_h / 2,
                top: track.top,
                right: track.right - track_h / 2,
                bottom: track.bottom,
            },
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
        draw_glyph(hdc, glyph, gx, track.top + 2, gx + track_h - 6, track_h - 4);
        // Sliding knob.
        let knob_x = switch_knob_x(track, progress);
        let ky = track.top + (track_h - SWITCH_KNOB) / 2;
        let knob = CreateSolidBrush(COLORREF(0x00FF_FFFF));
        let prev = SelectObject(hdc, HGDIOBJ(knob.0));
        let _ = Ellipse(hdc, knob_x, ky, knob_x + SWITCH_KNOB, ky + SWITCH_KNOB);
        SelectObject(hdc, prev);
        let _ = DeleteObject(HGDIOBJ(knob.0));
    }
}

/// Draw a small white glyph (sun/moon) inside the switch track.
unsafe fn draw_glyph(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    glyph: &str,
    x: i32,
    y: i32,
    right: i32,
    height: i32,
) {
    use windows::Win32::Graphics::Gdi::{DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER};
    // SAFETY: the caller holds a valid paint DC; the font is released below.
    unsafe {
        let font = CreateFontW(
            -(height.max(8)),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
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
        let previous = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00FF_FFFF));
        let mut wide: Vec<u16> = glyph.encode_utf16().collect();
        let mut rect = RECT { left: x, top: y, right, bottom: y + height };
        DrawTextW(hdc, &mut wide, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(hdc, previous);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
}

/// Paint the visible visual lines with per-kind fonts and colours.
unsafe fn paint(hwnd: HWND) {
    // SAFETY: all GDI calls below operate on the DC returned by BeginPaint.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            let _ = EndPaint(hwnd, &ps);
            return;
        }

        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        // Snapshot theme + geometry first (cheap), then clone only the visible
        // slice of lines: a full-day transcript can hold thousands of lines and
        // cloning all of them on every WM_PAINT wastes memory bandwidth.
        let (theme, theme_mode, scroll, padding, header_h, switch_anim, dpi_scale, kinds, file_name) = {
            let mut snapshot = (DARK, ViewerTheme::Dark, 0, 0, 0, 0.0, 1.0f32, Vec::new(), String::new());
            with_viewer(hwnd, |viewer| {
                snapshot = (
                    viewer.theme,
                    viewer.theme_mode,
                    viewer.scroll,
                    viewer.padding,
                    viewer.header_h,
                    viewer.switch_anim,
                    viewer.dpi_scale,
                    viewer.lines.iter().map(|l| l.kind).collect(),
                    viewer.file_name.clone(),
                );
            });
            snapshot
        };
        let background = CreateSolidBrush(COLORREF(theme.bg));
        FillRect(hdc, &client, background);
        let _ = DeleteObject(HGDIOBJ(background.0));

        let view_h = client.bottom - client.top;
        let view_w = client.right - client.left;

        // Header bar: file name on the left, flick theme switch on the right.
        let header_rect = RECT { left: 0, top: 0, right: view_w, bottom: header_h };
        let header_bg = CreateSolidBrush(COLORREF(if theme == DARK { 0x0026_2626 } else { 0x00EF_EFEF }));
        FillRect(hdc, &header_rect, header_bg);
        let _ = DeleteObject(HGDIOBJ(header_bg.0));
        let header_rule = CreateSolidBrush(COLORREF(theme.rule));
        let rule_rect = RECT { left: 0, top: header_h - 1, right: view_w, bottom: header_h };
        FillRect(hdc, &rule_rect, header_rule);
        let _ = DeleteObject(HGDIOBJ(header_rule.0));
        paint_header_title(hdc, &file_name, &theme, view_w, padding, header_h, dpi_scale);
        paint_switch(hdc, &switch_rect(view_w, header_h, padding), switch_anim, theme_mode);

        let gap = scale_gap(LINE_GAP_EXTRA, dpi_scale);
        let para = scale_gap(PARAGRAPH_GAP, dpi_scale) / 2;
        let line_h = |kind: VisualKind| kind.font_height(dpi_scale) + gap;
        let extra_h =
            |kind: VisualKind| if matches!(kind, VisualKind::Title | VisualKind::Session) { para } else { 0 };
        // Walk the cheap kind list to find the visible window [start, end).
        // Lines start below the header.
        let mut y = header_h + padding - scroll;
        let mut start = 0usize;
        for (i, kind) in kinds.iter().enumerate() {
            let h = line_h(*kind) + extra_h(*kind);
            if y + h < header_h {
                start = i + 1;
            }
            y += h;
            if y > view_h {
                break;
            }
        }
        let mut y = header_h + padding - scroll;
        for kind in kinds.iter().take(start) {
            y += line_h(*kind) + extra_h(*kind);
        }
        let mut end = start;
        let mut scan = y;
        while end < kinds.len() && scan <= view_h {
            scan += line_h(kinds[end]) + extra_h(kinds[end]);
            end += 1;
        }
        let visible: Vec<VisualLine> = {
            let mut out = Vec::new();
            with_viewer(hwnd, |viewer| {
                out = viewer.lines[start..end.min(viewer.lines.len())].to_vec();
            });
            out
        };

        let mut y = y;
        for line in &visible {
            let height = line.kind.font_height(dpi_scale) + scale_gap(LINE_GAP_EXTRA, dpi_scale);
            let extra = if matches!(line.kind, VisualKind::Title | VisualKind::Session) {
                scale_gap(PARAGRAPH_GAP, dpi_scale) / 2
            } else {
                0
            };
            paint_line(hdc, line, &theme, client.right, padding, y, height, dpi_scale);
            y += height + extra;
            if y > view_h {
                break;
            }
        }

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Paint one visual line; folded utterances get a dim timestamp plus bright body.
/// Draw one run of text and return the x where it ends, for chaining two-tone lines.
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
            w!("Segoe UI"),
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

unsafe fn paint_line(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    line: &VisualLine,
    theme: &Theme,
    client_right: i32,
    padding: i32,
    y: i32,
    height: i32,
    dpi_scale: f32,
) {
    let right = client_right - padding;
    let x0 = padding + line.indent;
    let height_px = line.kind.font_height(dpi_scale);
    // SAFETY: the caller holds a valid paint DC.
    unsafe {
        match line.kind {
            VisualKind::Title | VisualKind::Session => {
                draw_run(
                    hdc,
                    &line.text,
                    x0,
                    y,
                    right,
                    height,
                    height_px,
                    FW_BOLD.0 as i32,
                    line.kind.color(theme),
                );
            }
            VisualKind::UtteranceBody => {
                let (stamp, body) = split_folded(&line.text);
                if body.is_empty() {
                    draw_run(hdc, &stamp, x0, y, right, height, height_px, FW_NORMAL.0 as i32, theme.stamp);
                } else {
                    let after =
                        draw_run(hdc, &stamp, x0, y, right, height, height_px, FW_NORMAL.0 as i32, theme.stamp);
                    draw_run(
                        hdc,
                        &format!("   {body}"),
                        after,
                        y,
                        right,
                        height,
                        height_px,
                        FW_NORMAL.0 as i32,
                        theme.body,
                    );
                }
            }
            VisualKind::Stamp => {
                draw_run(hdc, &line.text, x0, y, right, height, height_px, FW_NORMAL.0 as i32, theme.stamp);
            }
            VisualKind::Body => {
                draw_run(hdc, &line.text, x0, y, right, height, height_px, FW_NORMAL.0 as i32, theme.body);
            }
            VisualKind::Rule => {
                draw_run(hdc, &line.text, x0, y, right, height, height_px, FW_NORMAL.0 as i32, theme.rule);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Transcript — 2025-06-01\n\n## Session 10:00 — English\n\n**10:00:01**  hello world\n\nsome note\n";

    #[test]
    fn parses_the_writer_format() {
        let rows = parse_markdown(SAMPLE);
        assert!(matches!(rows[0], Row::Title(_)));
        assert!(matches!(rows[2], Row::Session(_)));
        let utterance = rows.iter().find_map(|r| match r {
            Row::Utterance { time, body } => Some((time.clone(), body.clone())),
            _ => None,
        });
        assert_eq!(
            utterance,
            Some(("10:00:01".to_string(), "hello world".to_string()))
        );
        assert!(rows.iter().any(|r| matches!(r, Row::Body(_))));
    }

    #[test]
    fn non_timestamp_brackets_survive() {
        let rows = parse_markdown("**10:00:01** [Musik] halo\n");
        assert_eq!(
            rows[0],
            Row::Utterance {
                time: "10:00:01".to_string(),
                body: "[Musik] halo".to_string(),
            }
        );
    }

    #[test]
    fn folds_and_splits_utterances_for_two_tone_painting() {
        let rows = vec![Row::Utterance {
            time: "10:00:01".to_string(),
            body: "hello world".to_string(),
        }];
        let lines = layout_rows(&rows, 600, 1.0);
        assert!(!lines.is_empty());
        assert!(lines[0].text.starts_with("10:00:01"));
        let (stamp, body) = split_folded(&lines[0].text);
        assert_eq!(stamp, "10:00:01");
        assert_eq!(body, "hello world");
    }

    #[test]
    fn wrap_keeps_every_word() {
        let lines = wrap_text("alpha beta gamma delta", 14, 80);
        assert!(lines.len() > 1);
        assert_eq!(lines.join(" "), "alpha beta gamma delta");
    }

    #[test]
    fn empty_files_show_a_placeholder() {
        assert_eq!(
            parse_markdown(""),
            vec![Row::Body("(empty transcript)".to_string())]
        );
    }

    #[test]
    fn copy_all_keeps_the_original_markdown() {
        assert_eq!(build_copy_all(SAMPLE), SAMPLE);
    }

    #[test]
    fn copy_last_session_returns_only_the_final_block() {
        let raw = "# Transcript — 2025-06-01\n\n## Session 10:00 — English\n\n**10:00:01**  first\n\n## Session 11:00 — English\n\n**11:00:01**  second\n";
        let copied = build_copy_last_session(raw);
        assert!(copied.contains("second"));
        assert!(!copied.contains("first"), "got: {copied}");
        assert!(copied.contains("# Transcript"), "the day title rides along");
    }

    #[test]
    fn copy_last_session_skips_the_full_text_section() {
        let raw = "# Transcript — 2025-06-01\n\n## Session 1 — 10:00 — English\n\n**10:00:01**  first\n\n## Session 2 — 11:00 — English\n\n**11:00:01**  second\n\n## Full text\n\nfirst\n\nsecond\n";
        let copied = build_copy_last_session(raw);
        assert!(copied.contains("second"), "got: {copied}");
        assert!(!copied.contains("first"), "got: {copied}");
        assert!(!copied.contains("Full text"), "the combined section never rides along, got: {copied}");
    }

    #[test]
    fn copy_last_session_without_headers_returns_everything() {
        assert_eq!(build_copy_last_session("just a note\n"), "just a note\n");
    }

    #[test]
    fn both_themes_keep_body_text_readable() {
        for theme in [DARK, LIGHT] {
            let ratio = contrast_ratio(theme.body, theme.bg);
            assert!(
                ratio >= 4.5,
                "body/bg contrast {ratio:.1} is below 4.5:1 for {theme:?}"
            );
        }
    }

    #[test]
    fn theme_modes_resolve_to_their_palettes() {
        assert_eq!(theme_for(ViewerTheme::Dark), DARK);
        assert_eq!(theme_for(ViewerTheme::Light), LIGHT);
        // System resolves to one of the two; either is fine for the test.
        assert!(matches!(theme_for(ViewerTheme::System), DARK | LIGHT));
    }

    #[test]
    fn the_flick_switch_sits_inside_the_header() {
        let sw = switch_rect(760, 52, 20);
        assert_eq!((sw.right - sw.left, sw.bottom - sw.top), (SWITCH_W, SWITCH_H));
        assert!(sw.right <= 760 - 20);
        assert!(sw.top >= 0 && sw.bottom <= 52);
    }

    #[test]
    fn the_switch_knob_travels_the_track() {
        let track = RECT { left: 0, top: 0, right: 52, bottom: 26 };
        let left = switch_knob_x(&track, 0.0);
        let right = switch_knob_x(&track, 1.0);
        assert_eq!(left, 3);
        assert!(right > left);
        assert_eq!(right + SWITCH_KNOB, 49);
        assert_eq!(switch_target_for(ViewerTheme::Light), 1.0);
        assert_eq!(switch_target_for(ViewerTheme::Dark), 0.0);
    }
}

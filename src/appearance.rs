//! Windows appearance and non-activating floating-window placement.
use crate::config::{Config, IndicatorPosition, ViewerTheme};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::{
        Dwm::{DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute},
        Gdi::{
            CreateRectRgn, CreateRoundRectRgn, DeleteObject, EqualRgn, GetMonitorInfoW,
            GetWindowRgn, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
            SetWindowRgn,
        },
    },
    UI::{
        HiDpi::GetDpiForWindow,
        WindowsAndMessaging::{
            GetWindowRect, HWND_TOPMOST, IsZoomed, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowPos,
        },
    },
};
pub fn dark(theme: ViewerTheme) -> bool {
    if theme != ViewerTheme::System {
        return theme == ViewerTheme::Dark;
    }
    use windows::{
        Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        core::w,
    };
    let mut value = 1u32;
    let mut size = 4;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut _ as *mut _),
            Some(&mut size),
        );
    }
    value == 0
}
pub fn hwnd(window: &slint::Window) -> Option<HWND> {
    window
        .with_winit_window(|w| {
            w.window_handle().ok().and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
                _ => None,
            })
        })
        .flatten()
}
pub fn appearance(window: &slint::Window, config: &Config) -> bool {
    if let Some(h) = hwnd(window) {
        unsafe {
            let dark = dark(config.viewer_theme) as i32;
            let _ = DwmSetWindowAttribute(
                h,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as *const _,
                4,
            );
            // Unsupported versions reject this attribute; the Slint background remains readable.
            let backdrop = if config.reduced_transparency {
                1i32
            } else {
                2i32
            };
            return DwmSetWindowAttribute(
                h,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop as *const _ as *const _,
                4,
            )
            .is_ok();
        }
    }
    false
}
/// Keep the main window rounded through resizing, DPI changes and restore.
pub fn round_main_window(window: &slint::Window) {
    use slint::winit_030::{EventResult, winit::event::WindowEvent};
    window.on_winit_window_event(|window, event| {
        if matches!(
            event,
            WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::RedrawRequested
        ) {
            if let Some(h) = hwnd(window) {
                // SAFETY: this handle belongs to the main window on the UI thread.
                let radius = if unsafe { IsZoomed(h).as_bool() } {
                    0
                } else {
                    10
                };
                clip_window_corners(h, radius);
            }
        }
        EventResult::Propagate
    });
}

pub fn place_floating(window: &slint::Window, anchor: &slint::Window, config: &Config) {
    let Some(h) = hwnd(window) else {
        return;
    };
    let anchor = hwnd(anchor).unwrap_or(h);
    unsafe {
        let monitor = MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let work = usable_work_area(info);
        let dpi = GetDpiForWindow(h).max(96);
        let margin = config.indicator_margin * dpi as i32 / 96;
        let mut rect = RECT::default();
        let _ = GetWindowRect(h, &mut rect);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let x = match config.indicator_position {
            IndicatorPosition::TopLeft | IndicatorPosition::BottomLeft => work.left + margin,
            _ => work.right - width - margin,
        };
        let y = match config.indicator_position {
            IndicatorPosition::TopLeft | IndicatorPosition::TopRight => work.top + margin,
            _ => work.bottom - height - margin,
        };
        let _ = SetWindowPos(
            h,
            Some(HWND_TOPMOST),
            x.max(work.left),
            y.max(work.top),
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE,
        );
    }
    clip_window_corners(h, 18);
}

/// Apply a logical-pixel radius to the native window shape.
/// Clip the native window too, so an opaque renderer cannot fill the outer corners.
fn clip_window_corners(h: HWND, radius: u32) {
    // SAFETY: geometry and regions for our own window. Windows takes ownership of
    // the new region only when SetWindowRgn succeeds.
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(h, &mut rect).is_err() {
            return;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return;
        }
        let diameter = (2 * radius * GetDpiForWindow(h).max(96) + 48) / 96;
        let region = if radius == 0 {
            CreateRectRgn(0, 0, width + 1, height + 1)
        } else {
            CreateRoundRectRgn(
                0,
                0,
                width + 1,
                height + 1,
                diameter as i32,
                diameter as i32,
            )
        };
        if region.is_invalid() {
            return;
        }
        // Keep the existing region on repeated redraws and status updates when
        // its shape matches to avoid repeatedly invalidating the window.
        let current = CreateRectRgn(0, 0, 0, 0);
        let unchanged = if current.is_invalid() {
            false
        } else {
            let matches = GetWindowRgn(h, current).0 != 0 && EqualRgn(current, region).as_bool();
            let _ = DeleteObject(HGDIOBJ(current.0));
            matches
        };
        if unchanged || SetWindowRgn(h, Some(region), true) == 0 {
            let _ = DeleteObject(HGDIOBJ(region.0));
        }
    }
}

fn usable_work_area(info: MONITORINFO) -> RECT {
    use windows::{
        Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowRect},
        core::w,
    };
    let mut work = info.rcWork;
    unsafe {
        for class in [w!("Shell_TrayWnd"), w!("Shell_SecondaryTrayWnd")] {
            let mut previous = None;
            while let Ok(h) = FindWindowExW(None, previous, class, None) {
                previous = Some(h);
                let mut bar = RECT::default();
                if GetWindowRect(h, &mut bar).is_err() {
                    continue;
                }
                if bar.right <= info.rcMonitor.left
                    || bar.left >= info.rcMonitor.right
                    || bar.bottom <= info.rcMonitor.top
                    || bar.top >= info.rcMonitor.bottom
                {
                    continue;
                }
                if bar.right - bar.left > bar.bottom - bar.top {
                    let size = bar.bottom - bar.top;
                    if bar.bottom >= info.rcMonitor.bottom {
                        work.bottom = work.bottom.min(info.rcMonitor.bottom - size);
                    } else if bar.top <= info.rcMonitor.top {
                        work.top = work.top.max(info.rcMonitor.top + size);
                    }
                } else {
                    let size = bar.right - bar.left;
                    if bar.right >= info.rcMonitor.right {
                        work.right = work.right.min(info.rcMonitor.right - size);
                    } else if bar.left <= info.rcMonitor.left {
                        work.left = work.left.max(info.rcMonitor.left + size);
                    }
                }
            }
        }
    }
    work
}

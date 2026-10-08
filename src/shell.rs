//! Handing a path to the shell: a folder opens in Explorer, a file in its default handler.
//!
//! The `open` crate is deliberately not used here. Without its `shellexecute-on-windows` feature
//! it spawns PowerShell and reports success as soon as that process starts, so a failure is
//! invisible — which is exactly how the tray's "open" items managed to do nothing in silence.
//! `ShellExecuteW` returns a real error code, and Explorer is tried as a last resort.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

/// Errors below this value come from `ShellExecuteW`; anything above is a handle.
///
/// <https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecutew>
const SHELL_EXECUTE_LAST_ERROR: isize = 32;

/// Open `path` with the shell, reporting why it could not be opened.
pub fn reveal(path: &Path) -> Result<()> {
    let file = wide(path.as_os_str());
    // SAFETY: ShellExecuteW can delegate to a shell extension, which wants COM on this thread.
    // An apartment that is already initialised (tao initialises OLE on its own thread) makes
    // this fail harmlessly, so the result is ignored.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // SAFETY: `file` is NUL-terminated and outlives the call; the other strings are literals.
    let result = unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result.0 as isize;
    if code > SHELL_EXECUTE_LAST_ERROR {
        return Ok(());
    }
    // Explorer is always installed, and it opens both folders and files.
    explorer(path).map_err(|fallback| {
        anyhow::anyhow!(
            "ShellExecuteW returned {code} for {}; the explorer.exe fallback failed too: {fallback}",
            path.display()
        )
    })
}

/// Last resort: ask Explorer to open the path.
fn explorer(path: &Path) -> Result<()> {
    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .with_context(|| format!("starting explorer.exe for {}", path.display()))?;
    Ok(())
}

/// UTF-16 with a terminating NUL, for the wide Win32 APIs.
fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_terminates_the_string() {
        let encoded = wide(OsStr::new("a"));
        assert_eq!(encoded, vec!['a' as u16, 0]);
    }
}

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
    let bytes = std::mem::size_of_val(wide.as_slice());
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

//! Global hotkey that toggles listening.
//!
//! The hotkey is registered on its own thread with a null window, so it fires while the app sits
//! in the tray with nothing focused, and it never depends on the tray's event loop.
//!
//! Windows keeps a number of `Win`+key combinations for itself (`Win+P` opens the display
//! projection pane, for example), and `RegisterHotKey` refuses those. A fallback combination is
//! therefore tried when the configured one cannot be registered, and whichever one is live is
//! reported back so the tray can display it.

use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Result, anyhow};
use crossbeam_channel::Sender;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
};

use crate::logging::Logger;
use crate::pipeline::Control;

/// Ties `RegisterHotKey` to the `WM_HOTKEY` messages this thread receives.
const HOTKEY_ID: i32 = 0x7A11;

/// A parsed hotkey: modifier bits, a virtual-key code, and how to print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub modifiers: u32,
    pub key: u32,
    pub label: String,
}

/// Parse `"Win+P"`, `"Ctrl+Alt+Space"`, `"Ctrl+Shift+F9"` and similar spellings.
///
/// At least one modifier is required: a bare global key would swallow ordinary typing.
pub fn parse(text: &str) -> Result<HotkeySpec> {
    let mut modifiers = 0u32;
    let mut names: Vec<&str> = Vec::new();
    let mut key: Option<(u32, String)> = None;

    for raw in text.split('+') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => {
                modifiers |= MOD_CONTROL.0;
                names.push("Ctrl");
            }
            "alt" => {
                modifiers |= MOD_ALT.0;
                names.push("Alt");
            }
            "shift" => {
                modifiers |= MOD_SHIFT.0;
                names.push("Shift");
            }
            "win" | "windows" | "super" | "meta" => {
                modifiers |= MOD_WIN.0;
                names.push("Win");
            }
            other => {
                if key.is_some() {
                    anyhow::bail!("hotkey {text:?} names more than one key");
                }
                let (code, display) = parse_key(other)
                    .ok_or_else(|| anyhow!("unknown key {part:?} in hotkey {text:?}"))?;
                key = Some((code, display));
            }
        }
    }

    let (key, key_name) = key.ok_or_else(|| anyhow!("hotkey {text:?} names no key"))?;
    if modifiers == 0 {
        anyhow::bail!("hotkey {text:?} needs at least one modifier (Ctrl, Alt, Shift or Win)");
    }
    names.push(&key_name);
    Ok(HotkeySpec {
        modifiers,
        key,
        label: names.join("+"),
    })
}

/// Map a key name to a virtual-key code and its canonical spelling.
fn parse_key(lower: &str) -> Option<(u32, String)> {
    // Single letters and digits map straight onto their ASCII codes.
    if lower.len() == 1 {
        let ch = lower.chars().next()?;
        if ch.is_ascii_alphabetic() || ch.is_ascii_digit() {
            let upper = ch.to_ascii_uppercase();
            return Some((upper as u32, upper.to_string()));
        }
    }
    let named: &[(&str, u32, &str)] = &[
        ("space", 0x20, "Space"),
        ("spacebar", 0x20, "Space"),
        ("tab", 0x09, "Tab"),
        ("enter", 0x0D, "Enter"),
        ("return", 0x0D, "Enter"),
        ("esc", 0x1B, "Esc"),
        ("escape", 0x1B, "Esc"),
        ("backspace", 0x08, "Backspace"),
        ("insert", 0x2D, "Insert"),
        ("delete", 0x2E, "Delete"),
        ("home", 0x24, "Home"),
        ("end", 0x23, "End"),
        ("pageup", 0x21, "PageUp"),
        ("pagedown", 0x22, "PageDown"),
        ("up", 0x26, "Up"),
        ("down", 0x28, "Down"),
        ("left", 0x25, "Left"),
        ("right", 0x27, "Right"),
    ];
    for (name, code, display) in named {
        if lower == *name {
            return Some((*code, (*display).to_string()));
        }
    }
    // Function keys F1..F24 occupy 0x70..0x87.
    if let Some(digits) = lower.strip_prefix('f')
        && let Ok(number) = digits.parse::<u32>()
        && (1..=24).contains(&number)
    {
        return Some((0x70 + number - 1, format!("F{number}")));
    }
    None
}

/// A registered hotkey (or a record of why there is none).
pub struct Hotkey {
    /// The combination actually registered, if any.
    pub active: Option<HotkeySpec>,
    /// The combination that was asked for, for messages.
    pub requested: String,
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}

impl Hotkey {
    /// Stop pumping messages and unregister the hotkey.
    pub fn shutdown(&mut self) {
        if self.thread_id != 0 {
            // SAFETY: posting WM_QUIT to our own hotkey thread ends its GetMessageW loop.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            self.thread_id = 0;
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Register `primary` (falling back to `fallback`) and send [`Control::Toggle`] on every press.
pub fn spawn(
    primary: &str,
    fallback: &str,
    control: Sender<Control>,
    logger: std::sync::Arc<Logger>,
) -> Hotkey {
    let requested = primary.to_string();
    let primary_spec = parse(primary);
    let fallback_spec = parse(fallback);
    let (ready_tx, ready_rx) = mpsc::channel::<(Option<HotkeySpec>, u32)>();

    let thread = std::thread::Builder::new()
        .name("hotkey".to_string())
        .spawn(move || {
            let mut chosen: Option<HotkeySpec> = None;
            for (candidate, source) in [(primary_spec, "configured"), (fallback_spec, "fallback")] {
                let spec = match candidate {
                    Ok(spec) => spec,
                    Err(err) => {
                        logger.warn(format!("hotkey ({source}) ignored: {err:#}"));
                        continue;
                    }
                };
                match register(&spec) {
                    Ok(()) => {
                        logger.info(format!("hotkey {source} active: {}", spec.label));
                        chosen = Some(spec);
                        break;
                    }
                    Err(err) => {
                        // ERROR_HOTKEY_ALREADY_REGISTERED (1409) is the usual cause: the shell
                        // owns that combination.
                        logger.warn(format!(
                            "hotkey {} is unavailable ({err}), trying the next one",
                            spec.label
                        ));
                    }
                }
            }

            let thread_id = unsafe { GetCurrentThreadId() };
            let _ = ready_tx.send((chosen.clone(), thread_id));

            let Some(spec) = chosen else {
                logger.error(
                    "no global hotkey could be registered; use the tray menu to start listening",
                );
                return;
            };

            let mut msg: MSG = unsafe { std::mem::zeroed() };
            loop {
                // SAFETY: `msg` is a valid, writable MSG for the duration of the call.
                let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if !result.as_bool() {
                    break; // WM_QUIT
                }
                if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                    logger.info(format!("{} pressed", spec.label));
                    if control.send(Control::Toggle).is_err() {
                        break; // the pipeline is gone
                    }
                }
            }

            // SAFETY: the hotkey was registered on this thread, and is released on the same one.
            unsafe {
                let _ = UnregisterHotKey(None, HOTKEY_ID);
            }
            logger.info("hotkey released");
        })
        .expect("spawning the hotkey thread");

    let (active, thread_id) = ready_rx.recv().unwrap_or((None, 0));
    Hotkey {
        active,
        requested,
        thread: Some(thread),
        thread_id,
    }
}

fn register(spec: &HotkeySpec) -> Result<()> {
    // MOD_NOREPEAT stops held keys from toggling twice.
    let modifiers = HOT_KEY_MODIFIERS(spec.modifiers | MOD_NOREPEAT.0);
    // SAFETY: a null window registers the hotkey against this thread's message queue.
    unsafe { RegisterHotKey(None, HOTKEY_ID, modifiers, spec.key) }.map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_requested_combination() {
        let spec = parse("Win+P").unwrap();
        assert_eq!(spec.key, 'P' as u32);
        assert_eq!(spec.modifiers, MOD_WIN.0);
        assert_eq!(spec.label, "Win+P");
    }

    #[test]
    fn parses_letter_and_function_and_named_keys() {
        assert_eq!(parse("Ctrl+Alt+Space").unwrap().label, "Ctrl+Alt+Space");
        assert_eq!(parse("ctrl+shift+f9").unwrap().label, "Ctrl+Shift+F9");
        assert_eq!(parse("Ctrl+Alt+Enter").unwrap().key, 0x0D);
        assert_eq!(parse("Alt+5").unwrap().key, '5' as u32);
    }

    #[test]
    fn modifier_bits_are_combined_not_replaced() {
        let spec = parse("Ctrl+Alt+Shift+Win+K").unwrap();
        assert_eq!(
            spec.modifiers,
            MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0 | MOD_WIN.0
        );
    }

    #[test]
    fn rejects_a_bare_key_so_typing_is_not_swallowed() {
        assert!(parse("P").is_err());
        assert!(parse("Space").is_err());
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("Ctrl+Banana").is_err());
        assert!(parse("Ctrl+Alt").is_err());
        assert!(parse("Ctrl+P+Q").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn function_key_range_is_enforced() {
        assert_eq!(parse("Ctrl+F1").unwrap().key, 0x70);
        assert_eq!(parse("Ctrl+F24").unwrap().key, 0x87);
        assert!(parse("Ctrl+F25").is_err());
    }
}

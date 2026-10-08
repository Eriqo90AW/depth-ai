//! English transcription through the linked Cactus "needle" C engine.
//!
//! `needle.h` documents one process-global, non-thread-safe speech model, so every call is
//! serialised behind a mutex and only one engine instance is ever created.
//!
//! Verified against the shipped header (`vendor/needle/windows-x86_64/needle.h`):
//!
//! ```c
//! int  needle_load(const unsigned char* cact, unsigned long long n);
//! int  needle_models(void);
//! const char* needle_last_error(void);
//! int  needle_init(const char* system_prompt, const char* tools_json, const char* tool_index_path);
//! int  needle_transcribe(const float* pcm, int samples, const char* language, const char* keywords,
//!                        int word_timestamps, char* out, int out_capacity);
//! ```

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};

use super::{AsrEngine, AsrResult, MAX_SAMPLES, keywords_blob, parse_result};
use crate::logging::Logger;

#[link(name = "needle", kind = "static")]
unsafe extern "C" {
    fn needle_load(cact: *const u8, n: u64) -> c_int;
    fn needle_models() -> c_int;
    fn needle_last_error() -> *const c_char;
    fn needle_transcribe(
        pcm: *const f32,
        samples: c_int,
        language: *const c_char,
        keywords: *const c_char,
        word_timestamps: c_int,
        out: *mut c_char,
        out_capacity: c_int,
    ) -> c_int;
}

/// Serialises access to the process-global model.
static ENGINE: Mutex<()> = Mutex::new(());

/// First output buffer size; grows if the reply does not fit.
const INITIAL_CAPACITY: usize = 16 * 1024;
/// Refuse to grow past this, so a broken engine cannot exhaust memory.
const MAX_CAPACITY: usize = 4 * 1024 * 1024;

/// Whistle (English) via the static C engine.
pub struct WhistleEngine {
    language: CString,
    keywords: Option<CString>,
    word_timestamps: bool,
}

impl WhistleEngine {
    /// Load `whistle.cact` into the engine.
    pub fn new(model: &Path, logger: &Arc<Logger>) -> Result<Self> {
        let bytes = std::fs::read(model)
            .with_context(|| format!("reading Whistle model {}", model.display()))?;
        let _guard = lock();
        // SAFETY: `bytes` outlives the call and `n` matches its length exactly.
        let rc = unsafe { needle_load(bytes.as_ptr(), bytes.len() as u64) };
        if rc < 0 {
            let err = last_error();
            anyhow::bail!("needle_load failed for {}: {err}", model.display());
        }
        let models = unsafe { needle_models() };
        logger.info(format!(
            "Whistle loaded from {} ({:.1} MB, {} bytes, models bitmask {models:#b})",
            model.display(),
            bytes.len() as f64 / (1024.0 * 1024.0),
            bytes.len()
        ));
        Ok(Self {
            language: CString::new("en").expect("static string"),
            keywords: None,
            word_timestamps: false,
        })
    }

    /// Favour these words and phrases during decoding (newline-separated for the engine).
    pub fn set_keywords(&mut self, keywords: &[String]) {
        self.keywords = keywords_blob(keywords).and_then(|blob| CString::new(blob).ok());
    }

    /// Ask the engine for per-word timings too.
    pub fn set_word_timestamps(&mut self, enabled: bool) {
        self.word_timestamps = enabled;
    }
}

impl AsrEngine for WhistleEngine {
    fn name(&self) -> &'static str {
        "Whistle"
    }

    fn transcribe(&mut self, pcm: &[f32]) -> Result<AsrResult> {
        if pcm.is_empty() {
            return Ok(AsrResult::default());
        }
        let samples = pcm.len().min(MAX_SAMPLES) as c_int;
        let language = self.language.as_ptr();
        let keywords = self
            .keywords
            .as_ref()
            .map_or(std::ptr::null(), |k| k.as_ptr());
        let word_timestamps = c_int::from(self.word_timestamps);

        let _guard = lock();
        let mut capacity = INITIAL_CAPACITY;
        loop {
            let mut out = vec![0u8; capacity];
            let rc = unsafe {
                needle_transcribe(
                    pcm.as_ptr(),
                    samples,
                    language,
                    keywords,
                    word_timestamps,
                    out.as_mut_ptr() as *mut c_char,
                    capacity as c_int,
                )
            };
            if rc < 0 {
                anyhow::bail!("needle_transcribe failed: {}", last_error());
            }
            // The engine writes a NUL-terminated JSON string when it fits.
            let end = out.iter().position(|&b| b == 0);
            match end {
                Some(end) => {
                    let text = String::from_utf8_lossy(&out[..end]);
                    return parse_result(&text);
                }
                None if capacity < MAX_CAPACITY => {
                    capacity *= 4;
                }
                None => anyhow::bail!("needle_transcribe reply exceeded {MAX_CAPACITY} bytes"),
            }
        }
    }
}

fn lock() -> MutexGuard<'static, ()> {
    match ENGINE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Copy the engine's last error into a Rust string.
fn last_error() -> String {
    // SAFETY: the pointer is owned by the engine and valid until the next API call.
    unsafe {
        let ptr = needle_last_error();
        if ptr.is_null() {
            "no error reported".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

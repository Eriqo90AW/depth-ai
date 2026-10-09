//! Indonesian transcription through whisper.cpp (via `whisper-rs`).
//!
//! Whistle covers English, German, French, Spanish, Italian, Dutch and Polish, so Indonesian is
//! served by a Whisper checkpoint instead. This engine is only ever loaded when the tray's
//! language is set to Indonesian, which keeps an English session at 17 MB of model.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{AsrEngine, AsrResult, MAX_SAMPLES, log_result};
use crate::logging::Logger;

/// Whisper (Indonesian) via whisper.cpp.
pub struct WhisperEngine {
    context: WhisperContext,
    language: String,
    threads: i32,
    word_timestamps: bool,
}

impl WhisperEngine {
    /// Load a GGML checkpoint.
    pub fn new(model: &Path, threads: i32, logger: &Arc<Logger>) -> Result<Self> {
        let path = model
            .to_str()
            .context("the Whisper model path is not valid UTF-8")?;
        let context = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .with_context(|| format!("loading the Whisper checkpoint {}", model.display()))?;
        let size = std::fs::metadata(model).map(|m| m.len()).unwrap_or(0);
        logger.info(format!(
            "Whisper loaded from {} ({:.1} MB, {} threads)",
            model.display(),
            size as f64 / (1024.0 * 1024.0),
            if threads > 0 { threads } else { 0 }
        ));
        Ok(Self {
            context,
            language: "id".to_string(),
            threads,
            word_timestamps: false,
        })
    }

    /// Ask for per-word timings as well as text.
    pub fn set_word_timestamps(&mut self, enabled: bool) {
        self.word_timestamps = enabled;
    }
}

impl AsrEngine for WhisperEngine {
    fn name(&self) -> &'static str {
        "Whisper"
    }

    fn transcribe(&mut self, pcm: &[f32]) -> Result<AsrResult> {
        if pcm.is_empty() {
            return Ok(AsrResult::default());
        }
        let samples = &pcm[..pcm.len().min(MAX_SAMPLES)];

        let mut state = self
            .context
            .create_state()
            .context("creating a Whisper decoding state")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(Some(self.language.as_str()));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if self.word_timestamps {
            params.set_token_timestamps(true);
        }

        state
            .full(params, samples)
            .context("running the Whisper decoder")?;

        let count = state.full_n_segments();
        let mut text = String::new();
        let mut words = Vec::new();
        for index in 0..count {
            // whisper-rs 0.16 exposes segments as objects rather than by-text accessor.
            if let Some(segment) = state.get_segment(index)
                && let Ok(chunk) = segment.to_str_lossy()
            {
                text.push_str(&chunk);
                if self.word_timestamps {
                    for token_index in 0..segment.n_tokens() {
                        if let Some(token) = segment.get_token(token_index) {
                            let data = token.token_data();
                            if let Ok(word) = token.to_str_lossy() {
                                if data.t0 >= 0
                                    && data.t1 > data.t0
                                    && !word.starts_with("[_")
                                    && !word.starts_with("<|")
                                {
                                    words.push(super::Word {
                                        word: word.into_owned(),
                                        start: data.t0 as f32 / 100.0,
                                        end: data.t1 as f32 / 100.0,
                                        probability: data.p,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(AsrResult {
            text: text.trim().to_string(),
            language: Some(self.language.clone()),
            words,
            ..Default::default()
        })
    }
}

/// Shared logging helper.
pub(crate) fn log(logger: &Logger, secs: f32, result: &AsrResult) {
    log_result(logger, "Whisper", secs, result);
}

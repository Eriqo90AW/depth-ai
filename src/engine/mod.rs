//! Speech engines behind one trait.
//!
//! English runs on Cactus's Whistle model through the `needle` C engine; Indonesian runs on a
//! Whisper checkpoint, because Whistle covers seven languages and Indonesian is not one of them.
//! Only the engine for the selected language is ever loaded, so an English session pays 17 MB
//! of model instead of 180 MB.

use std::sync::Arc;

use serde::Deserialize;

use crate::config::{Config, Language};
use crate::logging::Logger;

#[cfg(feature = "whistle-sidecar")]
pub mod sidecar;
#[cfg(feature = "whisper")]
pub mod whisper;
#[cfg(feature = "whisper-sidecar")]
pub mod whisper_cli;
#[cfg(feature = "whistle")]
pub mod whistle;

/// One timed word, when the engine reports them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Word {
    #[serde(default)]
    pub word: String,
    #[serde(default)]
    pub start: f32,
    #[serde(default)]
    pub end: f32,
    #[serde(default)]
    pub probability: f32,
}

/// What an engine returns for one utterance.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AsrResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub ttft_ms: f32,
    #[serde(default)]
    pub decode_tps: f32,
    #[serde(default)]
    pub words: Vec<Word>,
}

impl AsrResult {
    /// True when the engine heard nothing worth writing down.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// A lazily loaded speech engine.
pub trait AsrEngine: Send {
    /// Short engine name for the Markdown header and the log.
    fn name(&self) -> &'static str;
    fn reusable_after_recording(&self) -> bool {
        true
    }
    fn status(&self) -> String {
        self.name().into()
    }
    fn take_notices(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// Transcribe 16 kHz mono PCM (at most 30 s) into text.
    fn transcribe(&mut self, pcm: &[f32]) -> anyhow::Result<AsrResult>;

    /// Publish revisable text without requiring every backend to stream.
    fn transcribe_with_updates(
        &mut self,
        pcm: &[f32],
        update: &mut dyn FnMut(&str),
    ) -> anyhow::Result<AsrResult> {
        let result = self.transcribe(pcm)?;
        if !result.is_empty() {
            update(&result.text);
        }
        Ok(result)
    }
}

/// 30 s at 16 kHz, the hard limit both engines document.
pub const MAX_SAMPLES: usize = 30 * 16_000;

/// Load the engine that serves `language`.
pub fn load(
    language: Language,
    config: &Config,
    logger: &Arc<Logger>,
) -> anyhow::Result<Box<dyn AsrEngine>> {
    match language {
        Language::En => load_english(config, logger),
        Language::Id => load_indonesian(config, logger),
    }
}

#[cfg(feature = "whistle")]
fn load_english(config: &Config, logger: &Arc<Logger>) -> anyhow::Result<Box<dyn AsrEngine>> {
    let path = config
        .resolve_model(
            &config.whistle_model,
            "Whistle",
            "Run `python scripts/fetch_assets.py whistle`.",
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if config.whistle_decoder_depth.is_some() {
        logger.warn(
            "whistle_decoder_depth is only honoured by the whistle-sidecar build; \
             the linked engine always runs the model's full decoder depth",
        );
    }
    let mut engine = whistle::WhistleEngine::new(&path, logger)?;
    engine.set_word_timestamps(config.word_timestamps || config.detect_speakers);
    engine.set_keywords(&config.keywords);
    Ok(Box::new(engine))
}

#[cfg(all(feature = "whistle-sidecar", not(feature = "whistle")))]
fn load_english(config: &Config, logger: &Arc<Logger>) -> anyhow::Result<Box<dyn AsrEngine>> {
    Ok(Box::new(sidecar::SidecarEngine::new(config, logger)?))
}

#[cfg(not(any(feature = "whistle", feature = "whistle-sidecar")))]
fn load_english(_config: &Config, _logger: &Arc<Logger>) -> anyhow::Result<Box<dyn AsrEngine>> {
    anyhow::bail!("this build has no English engine; rebuild with the `whistle` feature")
}

/// Indonesian through the prebuilt whisper.cpp CLI (the default: needs no native build).
#[cfg(feature = "whisper-sidecar")]
fn load_indonesian(config: &Config, logger: &Arc<Logger>) -> anyhow::Result<Box<dyn AsrEngine>> {
    Ok(Box::new(whisper_cli::WhisperCliEngine::new(
        config, logger,
    )?))
}

/// Indonesian compiled in-process, when the `whisper` feature is used without the sidecar.
#[cfg(all(feature = "whisper", not(feature = "whisper-sidecar")))]
fn load_indonesian(config: &Config, logger: &Arc<Logger>) -> anyhow::Result<Box<dyn AsrEngine>> {
    let path = config
        .resolve_model(
            &config.whisper_model,
            "Whisper",
            "Run `python scripts/fetch_assets.py model --size small`.",
        )
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut engine = whisper::WhisperEngine::new(&path, config.whisper_threads, logger)?;
    engine.set_word_timestamps(config.word_timestamps || config.detect_speakers);
    Ok(Box::new(engine))
}

#[cfg(not(any(feature = "whisper", feature = "whisper-sidecar")))]
fn load_indonesian(_config: &Config, _logger: &Arc<Logger>) -> anyhow::Result<Box<dyn AsrEngine>> {
    anyhow::bail!("this build has no Indonesian engine; rebuild with the `whisper-sidecar` feature")
}

/// The engine name a language will use, without loading it (for menus and headers).
pub fn engine_name(language: Language) -> &'static str {
    match language {
        Language::En => {
            if cfg!(feature = "whistle") {
                "Whistle"
            } else {
                "Whistle (sidecar)"
            }
        }
        Language::Id => {
            if cfg!(feature = "whisper-sidecar") {
                "Whisper (whisper.cpp)"
            } else {
                "Whisper"
            }
        }
    }
}

/// Build the newline-separated keyword list the Cactus engine expects.
#[cfg(any(feature = "whistle", feature = "whistle-sidecar"))]
pub(crate) fn keywords_blob(keywords: &[String]) -> Option<String> {
    if keywords.is_empty() {
        return None;
    }
    let joined = keywords
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Parse an engine's JSON reply, tolerating extra text around it.
pub(crate) fn parse_result(raw: &str) -> anyhow::Result<AsrResult> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(AsrResult::default());
    }
    if let Ok(parsed) = serde_json::from_str::<AsrResult>(trimmed) {
        return Ok(parsed);
    }
    // The CLI can emit progress lines around the JSON object; take the last object.
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start < end
        && let Ok(parsed) = serde_json::from_str::<AsrResult>(&trimmed[start..=end])
    {
        return Ok(parsed);
    }
    anyhow::bail!(
        "engine returned unparsable output: {}",
        truncate(trimmed, 200)
    )
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}…", &text[..max])
    }
}

/// Log a one-line summary of an engine result.
pub(crate) fn log_result(logger: &Logger, engine: &str, secs: f32, result: &AsrResult) {
    if result.is_empty() {
        logger.info(format!("{engine}: {secs:.1}s of audio -> (no speech)"));
    } else {
        logger.info(format!(
            "{engine}: {secs:.1}s of audio -> {} chars, ttft {:.0} ms, {:.0} tok/s",
            result.text.chars().count(),
            result.ttft_ms,
            result.decode_tps
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_json_shape() {
        let raw = r#"{"text":"hello there","language":"en","ttft_ms":11.1,"decode_tps":1319.0}"#;
        let r = parse_result(raw).unwrap();
        assert_eq!(r.text, "hello there");
        assert_eq!(r.language.as_deref(), Some("en"));
        assert!((r.ttft_ms - 11.1).abs() < 0.01);
        assert!(r.words.is_empty());
    }

    #[test]
    fn parses_word_timestamps() {
        let raw = r#"{"text":"a b","language":"en","words":[{"word":"a","start":0.0,"end":0.5,"probability":0.9}]}"#;
        let r = parse_result(raw).unwrap();
        assert_eq!(r.words.len(), 1);
        assert_eq!(r.words[0].word, "a");
    }

    #[test]
    fn silence_is_an_empty_result_not_an_error() {
        let r =
            parse_result(r#"{"text":"","language":"","ttft_ms":0.0,"decode_tps":0.0}"#).unwrap();
        assert!(r.is_empty());
        assert_eq!(r.language.as_deref(), Some(""));
    }

    #[test]
    fn tolerates_surrounding_output() {
        let raw = "loading model...\n{\"text\":\"ok\"}\ndone";
        assert_eq!(parse_result(raw).unwrap().text, "ok");
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_result("not json at all").is_err());
        assert!(parse_result("").unwrap().is_empty());
    }

    #[test]
    fn keywords_blob_joins_and_skips_blanks() {
        let k = vec![
            "Siobhan".to_string(),
            "  ".to_string(),
            "Krzysztof".to_string(),
        ];
        assert_eq!(keywords_blob(&k).as_deref(), Some("Siobhan\nKrzysztof"));
        assert!(keywords_blob(&[]).is_none());
    }
}

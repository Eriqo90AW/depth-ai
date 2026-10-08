//! English transcription by running the bundled `needle.exe` once per utterance.
//!
//! This exists as an escape hatch: it needs no static library and no C++ toolchain, only the
//! `needle.exe` that ships in the same folder as `libneedle.a`. It is slower per utterance
//! (the model is reloaded for every call) but it always links.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use super::{AsrEngine, AsrResult, MAX_SAMPLES, keywords_blob, parse_result};
use crate::config::Config;
use crate::logging::Logger;

/// A console child of this GUI-subsystem process would otherwise flash a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whistle through the prebuilt command line runner.
pub struct SidecarEngine {
    exe: PathBuf,
    model: PathBuf,
    language: String,
    keywords: Option<PathBuf>,
    scratch: PathBuf,
    word_timestamps: bool,
    depth: Option<u8>,
    logger: std::sync::Arc<Logger>,
}

impl SidecarEngine {
    pub fn new(config: &Config, logger: &std::sync::Arc<Logger>) -> Result<Self> {
        let exe = config
            .resolve_model(
                &config.needle_exe,
                "needle",
                "Run `python scripts/fetch_assets.py whistle`.",
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let model = config
            .resolve_model(
                &config.whistle_model,
                "Whistle",
                "Run `python scripts/fetch_assets.py whistle`.",
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let scratch = config.base_dir.join("scratch");
        std::fs::create_dir_all(&scratch)?;
        let mut engine = Self {
            exe,
            model,
            language: "en".to_string(),
            keywords: None,
            scratch,
            word_timestamps: false,
            depth: config.whistle_decoder_depth,
            logger: logger.clone(),
        };
        engine.set_keywords(&config.keywords);
        engine.word_timestamps = config.word_timestamps;
        logger.info(format!(
            "Whistle sidecar ready: {} (model {})",
            engine.exe.display(),
            engine.model.display()
        ));
        Ok(engine)
    }

    fn set_keywords(&mut self, keywords: &[String]) {
        self.keywords = match keywords_blob(keywords) {
            Some(blob) => {
                let path = self.scratch.join("keywords.txt");
                match std::fs::write(&path, blob) {
                    Ok(()) => Some(path),
                    Err(e) => {
                        self.logger
                            .warn(format!("could not write keywords file: {e}"));
                        None
                    }
                }
            }
            None => None,
        };
    }
}

impl AsrEngine for SidecarEngine {
    fn name(&self) -> &'static str {
        "Whistle (sidecar)"
    }

    fn transcribe(&mut self, pcm: &[f32]) -> Result<AsrResult> {
        if pcm.is_empty() {
            return Ok(AsrResult::default());
        }
        let samples = &pcm[..pcm.len().min(MAX_SAMPLES)];
        let wav = self.scratch.join("segment.wav");
        write_wav_16k_mono(&wav, samples)?;

        let mut cmd = Command::new(&self.exe);
        cmd.arg("--model")
            .arg(&self.model)
            .arg("--audio")
            .arg(&wav)
            .arg("--audio-language")
            .arg(&self.language);
        if let Some(keywords) = &self.keywords {
            cmd.arg("--audio-keywords").arg(keywords);
        }
        if self.word_timestamps {
            cmd.arg("--audio-word-timestamps");
        }
        if let Some(depth) = self.depth {
            cmd.arg("--audio-depth").arg(depth.to_string());
        }
        cmd.creation_flags(CREATE_NO_WINDOW);

        let output = cmd
            .output()
            .with_context(|| format!("running {}", self.exe.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "needle.exe exited with {}: {}",
                output.status,
                stderr.trim()
            );
        }
        parse_result(&stdout)
    }
}

/// Write 16 kHz mono float samples as a 16-bit PCM WAV the CLI can read.
fn write_wav_16k_mono(path: &std::path::Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating {}", path.display()))?;
    for &s in samples {
        let scaled = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        writer.write_sample(scaled)?;
    }
    writer.finalize()?;
    Ok(())
}

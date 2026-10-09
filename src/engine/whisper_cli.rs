//! Indonesian transcription by running the prebuilt whisper.cpp CLI.
//!
//! This is the Indonesian counterpart to the Whistle sidecar, and it exists for the same reason:
//! compiling whisper.cpp in-process (`whisper-rs`) needs a native toolchain whose CMake build
//! fails here, while whisper.cpp publishes ready-made Windows binaries. The checkpoint is
//! identical either way, so accuracy is unchanged — only the process boundary differs.
//!
//! The CLI reads 16 kHz mono WAV, which is exactly what the capture pipeline already produces.

use std::io::{BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::{AsrEngine, AsrResult, MAX_SAMPLES, log_result};
use crate::capture;
use crate::config::{Config, Processing};
use crate::logging::Logger;

/// A console child of this GUI-subsystem process would otherwise flash a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Indonesian via the whisper.cpp command line runner.
pub struct WhisperCliEngine {
    exe: PathBuf,
    model: PathBuf,
    language: String,
    threads: i32,
    scratch: PathBuf,
    logger: Arc<Logger>,
    draft: bool,
    word_timestamps: bool,
    device: Option<crate::gpu::Gpu>,
    config: Config,
    notices: Vec<String>,
    gpu_wall: f64,
    gpu_audio: f64,
    qualification_written: bool,
    #[cfg(test)]
    timeout_override: Option<Duration>,
}

impl WhisperCliEngine {
    /// Locate the CLI and the checkpoint, and prepare the scratch folder.
    pub fn new(config: &Config, logger: &Arc<Logger>) -> Result<Self> {
        let discovery = if config.indonesian_processing == Processing::Cpu {
            crate::gpu::Discovery::default()
        } else {
            crate::gpu::discover()
        };
        let cuda = if config.whisper_exe == Path::new("whisper-cli.exe") {
            cuda_cli(config)
        } else {
            None
        };
        let device = discovery.device.filter(|_| cuda.is_some());
        let exe = if device.is_some() {
            cuda.unwrap()
        } else {
            resolve_cli(config, logger)?
        };
        let model = if config.indonesian_processing == Processing::Nvidia && device.is_none() {
            crate::models::starter(config)?
        } else {
            crate::models::selected(config, device.as_ref(), device.is_some())?
        };
        if let Some(entry) = crate::models::catalog().iter().find(|m| {
            model
                .file_name()
                .is_some_and(|f| f == std::ffi::OsStr::new(&m.filename))
        }) {
            crate::models::verify(
                &model,
                entry,
                &std::sync::atomic::AtomicBool::new(false),
                |_| {},
            )?;
        }
        let notices = if config.indonesian_processing == Processing::Nvidia && device.is_none() {
            vec![format!(
                "NVIDIA inference unavailable; using CPU. {}",
                if config.whisper_exe != Path::new("whisper-cli.exe") {
                    "The custom executable is configured for CPU. Reset whisper_exe to whisper-cli.exe to use the bundled CUDA runner."
                } else if cuda_cli(config).is_none() {
                    "CUDA runtime is not installed."
                } else {
                    &discovery.reason
                }
            )]
        } else {
            Vec::new()
        };
        let scratch = config
            .base_dir
            .join("scratch")
            .join(format!("final-{}", std::process::id()));
        std::fs::create_dir_all(&scratch)?;
        let size = std::fs::metadata(&model).map(|m| m.len()).unwrap_or(0);
        logger.info(format!(
            "Whisper CLI ready: {} (model {}, {:.0} MB)",
            exe.display(),
            model.display(),
            size as f64 / (1024.0 * 1024.0)
        ));
        Ok(Self {
            exe,
            model,
            // Indonesian, in the two-letter form whisper.cpp expects.
            language: "id".to_string(),
            threads: worker_threads(config.whisper_threads, false),
            scratch,
            logger: logger.clone(),
            draft: false,
            word_timestamps: config.word_timestamps || config.detect_speakers,
            device,
            config: config.clone(),
            notices,
            gpu_wall: 0.0,
            gpu_audio: 0.0,
            qualification_written: false,
            #[cfg(test)]
            timeout_override: None,
        })
    }

    pub fn new_draft(config: &Config, logger: &Arc<Logger>) -> Result<Self> {
        let mut config = config.clone();
        config.whisper_model = config
            .resolve_model(
                &config.whisper_draft_model,
                "Whisper live preview",
                "Run `python scripts/fetch_assets.py model --size base` to install live previews.",
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        config.indonesian_model = crate::config::ModelSelection::Custom;
        config.indonesian_processing = Processing::Cpu;
        let mut engine = Self::new(&config, logger)?;
        engine.scratch = config
            .base_dir
            .join("scratch")
            .join(format!("draft-{}", std::process::id()));
        std::fs::create_dir_all(&engine.scratch)?;
        engine.draft = true;
        engine.word_timestamps = false;
        engine.threads = worker_threads(config.whisper_threads, true);
        Ok(engine)
    }
}

fn worker_threads(configured: i32, draft: bool) -> i32 {
    if configured > 0 {
        return configured;
    }
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // Reserve CPU capacity for the GUI and preview. The final small model needs
    // more encoder threads than the base preview to keep up with six-second chunks.
    if draft {
        (logical / 8).clamp(1, 2) as i32
    } else {
        (logical / 2).clamp(1, 8) as i32
    }
}
impl AsrEngine for WhisperCliEngine {
    fn name(&self) -> &'static str {
        "Whisper (whisper.cpp)"
    }

    fn reusable_after_recording(&self) -> bool {
        false
    }
    fn status(&self) -> String {
        format!(
            "{} · {}",
            self.device
                .as_ref()
                .map(|g| g.name.as_str())
                .unwrap_or("CPU"),
            self.model.file_name().unwrap_or_default().to_string_lossy()
        )
    }
    fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }
    fn transcribe(&mut self, pcm: &[f32]) -> Result<AsrResult> {
        self.transcribe_with_updates(pcm, &mut |_| {})
    }

    fn transcribe_with_updates(
        &mut self,
        pcm: &[f32],
        update: &mut dyn FnMut(&str),
    ) -> Result<AsrResult> {
        if self.device.is_none() {
            return self.run(pcm, update);
        }
        let started = Instant::now();
        match self.run(pcm, &mut |_| {}) {
            Ok(result) => {
                self.gpu_wall += started.elapsed().as_secs_f64();
                self.gpu_audio += pcm.len().min(MAX_SAMPLES) as f64 / capture::TARGET_RATE as f64;
                if self.gpu_audio >= 30.0
                    && !self.qualification_written
                    && self.model.file_name().is_some_and(|f| {
                        f == std::ffi::OsStr::new(
                            &crate::models::get(crate::models::TURBO).unwrap().filename,
                        )
                    })
                {
                    if let Some(gpu) = &self.device {
                        let passed = self.gpu_wall < self.gpu_audio;
                        if let Err(e) = crate::models::save_qualification(
                            &self.config,
                            gpu,
                            passed,
                            self.gpu_wall,
                            self.gpu_audio,
                        ) {
                            self.logger
                                .warn(format!("Could not save turbo benchmark: {e}"));
                        }
                        self.qualification_written = true;
                    }
                }
                if !result.text.is_empty() {
                    update(&result.text);
                }
                Ok(result)
            }
            Err(error) => {
                if let Some(gpu) = &self.device {
                    let _ = crate::models::save_qualification(
                        &self.config,
                        gpu,
                        false,
                        self.gpu_wall,
                        self.gpu_audio,
                    );
                }
                let mut cpu = self.config.clone();
                cpu.indonesian_processing = Processing::Cpu;
                cpu.indonesian_model = crate::config::ModelSelection::SmallId;
                let notice = format!(
                    "NVIDIA inference failed: {error:#}. Switched to CPU with Indonesian Whisper small for this recording."
                );
                self.logger.warn(&notice);
                let mut replacement = Self::new(&cpu, &self.logger)?;
                replacement.config = self.config.clone();
                replacement.notices = std::mem::take(&mut self.notices);
                replacement.notices.push(notice);
                *self = replacement;
                self.run(pcm, update)
            }
        }
    }
}
impl WhisperCliEngine {
    fn run(&mut self, pcm: &[f32], update: &mut dyn FnMut(&str)) -> Result<AsrResult> {
        if pcm.is_empty() {
            return Ok(AsrResult::default());
        }
        let samples = &pcm[..pcm.len().min(MAX_SAMPLES)];
        let wav = self.scratch.join("segment.wav");
        // The CLI does not resample, so hand it 16 kHz mono exactly as captured.
        capture::write_wav(&wav, samples)?;

        let json_path = self.scratch.join("timed.json");
        if self.word_timestamps && json_path.exists() {
            std::fs::remove_file(&json_path)?;
        }
        let mut cmd = Command::new(&self.exe);
        cmd.arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(&wav)
            .arg("-l")
            .arg(&self.language);
        if let Some(device) = &self.device {
            cmd.arg("-dev").arg(device.index.to_string());
        } else {
            cmd.arg("-ng");
        }
        if self.word_timestamps {
            // -nt disables timestamp decoding, even with full JSON output.
            cmd.args(["-ojf", "-ml", "1", "-sow"])
                .arg("-of")
                .arg(self.scratch.join("timed"));
        } else {
            cmd.arg("-nt");
        }
        if self.threads > 0 {
            cmd.arg("-t").arg(self.threads.to_string());
        }
        let short_context = self
            .model
            .file_name()
            .is_some_and(|f| f == std::ffi::OsStr::new("ggml-small-id-q8_0.bin"));
        if self.draft || short_context {
            cmd.args(["-bs", "1", "-bo", "1"]);
        }
        if short_context {
            // This fine-tune was trained with short encoder context (50 positions/second).
            let positions = ((samples.len() as u64 * 50).div_ceil(capture::TARGET_RATE as u64))
                .div_ceil(64)
                * 64;
            cmd.arg("-ac").arg(positions.clamp(64, 1500).to_string());
        }
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("running {}", self.exe.display()))?;
        let stdout = child.stdout.take().context("opening Whisper stdout")?;
        let stderr = child.stderr.take().context("opening Whisper stderr")?;
        let (lines_tx, lines_rx) = crossbeam_channel::unbounded();
        let out_reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut bytes = [0u8; 4096];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        if lines_tx.send(Ok(bytes[..count].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = lines_tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        let err_reader = std::thread::spawn(move || {
            let mut reader = stderr;
            let mut buffer = [0u8; 4096];
            let mut bytes = Vec::new();
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.len() > 65536 {
                    bytes.drain(..bytes.len() - 65536);
                }
            }
            String::from_utf8_lossy(&bytes).into_owned()
        });
        let started = Instant::now();
        let mut text = String::new();
        let mut output = Vec::new();
        let mut status = None;
        let mut stdout_done = false;
        let mut failure = None;
        while status.is_none() || !stdout_done {
            match lines_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(bytes)) => {
                    if let Err(e) = publish_output(&mut output, &bytes, &mut text, update) {
                        failure = Some(e.to_string());
                        break;
                    }
                }
                Ok(Err(e)) => {
                    failure = Some(format!("reading Whisper output: {e}"));
                    break;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => stdout_done = true,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
            match child.try_wait() {
                Ok(s) => {
                    status = s;
                }
                Err(e) => {
                    failure = Some(format!("waiting for Whisper: {e}"));
                    break;
                }
            }
            let timeout = Duration::from_secs(if self.draft || self.device.is_some() {
                20
            } else {
                60
            });
            #[cfg(test)]
            let timeout = self.timeout_override.unwrap_or(timeout);
            if started.elapsed() > timeout {
                failure = Some("Whisper timed out".into());
                break;
            }
            if stdout_done && status.is_none() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if failure.is_some() {
            let _ = child.kill();
        }
        let exit = child.wait().context("reaping Whisper process");
        let _ = out_reader.join();
        let stderr = err_reader.join().unwrap_or_default();
        if let Some(failure) = failure {
            anyhow::bail!("{failure}: {}", tail(&stderr, 400));
        }
        let status = exit?;
        if self.device.is_some() && status.success() {
            anyhow::ensure!(
                cuda_initialized(&stderr),
                "CUDA backend was not used: {}",
                tail(&stderr, 400)
            );
        }
        if !status.success() {
            anyhow::bail!("whisper.cpp exited with {status}: {}", tail(&stderr, 400));
        }
        if text.is_empty() {
            // Some builds log to stdout and print the transcription elsewhere, or a
            // future build may move it: fall back to stderr with engine log lines
            // filtered out, so a moved transcript still appears instead of silence.
            let fallback = clean_transcript_filtered(&stderr);
            if !fallback.is_empty() {
                self.logger.warn(format!(
                    "whisper.cpp wrote the transcript to stderr ({} stdout bytes); using it",
                    0
                ));
                text = fallback;
                update(&text);
            }
        }
        self.logger.info(format!(
            "Whisper {} inference {:.0} ms for {:.2}s audio",
            if self.draft { "draft" } else { "final" },
            started.elapsed().as_secs_f64() * 1000.0,
            samples.len() as f64 / capture::TARGET_RATE as f64
        ));
        let secs = samples.len() as f32 / capture::TARGET_RATE as f32;
        log_result(
            &self.logger,
            self.name(),
            secs,
            &AsrResult {
                text: text.clone(),
                language: Some(self.language.clone()),
                ..Default::default()
            },
        );
        let mut result = AsrResult {
            text,
            language: Some(self.language.clone()),
            ..Default::default()
        };
        if self.word_timestamps {
            match std::fs::read_to_string(&json_path)
                .map_err(anyhow::Error::from)
                .and_then(|s| parse_timed_json(&s))
            {
                Ok(timed) => {
                    result.text = timed.text;
                    result.words = timed.words;
                }
                Err(e) => self.logger.warn(format!(
                    "Whisper word timing unavailable: {e:#}; retaining transcript"
                )),
            }
        }
        Ok(result)
    }
}

/// whisper.cpp full JSON uses millisecond offsets for timestamped tokens.
fn parse_timed_json(raw: &str) -> Result<AsrResult> {
    let json: serde_json::Value = serde_json::from_str(raw)?;
    let segments = json
        .get("transcription")
        .and_then(|v| v.as_array())
        .context("Whisper JSON has no transcription")?;
    let mut result = AsrResult::default();
    for segment in segments {
        result
            .text
            .push_str(segment.get("text").and_then(|v| v.as_str()).unwrap_or(""));
        for token in segment
            .get("tokens")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let word = token.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if word.trim().is_empty() || word.starts_with("[_") || word.starts_with("<|") {
                continue;
            }
            let offsets = token.get("offsets");
            if let (Some(start), Some(end)) = (
                offsets.and_then(|v| v.get("from")).and_then(|v| v.as_f64()),
                offsets.and_then(|v| v.get("to")).and_then(|v| v.as_f64()),
            ) {
                if start >= 0.0 && end > start {
                    result.words.push(super::Word {
                        word: word.into(),
                        start: start as f32 / 1000.0,
                        end: end as f32 / 1000.0,
                        probability: token.get("p").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    });
                }
            }
        }
    }
    result.text = result.text.trim().into();
    Ok(result)
}
fn publish_output(
    output: &mut Vec<u8>,
    bytes: &[u8],
    text: &mut String,
    update: &mut dyn FnMut(&str),
) -> Result<()> {
    anyhow::ensure!(
        output.len() + bytes.len() <= 1024 * 1024,
        "Whisper stdout exceeded its limit"
    );
    output.extend_from_slice(bytes);
    // Whisper flushes text without a trailing newline. Publish those bytes now,
    // retaining incomplete UTF-8 until the next read instead of waiting for EOF.
    let complete = match std::str::from_utf8(output) {
        Ok(_) => output.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => output.len(),
    };
    let candidate = clean_transcript_filtered(&String::from_utf8_lossy(&output[..complete]));
    if !candidate.is_empty() && candidate != *text {
        *text = candidate;
        update(text);
    }
    Ok(())
}

/// Prefer the configured name, then the historical `main.exe`, then whatever the folder holds.
/// CPU and CUDA binaries must never share a DLL directory.
pub fn cuda_cli(config: &Config) -> Option<PathBuf> {
    config
        .resolve_model(
            Path::new("vendor/whisper/cuda/whisper-cli.exe"),
            "CUDA runtime",
            "Repair the installation",
        )
        .ok()
}
fn cuda_initialized(stderr: &str) -> bool {
    stderr.contains("using CUDA")
}
fn resolve_cli(config: &Config, logger: &Arc<Logger>) -> Result<PathBuf> {
    if config.whisper_exe == Path::new("whisper-cli.exe")
        && let Ok(path) = config.resolve_model(
            Path::new("vendor/whisper/cpu/whisper-cli.exe"),
            "Whisper CPU",
            "Repair the installation",
        )
    {
        return Ok(path);
    }
    let candidates = ["whisper-cli.exe", "main.exe"];
    let spec = config.whisper_exe.to_string_lossy().to_string();
    let mut order: Vec<String> = vec![spec.clone()];
    for candidate in candidates {
        if !order.iter().any(|c| c.eq_ignore_ascii_case(candidate)) {
            order.push(candidate.to_string());
        }
    }

    let mut searched = Vec::new();
    for name in &order {
        let hint = "The Indonesian component is not installed: re-run the setup, \
                    or run `python scripts/fetch_assets.py whisper`.";
        match config.resolve_model(Path::new(name), "whisper.cpp CLI", hint) {
            Ok(path) => {
                if name != &spec {
                    logger.info(format!("using {name} (configured {spec:?} was not found)"));
                }
                return Ok(path);
            }
            Err(err) => searched.extend(err.searched),
        }
    }
    anyhow::bail!(
        "the Indonesian component is not installed: the whisper.cpp CLI was not found (looked for \
         {}). Re-run the setup, or run \
         `python scripts/fetch_assets.py whisper`.",
        searched
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Turn whisper.cpp output into a single line of text.
///
/// With `-nt` each segment is printed on its own line; any timestamp prefix the build still emits
/// is stripped, and blank lines are dropped.
pub fn clean_transcript(stdout: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in stdout.lines() {
        let line = strip_timestamp(line.trim());
        if !line.is_empty() {
            parts.push(line);
        }
    }
    parts.join(" ").trim().to_string()
}

/// Remove a leading `[...]` timestamp block, e.g. `[00:00:00.000 --> 00:00:02.000]`.
fn strip_timestamp(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let inside = &rest[..end];
        if inside.contains(':') {
            return rest[end + 1..].trim_start();
        }
    }
    line
}

/// Turn whisper.cpp output into a single line of text, dropping engine log lines.
///
/// `clean_transcript` covers stdout, which is normally just the transcription.
/// This variant is the stderr fallback: the CLI logs model loading, timings and
/// progress there, and those lines must never end up in the transcript.
pub fn clean_transcript_filtered(output: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_log_line(trimmed) {
            continue;
        }
        let line = strip_timestamp(trimmed);
        if !line.is_empty() {
            parts.push(line);
        }
    }
    parts.join(" ").trim().to_string()
}

/// True for a whisper.cpp diagnostic line rather than transcribed speech.
///
/// Matches the `key: value` / `key = value` diagnostics this build emits
/// (`whisper_model_load: ...`, `system_info: ...`, `main: processing ...`,
/// `read_audio_data: ...`, `load_backend: ...`, `whisper_print_timings: ...`).
/// Transcribed speech almost never starts with one of these prefixes, and the
/// filter only runs on the stderr fallback path, so stdout is unaffected.
fn is_log_line(line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "whisper_",
        "system_info",
        "main:",
        "read_audio_data",
        "load_backend",
        "ggml",
        "llama",
        "sampling:",
        "encode time",
        "decode time",
        "mel time",
        "sample time",
        "total time",
        "prompt time",
        "batchd time",
        "fallbacks",
        "kv ",
        "compute buffer",
        "model size",
    ];
    PREFIXES.iter().any(|p| line.starts_with(p))
}

/// Keep the last part of a long message, for error reporting.
fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut start = trimmed.len() - max;
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &trimmed[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publishes_flushed_text_without_newlines_and_handles_split_utf8() {
        let (mut bytes, mut text, mut updates) = (Vec::new(), String::new(), Vec::new());
        let mut publish = |s: &str| updates.push(s.to_string());
        publish_output(
            &mut bytes,
            b"whisper_model_load: diagnostic\nHalo caf\xc3",
            &mut text,
            &mut publish,
        )
        .unwrap();
        publish_output(&mut bytes, b"\xa9", &mut text, &mut publish).unwrap();
        assert_eq!(updates, ["Halo caf", "Halo café"]);
    }
    #[test]
    fn missing_preview_model_does_not_change_the_final_model() {
        let config = Config {
            whisper_draft_model: PathBuf::from("missing-preview-checkpoint.bin"),
            ..Default::default()
        };
        let error = WhisperCliEngine::new_draft(&config, &Arc::new(Logger::disabled()))
            .err()
            .unwrap();
        assert!(error.to_string().contains("--size base"));
        assert_eq!(config.whisper_model, PathBuf::from("ggml-small-q5_1.bin"));
    }

    #[test]
    fn joins_plain_segment_lines() {
        let out = "Halo, apa kabar?\nSaya baik.\n\n";
        assert_eq!(clean_transcript(out), "Halo, apa kabar? Saya baik.");
    }

    #[test]
    fn strips_timestamp_prefixes_when_present() {
        let out = "[00:00:00.000 --> 00:00:02.000]   Halo dunia\n[00:00:02.000 --> 00:00:04.000]   Apa kabar\n";
        assert_eq!(clean_transcript(out), "Halo dunia Apa kabar");
    }

    #[test]
    fn empty_output_is_empty() {
        assert_eq!(clean_transcript(""), "");
        assert_eq!(clean_transcript("\n\n  \n"), "");
    }

    #[test]
    fn brackets_without_times_are_kept() {
        // e.g. a bracketed sound annotation must not be mistaken for a timestamp
        assert_eq!(clean_transcript("[Musik] halo"), "[Musik] halo");
    }

    #[test]
    fn filtered_output_drops_engine_logs_but_keeps_speech() {
        let stderr = "whisper_model_load: n_vocab       = 51865\n\
                      system_info: n_threads = 4 / 16\n\
                      main: processing 'segment.wav' (48000 samples, 3.0 sec), lang = id ...\n\
                      Halo dunia\n\
                      whisper_print_timings:     load time =   265.43 ms\n";
        assert_eq!(clean_transcript_filtered(stderr), "Halo dunia");
    }

    #[test]
    fn filtered_output_of_pure_logs_is_empty() {
        let stderr = "whisper_model_load: loading model\nsystem_info: n_threads = 4\n";
        assert_eq!(clean_transcript_filtered(stderr), "");
    }

    #[test]
    fn speech_lines_are_never_mistaken_for_logs() {
        assert_eq!(
            clean_transcript_filtered("Halo dunia, apa kabar?"),
            "Halo dunia, apa kabar?"
        );
        assert_eq!(clean_transcript_filtered("[Musik] halo"), "[Musik] halo");
    }

    #[test]
    fn unicode_error_tail_is_safe() {
        let text = "é世界".repeat(200);
        assert!(tail(&text, 400).starts_with('…'));
        assert!(tail(&text, 400).len() <= 403);
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[test]
    fn parses_full_json_and_filters_special_tokens() {
        let raw = r#"{"transcription":[{"text":" Halo Budi.","tokens":[{"text":"[_BEG_]","offsets":{"from":0,"to":10}},{"text":" Halo","p":0.9,"offsets":{"from":20,"to":500}},{"text":" Budi.","p":0.8,"offsets":{"from":500,"to":1000}}]}]}"#;
        let r = parse_timed_json(raw).unwrap();
        assert_eq!(r.text, "Halo Budi.");
        assert_eq!(r.words.len(), 2);
        assert_eq!(r.words[0].start, 0.02);
        assert_eq!(r.words[1].end, 1.0);
    }
}

#[cfg(test)]
mod gpu_integration_tests {
    use super::*;
    /// Uses fetched runtime/model fixtures, so it is run explicitly during hardware verification.
    #[test]
    #[ignore = "requires downloaded starter model and Windows fake-runner fixtures"]
    fn gpu_failures_retry_once_and_remain_on_cpu() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let logger = Arc::new(Logger::disabled());
        let mut reader = hound::WavReader::open(root.join(".scratch/youtube-id-six.wav")).unwrap();
        let pcm: Vec<f32> = reader
            .samples::<i16>()
            .map(|v| v.unwrap() as f32 / 32768.)
            .collect();
        for failure in ["missing", "allocation", "crash", "timeout"] {
            let config = Config {
                indonesian_processing: Processing::Cpu,
                indonesian_model: crate::config::ModelSelection::SmallId,
                base_dir: root.join(".scratch/fallback-test").join(failure),
                ..Config::default()
            };
            let mut engine = WhisperCliEngine::new(&config, &logger).unwrap();
            engine.device = Some(crate::gpu::Gpu {
                index: 0,
                name: "Simulated GPU".into(),
                total: 6 * 1073741824,
                free: 5 * 1073741824,
            });
            engine.exe = root
                .join(".scratch/fake-runners")
                .join(format!("{failure}.exe"));
            engine.timeout_override = Some(Duration::from_millis(150));
            let mut published = Vec::new();
            let result = engine
                .transcribe_with_updates(&pcm, &mut |v| published.push(v.to_string()))
                .unwrap();
            assert!(!result.text.is_empty());
            assert!(engine.device.is_none());
            assert!(published.iter().all(|v| !v.contains("DISCARD THIS")));
            assert_eq!(engine.take_notices().len(), 1);
            assert!(!engine.reusable_after_recording());
            engine.transcribe(&pcm).unwrap();
            assert!(engine.take_notices().is_empty());
        }
    }
}

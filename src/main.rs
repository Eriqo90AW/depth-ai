//! depth — a tray app that transcribes desktop audio on-device.
//!
//! Run with no arguments to start listening from the tray. The diagnostic modes exist so each
//! moving part can be checked on its own:
//!
//! ```text
//! depth --check                  # configuration, model and engine report
//! depth --record 10 scratch.wav  # 10 s of desktop audio to a WAV
//! depth --once scratch.wav       # transcribe a WAV and print the result
//! depth --headless               # run the pipeline without a tray icon
//! ```
//!
//! Release builds are GUI-subsystem binaries, so launching the app from a shortcut opens no
//! console window — and closing one can no longer kill it. A diagnostic run reattaches to the
//! terminal it was started from, which is why the modes above still print.

// A tray app must not own a console: Explorer, the Startup folder and the shortcuts all launch
// it, and a console window there is both ugly and load-bearing — closing it stops the app. Debug
// builds keep the console, so `cargo run -- --check` behaves like any other command line tool.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleWindow, SetConsoleOutputCP,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::PCWSTR;

use depth::config::{Config, Language};
use depth::logging::Logger;
use depth::{capture, engine, pipeline};

#[cfg(feature = "tray")]
use depth::gui;

const USAGE: &str = "\
depth — on-device transcription of desktop audio

USAGE:
    depth [OPTIONS] [MODE]

MODES:
    (none)                  run the tray app and listen to desktop audio
    --headless              run the pipeline without a tray icon
    --check                 report configuration, models and compiled engines, then exit
    --record <SECS> <WAV>   capture desktop audio for SECS seconds into a WAV file
    --once <WAV>            transcribe an existing 16 kHz WAV and print the result

OPTIONS:
    --lang <en|id>          English (Whistle) or Indonesian (Whisper)
    --background            start hidden, for Windows startup
    --preview-state <STATE>  sample UI: empty, recording, long, settings, error
    --home <DIR>            use DIR for config, log and transcripts
    -h, --help              show this message
";

fn main() {
    // A GUI-subsystem release build has no console of its own; the diagnostic modes are almost
    // always started from a terminal, so borrow that one. Failing means we already have a console
    // (debug builds) or there is none to borrow, and both are fine.
    if std::env::args_os().len() > 1 {
        // SAFETY: a plain console call; the result is deliberately ignored.
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            // UTF-8, so the report's em dashes survive the console code page.
            let _ = SetConsoleOutputCP(65001);
        }
    }
    if let Err(err) = real_main() {
        let message = format!("{err:#}");
        let _ = writeln!(std::io::stderr(), "error: {message}");
        // With no console there is nothing to read, so a fatal launch problem has to say so
        // out loud rather than leaving an app that silently never appears.
        // SAFETY: a plain query; a null handle means no console is attached.
        let has_console = unsafe { !GetConsoleWindow().is_invalid() };
        if !has_console {
            message_box("depth", &message);
        }
        std::process::exit(1);
    }
}

/// Print one line, tolerating a process that has no console to print to.
fn out(line: impl AsRef<str>) {
    let _ = writeln!(std::io::stdout(), "{}", line.as_ref());
}

/// A named mutex held while the app runs, so the uninstaller can detect a running instance.
///
/// The installers open `DepthRunning` and, when it exists, ask the user to close the app
/// instead of deleting files from under it. A missing mutex (e.g. creation failed) only means
/// the running check is skipped; it never stops the app itself.
fn hold_running_mutex(logger: &Logger) -> Option<windows::Win32::Foundation::HANDLE> {
    use std::os::windows::ffi::OsStrExt;
    let name: Vec<u16> = std::ffi::OsStr::new("DepthRunning")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: plain mutex creation with default security; the handle is closed by process exit.
    match unsafe {
        windows::Win32::System::Threading::CreateMutexW(None, false.into(), PCWSTR(name.as_ptr()))
    } {
        Ok(handle) => Some(handle),
        Err(err) => {
            logger.warn(format!("could not create the running mutex: {err:#}"));
            None
        }
    }
}

/// A modal error report, for the runs that have no console to write to.
fn message_box(caption: &str, message: &str) {
    let caption: Vec<u16> = caption.encode_utf16().chain(std::iter::once(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Tray,
    Headless,
    Check,
    Record { seconds: u64, out: PathBuf },
    Once { wav: PathBuf },
    Help,
}

#[derive(Debug)]
struct Options {
    mode: Mode,
    language: Option<Language>,
    home: Option<PathBuf>,
    background: bool,
    preview: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Options> {
    let mut mode = Mode::Tray;
    let mut language = None;
    let mut home = None;
    let mut index = 0;
    let mut background = false;
    let mut preview = None;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => mode = Mode::Help,
            "--headless" => mode = Mode::Headless,
            "--tray" => mode = Mode::Tray,
            "--background" => background = true,
            "--preview" => preview = Some("long".into()),
            "--preview-state" => {
                index += 1;
                preview = Some(
                    args.get(index)
                        .context("--preview-state needs empty, recording, long, settings or error")?
                        .clone(),
                );
            }
            "--check" => mode = Mode::Check,
            "--lang" => {
                index += 1;
                let value = args.get(index).context("--lang needs en or id")?;
                language = Some(match value.to_ascii_lowercase().as_str() {
                    "en" | "english" => Language::En,
                    "id" | "indonesian" => Language::Id,
                    other => anyhow::bail!("unknown language {other:?}; use en or id"),
                });
            }
            "--home" => {
                index += 1;
                home = Some(PathBuf::from(
                    args.get(index).context("--home needs a directory")?,
                ));
            }
            "--record" => {
                let seconds: u64 = args
                    .get(index + 1)
                    .context("--record needs SECS and a WAV path")?
                    .parse()
                    .context("--record SECS must be a whole number of seconds")?;
                let out = PathBuf::from(args.get(index + 2).context("--record needs a WAV path")?);
                mode = Mode::Record { seconds, out };
                index += 2;
            }
            "--once" => {
                let wav = PathBuf::from(args.get(index + 1).context("--once needs a WAV path")?);
                mode = Mode::Once { wav };
                index += 1;
            }
            other => anyhow::bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
        index += 1;
    }

    Ok(Options {
        mode,
        language,
        home,
        background,
        preview,
    })
}

fn real_main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = parse_args(&args)?;

    if options.mode == Mode::Help {
        out(USAGE);
        return Ok(());
    }

    let (mut config, config_path) = Config::load(options.home.clone())?;
    if let Some(language) = options.language {
        config.language = language;
    }

    match options.mode {
        Mode::Help => Ok(()),
        Mode::Check => check(&config, &config_path),
        Mode::Once { wav } => once(&config, &wav),
        Mode::Record { seconds, out } => record(&config, seconds, &out),
        Mode::Tray => run(
            config,
            &config_path,
            true,
            options.background,
            options.preview,
        ),
        Mode::Headless => run(config, &config_path, false, false, None),
    }
}

/// Report configuration, models and compiled engines.
fn check(config: &Config, config_path: &Path) -> Result<()> {
    out("depth configuration check");
    out(format!("  config file     {}", config_path.display()));
    out(format!("  log file        {}", config.log_path().display()));
    out(format!(
        "  transcripts     {} ({})",
        config.transcripts_dir().display(),
        if config.save_transcript {
            "autosave on"
        } else {
            "autosave off — session stays in memory"
        }
    ));
    out(format!(
        "  language        {} ({})",
        config.language.label(),
        config.language.code()
    ));
    out(format!(
        "  desktop source  {}",
        config
            .audio_output_device
            .as_deref()
            .unwrap_or("Windows default (mute-safe on supported Windows)")
    ));
    match config.resolve_model(
        &config.whisper_draft_model,
        "Whisper live preview",
        "python scripts/fetch_assets.py model --size base",
    ) {
        Ok(path) => out(format!("  live previews   {}", path.display())),
        Err(_) => out("  live previews   unavailable; final transcription remains enabled"),
    }
    out(format!(
        "  listening       {}",
        if config.start_listening {
            "starts immediately"
        } else {
            "idle until the hotkey is pressed"
        }
    ));
    out(format!(
        "  hotkey          {} (fallback {})",
        config.hotkey, config.hotkey_fallback
    ));
    out(format!(
        "  indicator       {} (margin {} px at 96 DPI, opacity {})",
        config.indicator_position.label(),
        config.indicator_margin,
        config.indicator_opacity
    ));
    out(format!(
        "  viewer          theme {}, engine unloads after {}s idle{}",
        config.viewer_theme.key(),
        config.engine_idle_unload_secs,
        if config.engine_idle_unload_secs == 0 {
            " (disabled)"
        } else {
            ""
        }
    ));
    out(format!(
        "  engines         whistle={} sidecar={} whisper={} whisper-cli={} tray={}",
        cfg!(feature = "whistle"),
        cfg!(feature = "whistle-sidecar"),
        cfg!(feature = "whisper"),
        cfg!(feature = "whisper-sidecar"),
        cfg!(feature = "tray")
    ));
    out(format!(
        "  segmentation    gate {:.0} dBFS, open {} ms, close {} ms, max {:.0} s",
        config.vad_threshold_db,
        config.speech_start_ms,
        config.silence_close_ms,
        config.max_segment_secs
    ));

    let mut missing = false;
    for language in Language::all() {
        let (spec, what, hint) = match language {
            Language::En => (
                &config.whistle_model,
                "Whistle",
                "python scripts/fetch_assets.py whistle",
            ),
            Language::Id => (
                &config.whisper_model,
                "Whisper",
                "python scripts/fetch_assets.py whisper --size small",
            ),
        };
        match config.resolve_model(spec, what, hint) {
            Ok(path) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                out(format!(
                    "  {:<16} {} ({:.1} MB)",
                    format!("{} model", language.label()),
                    path.display(),
                    size as f64 / (1024.0 * 1024.0)
                ));
            }
            Err(err) => {
                if language == config.language {
                    missing = true;
                    out(format!(
                        "  {:<16} MISSING — {}",
                        format!("{} model", language.label()),
                        err
                    ));
                } else {
                    // A component the setup did not install is not a fault: English-only installs
                    // are the default, and the checker must not make them look broken.
                    out(format!(
                        "  {:<16} not installed (optional) — the setup's \"Indonesian transcription\" component was not selected",
                        format!("{} model", language.label())
                    ));
                }
            }
        }
    }

    if missing {
        anyhow::bail!("the model for the configured language is missing");
    }
    out("\nready: depth will listen to the default output device");
    Ok(())
}

/// Transcribe a WAV file once and print the engine's reply.
fn once(config: &Config, wav: &Path) -> Result<()> {
    let logger = Arc::new(Logger::disabled());
    let samples = capture::read_wav(wav)?;
    let seconds = samples.len() as f32 / capture::TARGET_RATE as f32;
    out(format!(
        "{} — {:.1} s of audio at {} Hz",
        wav.display(),
        seconds,
        capture::TARGET_RATE
    ));

    let mut active = engine::load(config.language, config, &logger)?;
    let started = Instant::now();
    let result = active.transcribe(&samples)?;
    let elapsed = started.elapsed();

    out(format!("engine      {}", active.name()));
    out(format!("wall        {:.2} s", elapsed.as_secs_f32()));
    out(format!(
        "language    {}",
        result.language.clone().unwrap_or_default()
    ));
    out(format!("ttft        {:.0} ms", result.ttft_ms));
    out(format!("decode      {:.0} tok/s", result.decode_tps));
    out(format!("text        {}", result.text));
    if let Some(first) = result.words.first() {
        out(format!(
            "words       {} (first: {:?} @ {:.2}s)",
            result.words.len(),
            first.word,
            first.start
        ));
    }
    Ok(())
}

/// Capture desktop audio for a fixed time into a WAV file.
fn record(config: &Config, seconds: u64, out_path: &Path) -> Result<()> {
    let logger = Arc::new(Logger::new(config.log_path()));
    logger.info(format!(
        "recording {seconds} s of desktop audio to {}",
        out_path.display()
    ));
    out(format!(
        "recording {seconds} s of desktop audio to {} — play something now",
        out_path.display()
    ));

    let buffer = Arc::new(std::sync::Mutex::new(Vec::<f32>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let sink = {
        let buffer = buffer.clone();
        Box::new(move |audio: &[f32]| {
            let mut guard = match buffer.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.extend_from_slice(audio);
        })
    };
    let handle = capture::spawn_loopback(sink, stop.clone(), logger.clone());
    std::thread::sleep(Duration::from_secs(seconds));
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();

    let samples = {
        let guard = match buffer.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clone()
    };
    if samples.is_empty() {
        anyhow::bail!(
            "captured nothing; see the log at {}",
            config.log_path().display()
        );
    }
    capture::write_wav(out_path, &samples)?;
    let captured = samples.len() as f32 / capture::TARGET_RATE as f32;
    out(format!("wrote {captured:.1} s to {}", out_path.display()));
    Ok(())
}

fn run(
    config: Config,
    config_path: &Path,
    use_tray: bool,
    background: bool,
    preview: Option<String>,
) -> Result<()> {
    let logger = Arc::new(Logger::new(config.log_path()));
    logger.info(format!(
        "depth starting: language {}, config {}",
        config.language.label(),
        config_path.display()
    ));
    // Held for the whole session: the setup and uninstaller open this mutex to tell whether
    // the app is still running, and ask the user to close it before uninstalling.
    let _running = hold_running_mutex(&logger);

    out(format!(
        "listening to desktop audio; transcripts go to {}",
        config.transcripts_dir().display()
    ));

    #[cfg(feature = "tray")]
    if use_tray {
        return gui::run(config, logger, background, preview);
    }
    #[cfg(not(feature = "tray"))]
    if use_tray {
        logger.warn("this build has no tray support; running headless");
        out("this build has no tray support; running headless");
    }
    #[cfg(not(feature = "tray"))]
    let _ = (use_tray, background, preview);

    let mut config = config;
    config.start_listening = true;
    let handle = pipeline::start(config.clone(), logger.clone())?;
    unsafe {
        let _ = windows::Win32::System::Console::SetConsoleCtrlHandler(Some(headless_signal), true);
    }
    out("press Ctrl+C to stop");
    let mut last = handle.status();
    while !HEADLESS_STOP.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let status = handle.status();
        if status != last {
            logger.info(format!("status: {}", status.label()));
            out(format!("status: {}", status.label()));
            last = status;
        }
    }
    handle.shutdown();
    Ok(())
}

static HEADLESS_STOP: AtomicBool = AtomicBool::new(false);
unsafe extern "system" fn headless_signal(event: u32) -> windows::core::BOOL {
    if event <= 2 {
        HEADLESS_STOP.store(true, Ordering::Release);
        true.into()
    } else {
        false.into()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_mutex_can_be_held() {
        // The uninstaller detects a running app by opening this mutex; creating it twice must
        // succeed (the second handle just finds the existing one).
        let logger = depth::logging::Logger::disabled();
        let first = hold_running_mutex(&logger);
        assert!(first.is_some());
        let second = hold_running_mutex(&logger);
        assert!(second.is_some());
    }
}

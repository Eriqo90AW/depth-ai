//! User-editable configuration.
//!
//! Everything the app produces lives in one folder so it is easy to find:
//!
//! ```text
//! %USERPROFILE%\Documents\Depth\
//!   config.toml
//!   depth.log
//!   transcripts\transcript-2025-06-01.md
//! ```
//!
//! Set `DEPTH_HOME` to relocate that whole folder (used by tests and by portable
//! installs). `TRANSCRIBE_AI_HOME` is still honoured for portable installs made before the
//! rename to Depth.

use std::path::{Path, PathBuf};

use directories::UserDirs;
use serde::{Deserialize, Serialize};

/// Which engine and language a capture session uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// English, transcribed by the Whistle model.
    En,
    /// Indonesian, transcribed by the Whisper model.
    Id,
}

impl Language {
    /// The language code both engines accept.
    pub fn code(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Id => "id",
        }
    }

    /// A human-readable name for the tray menu and the Markdown header.
    pub fn label(self) -> &'static str {
        match self {
            Language::En => "English",
            Language::Id => "Indonesian",
        }
    }

    /// Both languages, in menu order.
    pub fn all() -> [Language; 2] {
        [Language::En, Language::Id]
    }

    /// The other language, for menu toggling.
    pub fn other(self) -> Language {
        match self {
            Language::En => Language::Id,
            Language::Id => Language::En,
        }
    }
}

/// Which corner of the usable screen area the on-screen indicator is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum IndicatorPosition {
    #[default]
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

impl IndicatorPosition {
    /// The spelling this variant uses in `config.toml`.
    pub fn key(self) -> &'static str {
        match self {
            IndicatorPosition::BottomRight => "bottom-right",
            IndicatorPosition::BottomLeft => "bottom-left",
            IndicatorPosition::TopRight => "top-right",
            IndicatorPosition::TopLeft => "top-left",
        }
    }

    /// Human-readable, for `--check`.
    pub fn label(self) -> &'static str {
        match self {
            IndicatorPosition::BottomRight => "bottom right",
            IndicatorPosition::BottomLeft => "bottom left",
            IndicatorPosition::TopRight => "top right",
            IndicatorPosition::TopLeft => "top left",
        }
    }
}

/// Which theme the built-in Markdown viewer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ViewerTheme {
    #[default]
    System,
    Dark,
    Light,
}

impl ViewerTheme {
    /// The spelling this variant uses in `config.toml`.
    pub fn key(self) -> &'static str {
        match self {
            ViewerTheme::Dark => "dark",
            ViewerTheme::Light => "light",
            ViewerTheme::System => "system",
        }
    }

    /// Parse a loose theme name (menu ids, CLI flags).
    pub fn parse(text: &str) -> Option<ViewerTheme> {
        match text.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(ViewerTheme::Dark),
            "light" => Some(ViewerTheme::Light),
            "system" => Some(ViewerTheme::System),
            _ => None,
        }
    }
}

/// Tunables for capture, segmentation and transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Language (and therefore engine) to start with.
    pub language: Language,
    /// Whistle model file; relative paths are searched next to the executable.
    pub whistle_model: PathBuf,
    /// The Cactus command line runner, used by the `whistle-sidecar` build.
    pub needle_exe: PathBuf,
    /// Whisper checkpoint for Indonesian; relative paths are searched next to the executable.
    pub whisper_model: PathBuf,
    /// Smaller multilingual checkpoint used for transient Indonesian live drafts.
    pub whisper_draft_model: PathBuf,
    /// None follows the Windows default output; otherwise a WASAPI endpoint ID.
    pub audio_output_device: Option<String>,
    /// The whisper.cpp command line runner, used by the `whisper-sidecar` build.
    pub whisper_exe: PathBuf,
    /// Words and phrases the engine should favour during decoding.
    pub keywords: Vec<String>,
    /// Whisper worker threads; 0 allocates CPU capacity separately for draft/final workers.
    pub whisper_threads: i32,
    /// Hard cap on one utterance. Both engines handle at most 30 s.
    pub max_segment_secs: f32,
    /// Silence that closes an utterance.
    pub silence_close_ms: u32,
    /// Speech needed to open an utterance.
    pub speech_start_ms: u32,
    /// Utterances shorter than this are discarded as noise.
    pub min_segment_ms: u32,
    /// Energy gate threshold in dBFS; desktop silence is digital silence, well below this.
    pub vad_threshold_db: f32,
    /// Silence kept at the end of a closed utterance so trailing sounds are not clipped.
    pub tail_keep_ms: u32,
    /// Analysis window for the energy gate.
    pub frame_ms: u32,
    /// Utterances waiting for the transcriber before the oldest is dropped.
    pub queue_capacity: usize,
    /// Whistle decoder depth (2..=8); `None` uses the model's full depth.
    pub whistle_decoder_depth: Option<u8>,
    /// Ask the engine for per-word timings as well as text.
    pub word_timestamps: bool,
    /// Global hotkey that toggles listening, e.g. `"Win+P"` or `"Ctrl+Alt+Space"`.
    pub hotkey: String,
    /// Tried when `hotkey` cannot be registered; Windows keeps some `Win`+key combos for itself.
    pub hotkey_fallback: String,
    /// Begin listening on launch instead of idling until the hotkey is pressed.
    pub start_listening: bool,
    /// Show the small always-on-top indicator at the bottom right of the screen.
    pub show_indicator: bool,
    /// Show the "It's done" toast when a listening session settles, so one
    /// click opens the copy-ready result popup.
    pub show_result_popup: bool,
    /// Corner of the usable screen area the indicator is anchored to.
    pub indicator_position: IndicatorPosition,
    /// Gap between the indicator and the usable corner of the screen, in 96-DPI pixels. The
    /// usable area already excludes the taskbar, even when the taskbar hides itself.
    pub indicator_margin: i32,
    /// Indicator opacity, 0-255.
    pub indicator_opacity: u8,
    /// Save one Markdown file and a private JSONL journal per recording.
    /// When false, nothing is created on disk; the session stays in memory for
    /// viewing/copying until the user exports it manually.
    pub save_transcript: bool,
    /// Built-in viewer theme: dark, light, or following the Windows app theme.
    pub viewer_theme: ViewerTheme,
    /// Drop the speech engine after this many idle seconds (paused, no queue).
    /// 0 disables unloading; the engine reloads lazily on the next utterance.
    pub engine_idle_unload_secs: u64,
    pub reduced_transparency: bool,
    pub reduced_motion: bool,
    /// Where config, log and transcripts live. Not serialised: it is derived at load time.
    #[serde(skip)]
    pub base_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            language: Language::En,
            whistle_model: PathBuf::from("whistle.cact"),
            needle_exe: PathBuf::from("needle.exe"),
            whisper_model: PathBuf::from("ggml-small-q5_1.bin"),
            whisper_draft_model: PathBuf::from("ggml-base-q5_1.bin"),
            audio_output_device: None,
            whisper_exe: PathBuf::from("whisper-cli.exe"),
            keywords: Vec::new(),
            whisper_threads: 0,
            max_segment_secs: 25.0,
            silence_close_ms: 1000,
            speech_start_ms: 300,
            min_segment_ms: 400,
            vad_threshold_db: -45.0,
            tail_keep_ms: 200,
            frame_ms: 20,
            queue_capacity: 8,
            whistle_decoder_depth: None,
            word_timestamps: false,
            hotkey: "Ctrl+Alt+Space".to_string(),
            hotkey_fallback: "Ctrl+Alt+Space".to_string(),
            start_listening: false,
            show_indicator: true,
            show_result_popup: true,
            indicator_position: IndicatorPosition::BottomRight,
            indicator_margin: 24,
            indicator_opacity: 225,
            save_transcript: true,
            viewer_theme: ViewerTheme::System,
            engine_idle_unload_secs: 120,
            reduced_transparency: false,
            reduced_motion: false,
            base_dir: PathBuf::new(),
        }
    }
}

/// Add a base directory, its `vendor` engine folders, to the search order.
fn push_roots(roots: &mut Vec<PathBuf>, base: PathBuf) {
    roots.push(base.join("vendor").join("needle").join("windows-x86_64"));
    roots.push(base.join("vendor").join("whisper").join("Release"));
    roots.push(base.join("vendor").join("whisper"));
    roots.push(base);
}

/// A model file that could not be located, with the places that were searched.
#[derive(Debug, Clone)]
pub struct MissingModel {
    pub what: &'static str,
    pub searched: Vec<PathBuf>,
    pub hint: String,
}

impl std::fmt::Display for MissingModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} model not found (looked in {}). {}",
            self.what,
            self.searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            self.hint
        )
    }
}

impl Config {
    /// The folder holding config, log and transcripts.
    pub fn default_base_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os("DEPTH_HOME") {
            return PathBuf::from(dir);
        }
        // Portable installs made before the rename to Depth.
        if let Some(dir) = std::env::var_os("TRANSCRIBE_AI_HOME") {
            return PathBuf::from(dir);
        }
        if let Some(docs) = UserDirs::new().and_then(|d| d.document_dir().map(Path::to_path_buf)) {
            return docs.join("Depth");
        }
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Move a pre-rename `Documents\TranscribeAI` folder to `Documents\Depth`.
    ///
    /// Only runs for the default location (never for `--home` or `DEPTH_HOME`), and only when
    /// the new folder does not exist yet. When the move itself fails, the old folder is kept
    /// and used, so transcripts are never stranded by a half-finished rename.
    fn migrate_legacy_dir(new_dir: &Path) -> PathBuf {
        let new_dir = new_dir.to_path_buf();
        let Some(docs) = UserDirs::new().and_then(|d| d.document_dir().map(Path::to_path_buf))
        else {
            return new_dir;
        };
        if new_dir != docs.join("Depth") || new_dir.exists() {
            return new_dir;
        }
        let legacy = docs.join("TranscribeAI");
        if !legacy.exists() {
            return new_dir;
        }
        match std::fs::rename(&legacy, &new_dir) {
            Ok(()) => new_dir,
            Err(_) => legacy,
        }
    }

    /// Load `config.toml`, writing a commented default file the first time.
    pub fn load(base_dir: Option<PathBuf>) -> anyhow::Result<(Self, PathBuf)> {
        let base_dir = match base_dir {
            Some(dir) => dir,
            None => Self::migrate_legacy_dir(&Self::default_base_dir()),
        };
        let path = base_dir.join("config.toml");
        let mut config = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            toml::from_str::<Config>(&raw)
                .map_err(|e| anyhow::anyhow!("{} is not valid TOML: {e}", path.display()))?
        } else {
            let defaults = Config::default();
            std::fs::create_dir_all(&base_dir)?;
            std::fs::write(&path, defaults.to_toml_with_help())?;
            defaults
        };
        config.base_dir = base_dir;
        Ok((config, path))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        crate::hotkey::parse(&self.hotkey)?;
        crate::hotkey::parse(&self.hotkey_fallback)?;
        anyhow::ensure!(
            self.frame_ms > 0 && self.frame_ms <= 100,
            "Frame length must be 1-100 ms"
        );
        anyhow::ensure!(
            self.max_segment_secs.is_finite() && (1.0..=30.0).contains(&self.max_segment_secs),
            "Maximum segment must be 1-30 seconds"
        );
        anyhow::ensure!(
            self.speech_start_ms > 0 && self.silence_close_ms > 0,
            "Speech and silence windows must be positive"
        );
        anyhow::ensure!(
            self.min_segment_ms > 0
                && (self.min_segment_ms as f32) < self.max_segment_secs * 1000.0,
            "Minimum segment must be shorter than maximum segment"
        );
        anyhow::ensure!(
            self.tail_keep_ms <= self.silence_close_ms,
            "Silence tail must not exceed the silence window"
        );
        anyhow::ensure!(
            self.vad_threshold_db.is_finite() && (-100.0..=0.0).contains(&self.vad_threshold_db),
            "Gate threshold must be between -100 and 0 dBFS"
        );
        anyhow::ensure!(
            (1..=256).contains(&self.queue_capacity),
            "Queue capacity must be 1-256"
        );
        anyhow::ensure!(
            self.whisper_threads >= 0 && self.whisper_threads <= 256,
            "Worker threads must be 0-256"
        );
        anyhow::ensure!(
            self.whistle_decoder_depth
                .is_none_or(|n| (2..=8).contains(&n)),
            "Decoder depth must be 2-8, or omitted"
        );
        anyhow::ensure!(
            (0..=300).contains(&self.indicator_margin),
            "Overlay margin must be 0-300"
        );
        Ok(())
    }
    /// The file this configuration came from.
    pub fn config_path(&self) -> PathBuf {
        self.base_dir.join("config.toml")
    }

    /// Persist the current values (e.g. a tray-menu toggle) back to the config file.
    pub fn save(&self) -> anyhow::Result<()> {
        self.validate()?;
        let path = self.config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::recording::atomic_write(&path, self.to_toml_with_help().as_bytes())?;
        Ok(())
    }

    /// Folder the daily Markdown transcripts are appended to.
    pub fn transcripts_dir(&self) -> PathBuf {
        self.base_dir.join("transcripts")
    }

    /// Diagnostic log file.
    pub fn log_path(&self) -> PathBuf {
        self.base_dir.join("depth.log")
    }

    /// Roots searched for a relative path: next to the executable first (so an installed copy
    /// works no matter what the working directory is), then the data folder, then the working
    /// directory. Each root also gets its `vendor\needle` and `vendor\whisper` subfolders, and
    /// every candidate is tried both directly and under a `models\` subfolder.
    pub fn model_search_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            push_roots(&mut roots, dir.to_path_buf());
        }
        push_roots(&mut roots, self.base_dir.clone());
        if let Ok(cwd) = std::env::current_dir() {
            push_roots(&mut roots, cwd);
        }
        roots
    }

    /// Find a model file, or describe everywhere it was looked for.
    pub fn resolve_model(
        &self,
        spec: &Path,
        what: &'static str,
        hint: &str,
    ) -> Result<PathBuf, MissingModel> {
        let mut searched = Vec::new();
        let bases: Vec<PathBuf> = if spec.is_absolute() {
            vec![PathBuf::new()]
        } else {
            self.model_search_roots()
        };
        for root in bases {
            for candidate in [root.join(spec), root.join("models").join(spec)] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
                searched.push(candidate);
            }
        }
        Err(MissingModel {
            what,
            searched,
            hint: hint.to_string(),
        })
    }

    /// The model this configuration needs for its current language.
    pub fn resolve_active_model(&self) -> Result<PathBuf, MissingModel> {
        match self.language {
            Language::En => self.resolve_model(
                &self.whistle_model,
                "Whistle",
                "Run `python scripts/fetch_assets.py whistle`.",
            ),
            Language::Id => self.resolve_model(
                &self.whisper_model,
                "Whisper",
                "Run `python scripts/fetch_assets.py whisper --size small`.",
            ),
        }
    }

    /// A default file with explanatory comments, so the knobs are discoverable.
    fn to_toml_with_help(&self) -> String {
        let body = toml::to_string_pretty(self).unwrap_or_default();
        format!(
            "# depth configuration.\n\
             # Delete this file to regenerate it with defaults.\n\
             #\n\
             # language: \"en\" uses the Whistle model, \"id\" uses the Whisper checkpoint.\n\
             # model paths may be bare file names; they are looked up next to the\n\
             # executable, in this folder, and in a models\\ subfolder of either.\n\
             #\n\
             # show_indicator can also be flipped from the tray menu (\"Show overlay\").\n\
             # indicator_position: bottom-right (default), bottom-left, top-right, top-left.\n\
             # indicator_margin is a gap in 96-DPI pixels from that corner of the usable\n\
             # screen area, so the pill stays clear of the taskbar even when it hides itself.\n\
             #\n\
             # save_transcript: true saves one file per recording with a unique start-time ID;\n\
             # false keeps the session in memory only (view/copy/export manually).\n\
             # viewer_theme: system (default), light, or dark.\n\
             # engine_idle_unload_secs: drop the speech model after N idle seconds (0 = keep).\n\n{body}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_coherent() {
        let cfg = Config::default();
        assert_eq!(cfg.language, Language::En);
        assert!(
            cfg.max_segment_secs <= 30.0,
            "both engines cap a pass at 30 s"
        );
        assert!(cfg.silence_close_ms > 0 && cfg.speech_start_ms > 0);
        assert!(cfg.min_segment_ms as f32 / 1000.0 < cfg.max_segment_secs);
    }

    #[test]
    fn older_config_gets_live_defaults_without_changing_final_model() {
        let cfg: Config =
            toml::from_str("language = 'id'\nwhisper_model = 'custom-small.bin'\n").unwrap();
        assert_eq!(cfg.whisper_model, PathBuf::from("custom-small.bin"));
        assert_eq!(cfg.whisper_draft_model, PathBuf::from("ggml-base-q5_1.bin"));
        assert!(cfg.audio_output_device.is_none());
        let mut cfg = cfg;
        cfg.audio_output_device = Some("endpoint-id".into());
        let restored: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(restored.audio_output_device.as_deref(), Some("endpoint-id"));
    }

    #[test]
    fn language_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.language = Language::Id;
        let text = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.language, Language::Id);
        assert_eq!(back.language.code(), "id");
        assert_eq!(back.language.other(), Language::En);
    }

    #[test]
    fn partial_file_keeps_defaults_for_missing_keys() {
        let cfg: Config = toml::from_str("language = \"id\"\n").unwrap();
        assert_eq!(cfg.language, Language::Id);
        assert_eq!(cfg.queue_capacity, Config::default().queue_capacity);
        assert_eq!(cfg.indicator_position, IndicatorPosition::BottomRight);
    }

    #[test]
    fn indicator_position_round_trips_through_toml() {
        let cfg: Config = toml::from_str("indicator_position = \"top-left\"\n").unwrap();
        assert_eq!(cfg.indicator_position, IndicatorPosition::TopLeft);
        assert_eq!(cfg.indicator_position.key(), "top-left");
        assert_eq!(cfg.indicator_position.label(), "top left");

        let text = toml::to_string(&Config::default()).unwrap();
        assert!(
            text.contains("indicator_position = \"bottom-right\""),
            "got: {text}"
        );
    }

    #[test]
    fn an_unknown_indicator_position_is_rejected() {
        assert!(toml::from_str::<Config>("indicator_position = \"middle\"\n").is_err());
    }

    #[test]
    fn new_knobs_have_backwards_compatible_defaults() {
        // Old config files without the new keys must keep working.
        let cfg: Config = toml::from_str("language = \"en\"\n").unwrap();
        assert!(cfg.save_transcript);
        assert!(cfg.show_result_popup);
        assert_eq!(cfg.viewer_theme, ViewerTheme::System);
        assert_eq!(cfg.engine_idle_unload_secs, 120);
    }

    #[test]
    fn viewer_theme_round_trips_through_toml() {
        let cfg: Config = toml::from_str("viewer_theme = \"light\"\n").unwrap();
        assert_eq!(cfg.viewer_theme, ViewerTheme::Light);
        assert_eq!(cfg.viewer_theme.key(), "light");
        assert_eq!(ViewerTheme::parse("System"), Some(ViewerTheme::System));
        assert_eq!(ViewerTheme::parse("nope"), None);

        let text = toml::to_string(&Config::default()).unwrap();
        assert!(text.contains("viewer_theme = \"system\""), "got: {text}");
    }
}
#[cfg(test)]
mod validation_tests {
    use super::*;
    #[test]
    fn invalid_capture_settings_are_rejected() {
        let mut c = Config::default();
        c.frame_ms = 0;
        assert!(c.validate().is_err());
        c.frame_ms = 20;
        c.queue_capacity = 0;
        assert!(c.validate().is_err());
        c.queue_capacity = 8;
        c.max_segment_secs = f32::NAN;
        assert!(c.validate().is_err());
    }
    #[test]
    fn explicit_existing_preferences_survive_new_defaults() {
        let c: Config =
            toml::from_str("hotkey = 'Win+P'\nviewer_theme = 'dark'\nsave_transcript = false\n")
                .unwrap();
        assert_eq!(c.hotkey, "Win+P");
        assert_eq!(c.viewer_theme, ViewerTheme::Dark);
        assert!(!c.save_transcript);
        assert!(!c.reduced_motion);
    }
}

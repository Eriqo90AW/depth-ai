//! Recording identities, durable events, readable documents and legacy imports.
use crate::config::Language;
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RecordingStatus {
    Recording,
    Processing,
    Completed,
    Incomplete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub recording_id: String,
    pub sequence: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Legacy files contain clock times, not reliable capture-relative timing.
    pub clock_time: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub id: String,
    pub title: String,
    pub started: DateTime<Local>,
    pub language: Language,
    pub duration_ms: u64,
    pub status: RecordingStatus,
    pub autosave: bool,
    pub preview: String,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing)]
    pub segments: Vec<TranscriptSegment>,
    #[serde(skip)]
    pub source: Option<PathBuf>,
    #[serde(skip)]
    pub legacy_session: Option<usize>,
    #[serde(skip)]
    pub exported: bool,
}
impl Recording {
    pub fn new(language: Language, autosave: bool) -> Self {
        let started = Local::now();
        let id = format!(
            "{}-{}-{}",
            started.format("%Y%m%dT%H%M%S%.9f"),
            std::process::id(),
            NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        Self {
            id,
            title: started.format("Recording %b %d, %H:%M").to_string(),
            started,
            language,
            duration_ms: 0,
            status: RecordingStatus::Recording,
            autosave,
            preview: String::new(),
            warnings: vec![],
            segments: vec![],
            source: None,
            legacy_session: None,
            exported: false,
        }
    }
    pub fn can_delete(&self) -> bool {
        !matches!(
            self.status,
            RecordingStatus::Recording | RecordingStatus::Processing
        )
    }
    pub fn can_continue(&self) -> bool {
        self.legacy_session.is_none()
            && matches!(
                self.status,
                RecordingStatus::Completed | RecordingStatus::Incomplete
            )
    }
    pub fn metadata(&self) -> Self {
        Self {
            id: self.id.clone(),
            title: self.title.clone(),
            started: self.started,
            language: self.language,
            duration_ms: self.duration_ms,
            status: self.status.clone(),
            autosave: self.autosave,
            preview: self.preview.clone(),
            warnings: self.warnings.clone(),
            segments: vec![],
            source: self.source.clone(),
            legacy_session: self.legacy_session,
            exported: self.exported,
        }
    }
    pub fn text(&self, timed: bool) -> String {
        if timed {
            return self
                .segments
                .iter()
                .map(|s| {
                    format!(
                        "[{}] {}",
                        s.clock_time
                            .clone()
                            .unwrap_or_else(|| timestamp(s.start_ms)),
                        s.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        let mut result = String::new();
        let mut words = 0;
        let mut previous_end = 0;
        for segment in &self.segments {
            if words > 0 && segment.start_ms.saturating_sub(previous_end) >= 4000 {
                result.push_str("\n\n");
                words = 0;
            }
            for word in segment.text.split_whitespace() {
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push(' ');
                }
                result.push_str(word);
                words += 1;
                if words >= 100 && word.ends_with(['.', '!', '?', '。', '！', '？']) {
                    result.push_str("\n\n");
                    words = 0;
                }
            }
            if words >= 150 {
                result.push_str("\n\n");
                words = 0;
            }
            previous_end = segment.end_ms;
        }
        result.trim_end().to_string()
    }
    pub fn markdown(&self) -> String {
        let warnings = if self.warnings.is_empty() {
            String::new()
        } else {
            format!("\n> Incomplete: {}\n", self.warnings.join("; "))
        };
        format!(
            "# {}\n\n{} · {} · {}\n{}\n{}\n",
            self.title.replace(['\r', '\n'], " "),
            self.started.format("%Y-%m-%d %H:%M:%S %:z"),
            self.language.label(),
            timestamp(self.duration_ms),
            warnings,
            self.text(false)
        )
    }
    pub fn label(&self) -> &'static str {
        match self.status {
            RecordingStatus::Recording => "Recording",
            RecordingStatus::Processing => "Finishing transcription",
            RecordingStatus::Completed => "Completed",
            RecordingStatus::Incomplete => "Incomplete",
        }
    }
}
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn timestamp(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum PipelineEvent {
    Started(Recording),
    Segment(TranscriptSegment),
    State {
        id: String,
        status: RecordingStatus,
        duration_ms: u64,
    },
    Warning {
        id: String,
        message: String,
    },
    Renamed {
        id: String,
        title: String,
    },
    Deleted {
        id: String,
    },
}

pub struct RecordingStore {
    dir: PathBuf,
    file: Option<std::fs::File>,
    last_render: Instant,
    dirty: bool,
}
impl RecordingStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            file: None,
            last_render: Instant::now() - Duration::from_secs(2),
            dirty: false,
        }
    }
    pub fn begin(&mut self, recording: &mut Recording) -> std::io::Result<()> {
        if !recording.autosave {
            return Ok(());
        }
        fs::create_dir_all(self.dir.join(".events"))?;
        let path = self
            .dir
            .join(".events")
            .join(format!("{}.jsonl", recording.id));
        self.file = Some(
            OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(&path)?,
        );
        recording.source = Some(path);
        self.append(&PipelineEvent::Started(recording.clone()))?;
        self.render(recording, true)
    }
    /// Reopen the same journal without writing another Started header.
    pub fn resume(&mut self, recording: &mut Recording) -> std::io::Result<()> {
        if !recording.can_continue() {
            return Err(std::io::Error::other("This recording cannot be continued"));
        }
        if recording.autosave {
            let path = recording
                .source
                .as_ref()
                .ok_or_else(|| std::io::Error::other("Recording journal is missing"))?;
            let bytes = fs::read(path)?;
            // Appending behind a damaged tail would make the new results unrecoverable.
            for line in bytes.split_inclusive(|byte| *byte == b'\n') {
                serde_json::from_slice::<PipelineEvent>(line)
                    .map_err(|_| std::io::Error::other("The recording journal is damaged. Export the recovered text and start a new recording."))?;
            }
            let mut file = OpenOptions::new().append(true).open(path)?;
            if !bytes.ends_with(b"\n") {
                file.write_all(b"\n")?;
            }
            self.file = Some(file);
        }
        self.append(&PipelineEvent::State {
            id: recording.id.clone(),
            status: RecordingStatus::Recording,
            duration_ms: recording.duration_ms,
        })?;
        recording.status = RecordingStatus::Recording;
        recording.exported = false;
        Ok(())
    }
    pub fn append(&mut self, event: &PipelineEvent) -> std::io::Result<()> {
        self.dirty = true;
        if let Some(file) = self.file.as_mut() {
            serde_json::to_writer(&mut *file, event)?;
            file.write_all(b"\n")?;
            file.flush()?;
            file.sync_data()?;
        }
        Ok(())
    }
    pub fn render(&mut self, recording: &Recording, force: bool) -> std::io::Result<()> {
        if !recording.autosave || recording.source.is_none() {
            return Ok(());
        }
        if !force && (!self.dirty || self.last_render.elapsed() < Duration::from_millis(600)) {
            return Ok(());
        }
        atomic_write(
            &self.dir.join(format!("{}.md", recording.id)),
            recording.markdown().as_bytes(),
        )?;
        self.last_render = Instant::now();
        self.dirty = false;
        Ok(())
    }
}
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension(format!(
        "{}-{}-{}.tmp",
        std::process::id(),
        Local::now().format("%Y%m%d%H%M%S%.9f"),
        NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    // MoveFileExW provides replacement on Windows, where fs::rename refuses an existing target.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            },
            core::PCWSTR,
        };
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(std::io::Error::other)?;
    }
    #[cfg(not(windows))]
    fs::rename(temp, path)?;
    Ok(())
}
fn replay(path: &Path, load_segments: bool) -> std::io::Result<Recording> {
    let mut recording: Option<Recording> = None;
    let mut damaged = false;
    for line in BufReader::new(std::fs::File::open(path)?).lines() {
        let line = line?;
        let event = match serde_json::from_str::<PipelineEvent>(&line) {
            Ok(event) => event,
            Err(_) => {
                damaged = true;
                break;
            }
        };
        match event {
            PipelineEvent::Started(mut r) => {
                r.source = Some(path.to_path_buf());
                recording = Some(r);
            }
            PipelineEvent::Segment(segment) => {
                if let Some(r) = recording.as_mut() {
                    if r.preview.is_empty() {
                        r.preview = segment.text.chars().take(110).collect();
                    }
                    if load_segments {
                        r.segments.push(segment);
                    }
                }
            }
            PipelineEvent::State {
                status,
                duration_ms,
                ..
            } => {
                if let Some(r) = recording.as_mut() {
                    r.status = status;
                    r.duration_ms = duration_ms;
                }
            }
            PipelineEvent::Warning { message, .. } => {
                if let Some(r) = recording.as_mut() {
                    r.warnings.push(message);
                }
            }
            PipelineEvent::Renamed { title, .. } => {
                if let Some(r) = recording.as_mut() {
                    r.title = title;
                }
            }
            PipelineEvent::Deleted { .. } => {}
        }
    }
    let mut r = recording.ok_or_else(|| std::io::Error::other("Recording header is missing"))?;
    if damaged
        || matches!(
            r.status,
            RecordingStatus::Recording | RecordingStatus::Processing
        )
    {
        r.status = RecordingStatus::Incomplete;
        r.warnings
            .push("Interrupted recording. Recovered all intact saved results.".into());
    }
    Ok(r)
}
pub fn load_recording(meta: &Recording) -> std::io::Result<Recording> {
    if let Some(path) = &meta.source {
        if let Some(index) = meta.legacy_session {
            let mut r = legacy(path)?
                .into_iter()
                .nth(index)
                .ok_or_else(|| std::io::Error::other("Legacy session is missing"))?;
            r.title = meta.title.clone();
            return Ok(r);
        }
        return replay(path, true);
    }
    Ok(meta.clone())
}
pub fn library(dir: &Path) -> std::io::Result<Vec<Recording>> {
    let mut result = vec![];
    let events = dir.join(".events");
    if events.exists() {
        for entry in fs::read_dir(events)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                match replay(&path, false) {
                    Ok(r) => result.push(r),
                    Err(e) => {
                        let mut r = Recording::new(Language::En, true);
                        r.id = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        r.title = "Damaged recording".into();
                        r.status = RecordingStatus::Incomplete;
                        r.warnings.push(format!("Cannot read recording: {e}"));
                        r.source = Some(path);
                        result.push(r);
                    }
                }
            }
        }
    }
    if dir.exists() {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("transcript-") && n.ends_with(".md"))
            {
                for mut r in legacy(&path)? {
                    r.segments.clear();
                    result.push(r);
                }
            }
        }
    }
    let titles_path = dir.join(".events").join("titles.json");
    if titles_path.exists() {
        let titles: std::collections::HashMap<String, String> =
            serde_json::from_slice(&fs::read(titles_path)?)?;
        for r in &mut result {
            if let Some(title) = titles.get(&r.id) {
                r.title = title.clone();
            }
        }
    }
    let deleted = deleted_ids(dir)?;
    result.retain(|r| !deleted.contains(&r.id));
    result.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.id.cmp(&a.id)));
    Ok(result)
}
fn legacy(path: &Path) -> std::io::Result<Vec<Recording>> {
    use chrono::{NaiveDate, NaiveTime, TimeZone};
    let raw = fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("transcript");
    let date = name
        .strip_prefix("transcript-")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Local::now().date_naive());
    let mut recordings: Vec<Recording> = vec![];
    for line in raw.lines() {
        if line.starts_with("## Full text") {
            break;
        }
        if line.starts_with("## Session") {
            let parts: Vec<_> = line.split('—').map(str::trim).collect();
            let time = parts
                .get(1)
                .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok())
                .unwrap_or_default();
            let mut r = Recording::new(
                if parts.get(2).is_some_and(|s| *s == "Indonesian") {
                    Language::Id
                } else {
                    Language::En
                },
                true,
            );
            r.started = Local
                .from_local_datetime(&date.and_time(time))
                .earliest()
                .unwrap_or_else(Local::now);
            let index = recordings.len();
            r.id = format!("legacy-{name}-{index}");
            r.title = format!("{} · {}", date, parts.first().unwrap_or(&"Session"));
            r.status = RecordingStatus::Completed;
            r.source = Some(path.to_path_buf());
            r.legacy_session = Some(index);
            recordings.push(r);
        } else if let Some(rest) = line.strip_prefix("**")
            && let Some((clock, text)) = rest.split_once("**")
            && let Some(r) = recordings.last_mut()
        {
            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }
            if r.preview.is_empty() {
                r.preview = text.chars().take(110).collect();
            }
            r.segments.push(TranscriptSegment {
                recording_id: r.id.clone(),
                sequence: r.segments.len() as u64,
                start_ms: 0,
                end_ms: 0,
                text,
                clock_time: Some(clock.into()),
            });
        }
    }
    Ok(recordings)
}
fn deleted_ids(dir: &Path) -> std::io::Result<std::collections::BTreeSet<String>> {
    match fs::read(dir.join(".events").join("deleted.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(error) => Err(error),
    }
}

/// Delete only this recording's managed files. Imported sessions share a source file.
pub fn delete(recording: &Recording, dir: &Path) -> std::io::Result<()> {
    if !recording.can_delete() {
        return Err(std::io::Error::other(
            "Stop recording and wait for transcription to finish before deleting it.",
        ));
    }
    if recording.legacy_session.is_some() {
        let mut deleted = deleted_ids(dir)?;
        deleted.insert(recording.id.clone());
        fs::create_dir_all(dir.join(".events"))?;
        return atomic_write(
            &dir.join(".events").join("deleted.json"),
            &serde_json::to_vec(&deleted)?,
        );
    }
    let Some(source) = &recording.source else {
        return Ok(());
    };
    let mut components = Path::new(&recording.id).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(std::io::Error::other("Invalid recording identity"));
    }
    let journal = dir.join(".events").join(format!("{}.jsonl", recording.id));
    if source != &journal {
        return Err(std::io::Error::other(
            "Recording files are outside the recordings folder",
        ));
    }
    let root = match fs::canonicalize(dir) {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let files = [dir.join(format!("{}.md", recording.id)), journal];
    for path in &files {
        match fs::metadata(path) {
            Ok(metadata) => {
                if !metadata.is_file() || !fs::canonicalize(path)?.starts_with(&root) {
                    return Err(std::io::Error::other(
                        "Recording files are outside the recordings folder",
                    ));
                }
                if metadata.permissions().readonly() {
                    return Err(std::io::Error::other("Recording files are read-only"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    // The journal contains the authoritative text. Remove it after the readable document.
    for path in files {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn rename(recording: &mut Recording, title: &str, dir: &Path) -> std::io::Result<()> {
    if title.trim().is_empty() {
        return Err(std::io::Error::other("Enter a title"));
    }
    if recording.legacy_session.is_some() {
        let path = dir.join(".events").join("titles.json");
        let mut titles: std::collections::HashMap<String, String> = if path.exists() {
            serde_json::from_slice(&fs::read(&path)?)?
        } else {
            Default::default()
        };
        let title = title.trim().replace(['\r', '\n'], " ");
        titles.insert(recording.id.clone(), title.clone());
        fs::create_dir_all(dir.join(".events"))?;
        atomic_write(&path, &serde_json::to_vec(&titles)?)?;
        recording.title = title;
        return Ok(());
    }
    recording.title = title.trim().replace(['\r', '\n'], " ");
    if let Some(path) = &recording.source {
        if recording.legacy_session.is_some() {
            let path = dir.join(".events").join("titles.json");
            let mut titles: std::collections::HashMap<String, String> = if path.exists() {
                serde_json::from_slice(&fs::read(&path)?)?
            } else {
                Default::default()
            };
            let title = title.trim().replace(['\r', '\n'], " ");
            titles.insert(recording.id.clone(), title.clone());
            fs::create_dir_all(dir.join(".events"))?;
            atomic_write(&path, &serde_json::to_vec(&titles)?)?;
            recording.title = title;
            return Ok(());
        }
        let mut file = OpenOptions::new().append(true).open(path)?;
        serde_json::to_writer(
            &mut file,
            &PipelineEvent::Renamed {
                id: recording.id.clone(),
                title: recording.title.clone(),
            },
        )?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        atomic_write(
            &dir.join(format!("{}.md", recording.id)),
            recording.markdown().as_bytes(),
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        std::env::temp_dir().join(Recording::new(Language::En, false).id)
    }
    fn segment(r: &Recording, n: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            recording_id: r.id.clone(),
            sequence: n,
            start_ms: n * 1000,
            end_ms: n * 1000 + 500,
            text: text.into(),
            clock_time: None,
        }
    }
    #[test]
    fn complete_long_unicode_document_without_duplication() {
        let mut r = Recording::new(Language::En, false);
        for n in 0..2501 {
            r.segments.push(segment(&r, n, "Hello 世界. Selamat pagi!"));
        }
        assert_eq!(r.text(false).matches("世界").count(), 2501);
        assert_eq!(r.markdown().matches("世界").count(), 2501);
    }
    #[test]
    fn off_never_creates_storage() {
        let dir = temp();
        let mut r = Recording::new(Language::En, false);
        let mut store = RecordingStore::new(dir.clone());
        store.begin(&mut r).unwrap();
        store
            .append(&PipelineEvent::Segment(segment(&r, 0, "hello")))
            .unwrap();
        store.render(&r, true).unwrap();
        assert!(!dir.exists());
    }
    #[test]
    fn crash_recovers_only_intact_results() {
        let dir = temp();
        let mut r = Recording::new(Language::En, true);
        let mut store = RecordingStore::new(dir.clone());
        store.begin(&mut r).unwrap();
        let s = segment(&r, 0, "Saved result.");
        store.append(&PipelineEvent::Segment(s)).unwrap();
        drop(store);
        let mut file = OpenOptions::new()
            .append(true)
            .open(r.source.as_ref().unwrap())
            .unwrap();
        file.write_all(b"{broken").unwrap();
        drop(file);
        let loaded = load_recording(&r).unwrap();
        assert_eq!(loaded.status, RecordingStatus::Incomplete);
        assert_eq!(loaded.text(false), "Saved result.");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn completion_and_atomic_replacement() {
        let dir = temp();
        let mut r = Recording::new(Language::En, true);
        let mut store = RecordingStore::new(dir.clone());
        store.begin(&mut r).unwrap();
        r.segments.push(segment(&r, 0, "One copy."));
        store
            .append(&PipelineEvent::Segment(r.segments[0].clone()))
            .unwrap();
        r.status = RecordingStatus::Completed;
        store
            .append(&PipelineEvent::State {
                id: r.id.clone(),
                status: r.status.clone(),
                duration_ms: 3600000,
            })
            .unwrap();
        store.render(&r, true).unwrap();
        store.render(&r, true).unwrap();
        drop(store);
        assert_eq!(library(&dir).unwrap()[0].duration_ms, 3600000);
        assert!(library(&dir).unwrap()[0].segments.is_empty());
        assert_eq!(load_recording(&r).unwrap().text(false), "One copy.");
        fs::remove_dir_all(dir).unwrap();
    }
}
#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn legacy_sessions_keep_clock_times_and_original_files() {
        let dir = std::env::temp_dir().join(Recording::new(Language::En, false).id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript-2025-06-01.md");
        let original = "# Transcript — 2025-06-01\n\n## Session 1 — 23:58 — English\n\n**23:58:40**  Original words.\n\n## Session 2 — 23:59 — Indonesian\n\n**23:59:10**  Selamat malam.\n\n## Full text\n\nOriginal words. Selamat malam.\n";
        fs::write(&path, original).unwrap();
        let mut entries = library(&dir).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|r| r.segments.is_empty()));
        let selected = load_recording(&entries[0]).unwrap();
        assert_eq!(selected.segments[0].clock_time.as_deref(), Some("23:59:10"));
        assert_eq!(selected.segments[0].start_ms, 0);
        rename(&mut entries[0], "Late notes", &dir).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(library(&dir).unwrap()[0].title, "Late notes");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn start_identity_survives_midnight_and_empty_recordings() {
        use chrono::TimeZone;
        let dir = std::env::temp_dir().join(Recording::new(Language::En, false).id);
        let mut r = Recording::new(Language::En, true);
        r.started = Local.with_ymd_and_hms(2025, 6, 1, 23, 59, 59).unwrap();
        let id = r.id.clone();
        let mut store = RecordingStore::new(dir.clone());
        store.begin(&mut r).unwrap();
        r.duration_ms = 120000;
        r.status = RecordingStatus::Completed;
        store
            .append(&PipelineEvent::State {
                id: id.clone(),
                status: r.status.clone(),
                duration_ms: r.duration_ms,
            })
            .unwrap();
        store.render(&r, true).unwrap();
        drop(store);
        let loaded = library(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, id);
        assert_eq!(loaded[0].started, r.started);
        assert!(load_recording(&loaded[0]).unwrap().segments.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_atomic_replacement_keeps_the_existing_target() {
        let dir = std::env::temp_dir().join(Recording::new(Language::En, false).id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.md");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("original"), "keep").unwrap();
        assert!(atomic_write(&path, b"replacement").is_err());
        assert_eq!(fs::read_to_string(path.join("original")).unwrap(), "keep");
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    fn root() -> PathBuf {
        std::env::temp_dir().join(Recording::new(Language::En, false).id)
    }
    fn clean_up(dir: &Path) {
        let resolved = fs::canonicalize(dir).unwrap();
        assert!(resolved.starts_with(fs::canonicalize(std::env::temp_dir()).unwrap()));
        fs::remove_dir_all(resolved).unwrap();
    }
    fn saved(dir: &Path) -> Recording {
        let mut r = Recording::new(Language::En, true);
        let mut store = RecordingStore::new(dir.to_path_buf());
        store.begin(&mut r).unwrap();
        r.status = RecordingStatus::Completed;
        store
            .append(&PipelineEvent::State {
                id: r.id.clone(),
                status: r.status.clone(),
                duration_ms: 0,
            })
            .unwrap();
        store.render(&r, true).unwrap();
        r
    }
    #[test]
    fn native_delete_removes_only_selected_managed_files_and_survives_reload() {
        let root = root();
        let dir = root.join("transcripts");
        let first = saved(&dir);
        let second = saved(&dir);
        let exported = root.join("exported.md");
        fs::write(&exported, "Keep this export").unwrap();
        delete(&first, &dir).unwrap();
        assert!(!first.source.unwrap().exists());
        assert!(!dir.join(format!("{}.md", first.id)).exists());
        assert!(second.source.unwrap().exists());
        assert!(dir.join(format!("{}.md", second.id)).exists());
        assert_eq!(fs::read_to_string(exported).unwrap(), "Keep this export");
        let entries = library(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, second.id);
        clean_up(&root);
    }
    #[test]
    fn active_recordings_are_protected_and_unsaved_delete_creates_no_files() {
        let dir = root();
        let mut r = Recording::new(Language::En, false);
        assert!(delete(&r, &dir).is_err());
        r.status = RecordingStatus::Processing;
        assert!(delete(&r, &dir).is_err());
        r.status = RecordingStatus::Completed;
        delete(&r, &dir).unwrap();
        assert!(!dir.exists());
    }
    #[test]
    fn imported_delete_preserves_other_sessions_and_original_file() {
        let dir = root();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript-2025-06-01.md");
        let original = "# Transcript\n\n## Session 1 — 10:00 — English\n\n**10:00:01** First words.\n\n## Session 2 — 11:00 — English\n\n**11:00:01** Other words.\n";
        fs::write(&path, original).unwrap();
        let entries = library(&dir).unwrap();
        assert_eq!(entries.len(), 2);
        delete(&entries[0], &dir).unwrap();
        let remaining = library(&dir).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, entries[1].id);
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        clean_up(&dir);
    }
    #[test]
    fn unrelated_source_is_rejected_without_deleting_any_files() {
        let root = root();
        let dir = root.join("transcripts");
        let mut r = saved(&dir);
        let original_source = r.source.clone().unwrap();
        let outside = root.join("other.jsonl");
        fs::write(&outside, "Unrelated data").unwrap();
        r.source = Some(outside.clone());
        assert!(delete(&r, &dir).is_err());
        assert!(original_source.exists());
        assert!(dir.join(format!("{}.md", r.id)).exists());
        assert_eq!(fs::read_to_string(outside).unwrap(), "Unrelated data");
        clean_up(&root);
    }
}

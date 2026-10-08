//! Legacy daily writer retained for compatibility tests. New recordings use `recording`.
//!
//! Layout of each day file:
//!
//! ```markdown
//! # Transcript — 2025-06-01
//!
//! ## Session 1 — 14:02 — English
//!
//! **14:02:11**  And so, my fellow Americans.
//! **14:02:19**  Ask what you can do for your country.
//!
//! ## Session 2 — 14:35 — English
//!
//! **14:35:02**  A new listening run.
//!
//! ## Full text
//!
//! And so, my fellow Americans. Ask what you can do for your country.
//!
//! A new listening run.
//! ```
//!
//! Rules:
//!
//! * **(A) Sessions are listening instances, never clock minutes.** A new
//!   `## Session N — HH:MM — Language` header is written only when a new
//!   listening instance starts (hotkey Toggle/Resume, language switch, autosave
//!   toggle, day rollover into a fresh file, or a fresh process run). The clock
//!   minute changing mid-session never splits a session. `N` is a per-day
//!   counter (1, 2, …) so two instances started in the same minute stay distinct,
//!   and `HH:MM` is just the start-time label, not a grouping key.
//! * **(B) Full text holds everything regardless of time.** `## Full text` is a
//!   trailing section with the same words as the session blocks but no
//!   timestamps: one paragraph per session instance, utterances joined with
//!   spaces. It is regenerated from the session blocks on every write, so it
//!   stays consistent across process restarts.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate};

use crate::config::Language;

/// Which language a session was transcribed in, for the Markdown header.
///
/// The header names only the language (e.g. `## Session 1 — 14:02 — English`):
/// engine internals are a diagnostic detail and stay in the log, not the transcript.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub language: Language,
}

/// Marker that starts the combined section. Sessions are everything before it.
const FULL_MARKER: &str = "\n## Full text\n";
/// The combined section header, written ahead of the combined body.
const FULL_HEADER: &str = "\n## Full text\n\n";

/// Writes `transcripts/transcript-YYYY-MM-DD.md`, appending a line per utterance.
///
/// When `save_enabled` is false, nothing touches the disk; every line that *would*
/// have been written is kept in a complete in-memory buffer instead, so the viewer
/// can still show/copy it and the user can export it manually later.
pub struct TranscriptWriter {
    dir: PathBuf,
    day: Option<NaiveDate>,
    path: Option<PathBuf>,
    session: Option<SessionInfo>,
    /// Whether the session header has been written into the current day's file.
    session_written: bool,
    save_enabled: bool,
    /// Lines that would have gone to disk (title, headers, utterances), oldest-first.
    memory: Vec<String>,
    /// Day the in-memory buffer currently describes.
    memory_day: Option<NaiveDate>,
    /// Next session number to use for the current day (1-based, per-day instances).
    session_counter: usize,
}

impl TranscriptWriter {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            day: None,
            path: None,
            session: None,
            session_written: false,
            save_enabled: true,
            memory: Vec::new(),
            memory_day: None,
            session_counter: 1,
        }
    }

    /// Turn automatic file writes on/off. Turning it off never deletes files;
    /// turning it back on resumes appending (a fresh session header is written).
    pub fn set_save_enabled(&mut self, enabled: bool) {
        if self.save_enabled != enabled {
            self.save_enabled = enabled;
            // Force a fresh header/ensure on the next write in either direction.
            self.session_written = enabled && self.session_written && self.day.is_some();
            if !enabled {
                self.session_written = false;
            }
        }
    }

    pub fn save_enabled(&self) -> bool {
        self.save_enabled
    }

    /// Everything captured so far for the current day, as one Markdown document.
    /// Used by the viewer/export path when autosave is off (and for Copy).
    ///
    /// This is the session blocks plus the trailing `## Full text` section,
    /// derived from the same in-memory lines so the two always agree.
    pub fn memory_markdown(&self) -> String {
        let sessions: String = self.memory.concat();
        let body = full_body_from_sessions(&sessions);
        if body.is_empty() {
            sessions
        } else {
            format!("{sessions}{FULL_HEADER}{body}\n")
        }
    }

    /// Number of lines currently held in memory (session area only; the derived
    /// Full text section is not counted toward the cap).
    pub fn memory_lines(&self) -> usize {
        self.memory.len()
    }

    /// Write the in-memory buffer to an explicit path (the manual Export action).
    pub fn export_memory(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.memory_markdown())
    }

    fn remember(&mut self, text: &str) {
        self.memory.push(text.to_string());
    }

    /// Path of the file for a given day.
    pub fn path_for(&self, day: NaiveDate) -> PathBuf {
        self.dir
            .join(format!("transcript-{}.md", day.format("%Y-%m-%d")))
    }

    /// The file currently being appended to, once a session has started.
    pub fn current_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Start (or restart, after a pause) a session: writes the day and session headers.
    ///
    /// The header is one per listening instance: `## Session N — HH:MM — Language`.
    pub fn begin_session(
        &mut self,
        now: DateTime<Local>,
        info: SessionInfo,
    ) -> std::io::Result<PathBuf> {
        let path = self.ensure_day(now.date_naive())?;
        let n = self.session_counter;
        self.session_counter += 1;
        let header = format!(
            "\n## Session {n} — {} — {}\n\n",
            now.format("%H:%M"),
            info.language.label(),
        );
        if self.save_enabled {
            insert_into_sessions(&path, &header)?;
        }
        self.remember(&header);
        self.session = Some(info);
        self.session_written = true;
        Ok(path)
    }

    /// Append one utterance. Opens a session implicitly if the day rolled over mid-session.
    pub fn append(&mut self, now: DateTime<Local>, text: &str) -> std::io::Result<()> {
        let normalized = normalize(text);
        if normalized.is_empty() {
            return Ok(());
        }
        let path = self.ensure_day(now.date_naive())?;
        if !self.session_written {
            // A new day started while the session was running: repeat the header in the new file.
            // This is still the same listening instance continued, but each day file numbers
            // its own sessions, so it gets the next per-day number with the new start label.
            if let Some(info) = self.session.clone() {
                let n = self.session_counter;
                self.session_counter += 1;
                let header = format!(
                    "\n## Session {n} — {} — {}\n\n",
                    now.format("%H:%M"),
                    info.language.label(),
                );
                if self.save_enabled {
                    insert_into_sessions(&path, &header)?;
                }
                self.remember(&header);
                self.session_written = true;
            }
        }
        let line = format!("**{}**  {}\n", now.format("%H:%M:%S"), normalized);
        if self.save_enabled {
            insert_into_sessions(&path, &line)?;
        }
        self.remember(&line);
        Ok(())
    }

    /// Ensure the day file exists with its title, switching files when the date changes.
    ///
    /// With saving disabled this only updates the in-memory day (no directory or
    /// file is created) but still returns the path the file *would* use.
    fn ensure_day(&mut self, day: NaiveDate) -> std::io::Result<PathBuf> {
        if self.day == Some(day)
            && let Some(path) = &self.path
        {
            return Ok(path.clone());
        }
        let path = self.path_for(day);
        let title = format!("# Transcript — {}\n", day.format("%Y-%m-%d"));
        if self.memory_day != Some(day) {
            self.remember(&title);
            self.memory_day = Some(day);
        }
        if self.save_enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if !path.exists() {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                file.write_all(title.as_bytes())?;
            }
            // Number sessions per day file: continue after headers left by earlier runs.
            let sessions = read_sessions_part(&path)?;
            self.session_counter = count_sessions(&sessions) + 1;
        } else if self.day.is_some() {
            // Saving off and the day changed: fresh per-day numbering for the buffer.
            self.session_counter = 1;
        }
        if self.day.is_some() {
            // Day rolled over: the new file needs its own session header.
            self.session_written = false;
        }
        self.day = Some(day);
        self.path = Some(path.clone());
        Ok(path)
    }
}

/// Collapse whitespace so one utterance always occupies exactly one Markdown line.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split full file content into the session area (everything before Full text).
fn split_sessions(full: &str) -> &str {
    match full.find(FULL_MARKER) {
        Some(at) => &full[..at],
        None => full,
    }
}

/// Read the session area of `path`, ignoring any trailing `## Full text` section.
/// Missing files read as empty.
fn read_sessions_part(path: &Path) -> std::io::Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let full = std::fs::read_to_string(path)?;
    Ok(split_sessions(&full).to_string())
}

/// Count `## Session` headers in a session-area string (per-day instances).
fn count_sessions(sessions: &str) -> usize {
    sessions
        .lines()
        .filter(|l| l.trim().starts_with("## Session"))
        .count()
}

/// Extract the body of a `**HH:MM:SS** rest` utterance line, if it is one.
fn utterance_body(line: &str) -> Option<String> {
    let rest = line.strip_prefix("**")?;
    let end = rest.find("**")?;
    let (time, body) = rest.split_at(end);
    if !time.contains(':') {
        return None;
    }
    let body = body[2..].replace("**", "").trim().to_string();
    if body.is_empty() { None } else { Some(body) }
}

/// Derive the Full text body from a session area: one paragraph per `## Session`
/// instance, each paragraph joining that session's utterance bodies with spaces.
/// Plain notes and headers never enter the combined text; timestamps are dropped.
fn full_body_from_sessions(sessions: &str) -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in sessions.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## Session") {
            if !current.is_empty() {
                paragraphs.push(current.join(" "));
                current.clear();
            }
        } else if let Some(body) = utterance_body(trimmed) {
            current.push(body);
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }
    paragraphs.join("\n\n")
}

/// Insert `chunk` (a session header or an utterance line) at the end of the
/// session area — i.e. before any trailing `## Full text` — then regenerate
/// `## Full text` from the updated sessions so both sections always agree.
fn insert_into_sessions(path: &Path, chunk: &str) -> std::io::Result<()> {
    let full = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    let sessions = split_sessions(&full);
    let mut updated = String::with_capacity(sessions.len() + chunk.len() + 512);
    updated.push_str(sessions);
    updated.push_str(chunk);
    let body = full_body_from_sessions(&updated);
    let mut out = updated;
    if !body.is_empty() {
        out.push_str(FULL_HEADER);
        out.push_str(&body);
        out.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, out)
}

/// The most recent `transcript-*.md` in `dir`, so "View transcript" has something to open even
/// when this process has not written one yet.
///
/// File names begin with an ISO date, so the greatest name is the newest day.
pub fn newest_transcript(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !(name.starts_with("transcript-") && name.ends_with(".md")) || !path.is_file() {
            continue;
        }
        if newest.as_ref().is_none_or(|(best, _)| name > best.as_str()) {
            newest = Some((name.to_string(), path));
        }
    }
    newest.map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(normalize("  hello \n world\tagain "), "hello world again");
        assert_eq!(normalize("\n\n"), "");
    }

    #[test]
    fn path_is_per_day() {
        let w = TranscriptWriter::new("C:\\out");
        let day = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        assert!(w.path_for(day).ends_with("transcript-2025-06-01.md"));
    }

    #[test]
    fn full_text_derives_from_sessions_without_timestamps() {
        let sessions = "# Transcript — 2025-06-01\n\n## Session 1 — 14:02 — English\n\n**14:02:11**  hello world\n**14:02:19**  second line\n";
        let body = full_body_from_sessions(sessions);
        assert_eq!(body, "hello world second line");
        assert!(!body.contains("14:02"), "no timestamps in Full text");
    }

    #[test]
    fn full_text_groups_one_paragraph_per_session() {
        let sessions = "## Session 1 — 09:00 — English\n\n**09:00:01**  first\n\n## Session 2 — 10:00 — English\n\n**10:00:01**  second\n";
        let body = full_body_from_sessions(sessions);
        assert_eq!(body, "first\n\nsecond");
    }

    #[test]
    fn disabled_saving_writes_nothing_but_keeps_memory() {
        let dir = std::env::temp_dir().join(format!(
            "depth-writer-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut w = TranscriptWriter::new(&dir);
        w.set_save_enabled(false);
        assert!(!w.save_enabled());
        let now = Local::now();
        let info = SessionInfo {
            language: Language::En,
        };
        let path = w.begin_session(now, info).unwrap();
        w.append(now, "hello world").unwrap();
        // No directory or file may have been created.
        assert!(!path.exists(), "autosave is off, no file may be created");
        assert!(
            !dir.exists(),
            "autosave is off, no directory may be created"
        );
        let md = w.memory_markdown();
        assert!(md.contains("# Transcript"), "title must be in memory");
        assert!(
            md.contains("## Session"),
            "session header must be in memory"
        );
        assert!(md.contains("hello world"), "utterance must be in memory");
        assert!(
            md.contains("## Full text"),
            "combined section must be in memory"
        );
        // Manual export still works.
        let out = dir.with_extension("export.md");
        w.export_memory(&out).unwrap();
        let saved = std::fs::read_to_string(&out).unwrap();
        assert!(saved.contains("hello world"));
        assert!(saved.contains("## Full text"));
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn memory_buffer_keeps_the_whole_session() {
        let mut w = TranscriptWriter::new("C:\\out");
        w.set_save_enabled(false);
        let now = Local::now();
        let info = SessionInfo {
            language: Language::En,
        };
        w.begin_session(now, info).unwrap();
        for i in 0..2100 {
            w.append(now, &format!("line {i}")).unwrap();
        }
        assert!(w.memory_lines() >= 2100);
        assert!(w.memory_markdown().contains(&format!("line {}", 2099)));
    }
}

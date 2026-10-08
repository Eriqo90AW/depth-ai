//! Filesystem behaviour of the Markdown writer.
//!
//! These tests write into `CARGO_TARGET_TMPDIR`, which Cargo creates inside `target/`, so they
//! never touch the user's Documents folder.

use std::path::PathBuf;

use chrono::{Local, NaiveDate, TimeZone};
use depth::config::Language;
use depth::writer::{SessionInfo, TranscriptWriter, newest_transcript};

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

fn at(hour: u32, minute: u32, second: u32) -> chrono::DateTime<Local> {
    Local
        .with_ymd_and_hms(2025, 6, 1, hour, minute, second)
        .single()
        .expect("a valid local time")
}

fn session() -> SessionInfo {
    SessionInfo {
        language: Language::En,
    }
}

#[test]
fn first_append_creates_a_file_with_day_and_session_headers() {
    let dir = scratch("writer_first");
    let mut writer = TranscriptWriter::new(&dir);

    let path = writer.begin_session(at(14, 2, 11), session()).unwrap();
    writer
        .append(at(14, 2, 11), "And so, my fellow Americans.")
        .unwrap();

    assert!(path.exists());
    assert!(path.ends_with("transcript-2025-06-01.md"));
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("# Transcript — 2025-06-01"), "got: {body}");
    assert!(
        body.contains("## Session 1 — 14:02 — English"),
        "one header per listening instance, got: {body}"
    );
    assert!(
        !body.contains("Whistle"),
        "engine names stay out of the transcript, got: {body}"
    );
    assert!(
        body.contains("**14:02:11**  And so, my fellow Americans."),
        "got: {body}"
    );
    // (B) the same words are also available without timestamps.
    assert!(body.contains("## Full text"), "got: {body}");
    let full = body.split("## Full text").nth(1).unwrap();
    assert!(full.contains("And so, my fellow Americans."), "got: {body}");
    assert!(
        !full.contains("**14:02:11**"),
        "no timestamps in Full text, got: {body}"
    );
}

#[test]
fn utterances_append_in_order_across_calls() {
    let dir = scratch("writer_append");
    let mut writer = TranscriptWriter::new(&dir);
    writer.begin_session(at(9, 0, 0), session()).unwrap();
    writer.append(at(9, 0, 1), "first line").unwrap();
    writer.append(at(9, 0, 2), "second line").unwrap();

    let body = std::fs::read_to_string(writer.current_path().unwrap()).unwrap();
    let first = body.find("first line").expect("first line present");
    let second = body.find("second line").expect("second line present");
    assert!(
        first < second,
        "utterances must stay in chronological order"
    );
    assert_eq!(
        body.matches("## Session").count(),
        1,
        "one session header only"
    );
    // The combined section joins the session without per-line times.
    let full = body
        .split("## Full text")
        .nth(1)
        .expect("Full text present");
    assert!(full.contains("first line second line"), "got: {body}");
}

#[test]
fn blank_and_whitespace_text_is_skipped() {
    let dir = scratch("writer_blank");
    let mut writer = TranscriptWriter::new(&dir);
    writer.begin_session(at(10, 0, 0), session()).unwrap();
    writer.append(at(10, 0, 1), "   \n\t ").unwrap();

    let body = std::fs::read_to_string(writer.current_path().unwrap()).unwrap();
    assert!(
        !body.contains("**10:00:01**"),
        "no empty line should be written"
    );
    assert!(
        !body.contains("## Full text"),
        "no combined section without speech"
    );
}

#[test]
fn multi_line_text_is_collapsed_to_one_markdown_line() {
    let dir = scratch("writer_multiline");
    let mut writer = TranscriptWriter::new(&dir);
    writer.begin_session(at(11, 0, 0), session()).unwrap();
    writer.append(at(11, 0, 1), "hello\n  world").unwrap();

    let body = std::fs::read_to_string(writer.current_path().unwrap()).unwrap();
    assert!(body.contains("**11:00:01**  hello world"));
}

#[test]
fn a_new_day_gets_its_own_file_and_repeats_the_session_header() {
    let dir = scratch("writer_rollover");
    let mut writer = TranscriptWriter::new(&dir);
    writer.begin_session(at(23, 59, 0), session()).unwrap();
    writer.append(at(23, 59, 30), "before midnight").unwrap();

    // Same session, next day: the writer must switch files on its own.
    let day_two = NaiveDate::from_ymd_opt(2025, 6, 2).unwrap();
    let tomorrow = Local
        .with_ymd_and_hms(2025, 6, 2, 0, 0, 5)
        .single()
        .unwrap();
    writer.append(tomorrow, "after midnight").unwrap();

    let first = dir.join("transcript-2025-06-01.md");
    let second = writer.path_for(day_two);
    assert!(first.exists() && second.exists(), "one file per day");
    let body_two = std::fs::read_to_string(&second).unwrap();
    assert!(
        body_two.starts_with("# Transcript — 2025-06-02"),
        "got: {body_two}"
    );
    assert!(body_two.contains("after midnight"));
    assert!(
        body_two.contains("## Session"),
        "the new file needs its own session header"
    );
}

#[test]
fn appending_to_an_existing_file_preserves_earlier_content() {
    let dir = scratch("writer_resume");
    {
        let mut writer = TranscriptWriter::new(&dir);
        writer.begin_session(at(8, 0, 0), session()).unwrap();
        writer.append(at(8, 0, 1), "earlier run").unwrap();
    }
    // A fresh process appending to the same day must not truncate the file.
    {
        let mut writer = TranscriptWriter::new(&dir);
        writer.begin_session(at(9, 30, 0), session()).unwrap();
        writer.append(at(9, 30, 1), "later run").unwrap();
    }
    let body = std::fs::read_to_string(dir.join("transcript-2025-06-01.md")).unwrap();
    assert!(
        body.contains("earlier run"),
        "existing content must survive"
    );
    assert!(body.contains("later run"));
    // Numbering continues per day file, and Full text combines both instances.
    assert!(
        body.contains("## Session 1 — 08:00 — English"),
        "got: {body}"
    );
    assert!(
        body.contains("## Session 2 — 09:30 — English"),
        "got: {body}"
    );
    let full = body
        .split("## Full text")
        .nth(1)
        .expect("Full text present");
    assert!(full.contains("earlier run"), "got: {body}");
    assert!(full.contains("later run"), "got: {body}");
}

#[test]
fn sessions_are_numbered_by_instance_not_by_minute() {
    let dir = scratch("writer_instances");
    let mut writer = TranscriptWriter::new(&dir);
    // Two instances started inside the same clock minute stay distinct.
    writer.begin_session(at(14, 2, 5), session()).unwrap();
    writer.append(at(14, 2, 6), "first instance").unwrap();
    writer.begin_session(at(14, 2, 40), session()).unwrap();
    writer.append(at(14, 2, 50), "second instance").unwrap();
    // One long instance spanning minutes keeps a single header.
    writer.begin_session(at(15, 0, 0), session()).unwrap();
    writer.append(at(15, 1, 10), "minute one").unwrap();
    writer
        .append(at(15, 33, 20), "minute thirty-three")
        .unwrap();

    let body = std::fs::read_to_string(writer.current_path().unwrap()).unwrap();
    assert!(
        body.contains("## Session 1 — 14:02 — English"),
        "got: {body}"
    );
    assert!(
        body.contains("## Session 2 — 14:02 — English"),
        "got: {body}"
    );
    assert!(
        body.contains("## Session 3 — 15:00 — English"),
        "got: {body}"
    );
    assert_eq!(body.matches("## Session").count(), 3, "got: {body}");
    // Full text: one paragraph per instance, no times.
    let full = body
        .split("## Full text")
        .nth(1)
        .expect("Full text present");
    assert!(full.contains("first instance"), "got: {body}");
    assert!(
        full.contains("minute one minute thirty-three"),
        "got: {body}"
    );
    assert!(
        !full.contains("**15:"),
        "no timestamps in Full text, got: {body}"
    );
}

#[test]
fn newest_transcript_prefers_the_latest_day() {
    let dir = scratch("writer_newest");
    std::fs::write(dir.join("transcript-2025-05-31.md"), "# older").unwrap();
    std::fs::write(dir.join("transcript-2025-06-01.md"), "# newer").unwrap();
    std::fs::write(dir.join("notes.md"), "not a transcript").unwrap();

    let newest = newest_transcript(&dir).expect("a transcript file");
    assert!(
        newest.ends_with("transcript-2025-06-01.md"),
        "got {}",
        newest.display()
    );
}

#[test]
fn newest_transcript_is_none_without_transcripts() {
    let dir = scratch("writer_newest_empty");
    assert!(newest_transcript(&dir).is_none());
}

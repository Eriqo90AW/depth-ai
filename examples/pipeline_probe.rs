//! Exercise the real capture/dual-worker pipeline against playing desktop speech.
use depth::{
    config::{Config, Language},
    logging::Logger,
    pipeline,
    recording::PipelineEvent,
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
fn main() -> anyhow::Result<()> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let config = Config {
        base_dir: PathBuf::from(".scratch/live-verification"),
        language: if std::env::args().nth(2).as_deref() == Some("en") {
            Language::En
        } else {
            Language::Id
        },
        start_listening: true,
        ..Default::default()
    };
    let pipeline = pipeline::start(
        config.clone(),
        Arc::new(Logger::new(config.base_dir.join("depth.log"))),
    )?;
    let start = Instant::now();
    let events = pipeline.events();
    let mut first = true;
    let mut last = String::new();
    while start.elapsed() < Duration::from_secs(seconds) {
        let live = pipeline.live_state();
        let draft = live.draft_text();
        if draft != last && !draft.is_empty() {
            if first {
                println!(
                    "FIRST_DRAFT {:.3}s from Start",
                    start.elapsed().as_secs_f64()
                );
                first = false;
            }
            println!(
                "DRAFT {:.3}s queue {}: {draft}",
                start.elapsed().as_secs_f64(),
                live.queued
            );
        }
        last = draft;
        for event in events.try_iter() {
            if let PipelineEvent::Segment(s) = event {
                println!(
                    "FINAL {:.3}s [{}..{}] {}",
                    start.elapsed().as_secs_f64(),
                    s.start_ms,
                    s.end_ms,
                    s.text
                );
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    pipeline.set_paused(true);
    println!("STOP {:.3}s", start.elapsed().as_secs_f64());
    while pipeline.recordings().last().is_some_and(|r| {
        matches!(
            r.status,
            depth::recording::RecordingStatus::Recording
                | depth::recording::RecordingStatus::Processing
        )
    }) {
        std::thread::sleep(Duration::from_millis(100));
    }
    for event in events.try_iter() {
        println!("DRAIN {event:?}");
    }
    let id = pipeline.live_state().recording_id;
    let recording = pipeline.recording(&id).expect("recording is retained");
    assert!(
        recording
            .segments
            .windows(2)
            .all(|s| s[0].end_ms <= s[1].start_ms && s[0].sequence + 1 == s[1].sequence)
    );
    assert!(pipeline.live_state().drafts.is_empty());
    assert_eq!(
        std::fs::read_to_string(config.transcripts_dir().join(format!("{id}.md")))?,
        recording.markdown()
    );
    println!(
        "VERIFIED {} final segments, no drafts in saved Markdown, no duplicate chunk sequence",
        recording.segments.len()
    );
    println!("DONE {:.3}s", start.elapsed().as_secs_f64());
    pipeline.shutdown();
    Ok(())
}

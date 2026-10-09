//! Bounded audio windows and an independent local speaker worker.
use super::{Change, Speaker, SpeakerTurn, match_profile};
use crate::{config::Config, logging::Logger};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use std::{collections::BTreeMap, sync::Arc, thread::JoinHandle, time::Instant};
const RATE: u64 = 16000;
const WINDOW: usize = 160000;
const STRIDE: usize = 80000;
#[derive(Debug)]
pub struct AudioWindow {
    pub start_ms: u64,
    pub samples: Vec<f32>,
}
pub struct WindowInput {
    queue: Sender<AudioWindow>,
    buffer: Vec<f32>,
    base_sample: Option<u64>,
    last_submitted_end: u64,
    pub skipped: usize,
}
impl WindowInput {
    pub fn new(queue: Sender<AudioWindow>) -> Self {
        Self {
            queue,
            buffer: Vec::with_capacity(WINDOW),
            base_sample: None,
            last_submitted_end: 0,
            skipped: 0,
        }
    }
    pub fn push(&mut self, samples: &[f32], start_ms: u64) {
        let start = start_ms * RATE / 1000;
        let base = *self.base_sample.get_or_insert(start);
        let expected = base + self.buffer.len() as u64;
        if start > expected + RATE / 100 {
            let gap = start - expected;
            if gap > WINDOW as u64 {
                self.submit(false);
                self.buffer.clear();
                self.base_sample = Some(start);
            } else {
                self.append(&vec![0.0; gap as usize]);
            }
        }
        // Capture timestamps round to milliseconds. Do not duplicate packets on reconnect.
        let expected = self.base_sample.unwrap() + self.buffer.len() as u64;
        let skip = expected.saturating_sub(start).min(samples.len() as u64) as usize;
        self.append(&samples[skip..]);
    }
    fn append(&mut self, mut audio: &[f32]) {
        while !audio.is_empty() {
            let n = (WINDOW - self.buffer.len()).min(audio.len());
            self.buffer.extend_from_slice(&audio[..n]);
            audio = &audio[n..];
            if self.buffer.len() == WINDOW {
                self.submit(false);
                self.buffer.drain(..STRIDE);
                *self.base_sample.as_mut().unwrap() += STRIDE as u64;
            }
        }
    }
    fn submit(&mut self, blocking: bool) {
        let Some(base) = self.base_sample else { return };
        let end = base + self.buffer.len() as u64;
        if self.buffer.is_empty() || end <= self.last_submitted_end {
            return;
        }
        let window = AudioWindow {
            start_ms: base * 1000 / RATE,
            samples: self.buffer.clone(),
        };
        let sent = if blocking {
            self.queue.send(window).is_ok()
        } else {
            self.queue.try_send(window).is_ok()
        };
        if sent {
            self.last_submitted_end = end;
        } else {
            self.skipped += 1;
        }
    }
    /// The final tail is blocking: Stop drains it even when the worker is behind.
    pub fn finish(mut self) -> usize {
        self.submit(true);
        self.skipped
    }
}
#[derive(Debug, Clone)]
pub struct LocalTurn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub cluster: i32,
}
#[derive(Debug, Default)]
pub struct Detection {
    pub turns: Vec<LocalTurn>,
    pub embeddings: BTreeMap<i32, Vec<f32>>,
}
pub trait Diarizer: Send {
    fn analyze(&mut self, samples: &[f32]) -> Result<Detection>;
}
#[derive(Debug)]
pub enum Update {
    Change(Change),
    Notice(String),
}
pub struct Session {
    pub input: Option<WindowInput>,
    pub updates: Receiver<Update>,
    pub thread: JoinHandle<()>,
}
pub fn start(
    config: Config,
    profiles: Vec<Speaker>,
    offset_ms: u64,
    logger: Arc<Logger>,
) -> Session {
    let (audio_tx, audio_rx) = bounded(2);
    let (out_tx, updates) = unbounded();
    let thread = std::thread::spawn(move || {
        #[cfg(feature = "speakers")]
        let backend =
            super::backend::LocalDiarizer::load(&config).map(|b| Box::new(b) as Box<dyn Diarizer>);
        #[cfg(not(feature = "speakers"))]
        let backend: Result<Box<dyn Diarizer>> = Err(anyhow::anyhow!(
            "This build does not include speaker detection"
        ));
        let _ = &config;
        match backend {
            Ok(backend) => run(backend, profiles, offset_ms, audio_rx, out_tx, logger),
            Err(e) => {
                let _ = out_tx.send(Update::Notice(format!(
                    "Speaker detection unavailable: {e:#}. Transcription is preserved."
                )));
                let _ = out_tx.send(Update::Change(Change::Finished));
            }
        }
    });
    Session {
        input: Some(WindowInput::new(audio_tx)),
        updates,
        thread,
    }
}
pub fn run(
    mut backend: Box<dyn Diarizer>,
    mut profiles: Vec<Speaker>,
    offset_ms: u64,
    audio: Receiver<AudioWindow>,
    out: Sender<Update>,
    logger: Arc<Logger>,
) {
    let mut uncertain: Vec<(SpeakerTurn, Vec<f32>)> = vec![];
    while let Ok(window) = audio.recv() {
        let start = Instant::now();
        let detection = match backend.analyze(&window.samples) {
            Ok(d) => d,
            Err(e) => {
                let _ = out.send(Update::Notice(format!(
                    "Speaker detection failed: {e:#}. Transcription is preserved."
                )));
                continue;
            }
        };
        logger.info(format!(
            "speaker window {} ms: {:.3}s inference, {} turns, {} queued",
            window.start_ms,
            start.elapsed().as_secs_f64(),
            detection.turns.len(),
            audio.len()
        ));
        if start.elapsed().as_secs_f32() > 5.0 {
            let _ = out.send(Update::Notice(
                "Speaker detection is behind playback; labels may be delayed.".into(),
            ));
        }
        let mut mapping = BTreeMap::new();
        // First appearance, rather than backend cluster numbers, determines stable IDs.
        for turn in &detection.turns {
            if mapping.contains_key(&turn.cluster) {
                continue;
            }
            let id = detection
                .embeddings
                .get(&turn.cluster)
                .and_then(|e| match_profile(&mut profiles, e, true));
            // Different clusters in this window cannot claim the same identity.
            let id = id.filter(|id| {
                !mapping
                    .values()
                    .any(|v: &Option<String>| v.as_ref() == Some(id))
            });
            mapping.insert(turn.cluster, id);
        }
        for profile in &profiles {
            let _ = out.send(Update::Change(Change::Profile(profile.clone())));
        }
        let base = offset_ms + window.start_ms;
        let end = base + window.samples.len() as u64 * 1000 / RATE;
        // Track only unresolved intervals that remain after this overlapping window.
        // Clip them exactly as the recording renderer clips older assignments.
        let mut retained = vec![];
        for (t, e) in uncertain.drain(..) {
            if t.end_ms <= base || t.start_ms >= end {
                retained.push((t, e));
            } else {
                if t.start_ms < base {
                    let mut clipped = t.clone();
                    clipped.end_ms = base;
                    retained.push((clipped, e.clone()));
                }
                if t.end_ms > end {
                    let mut clipped = t;
                    clipped.start_ms = end;
                    retained.push((clipped, e));
                }
            }
        }
        uncertain = retained;
        let mut turns = vec![];
        for t in detection.turns {
            let turn = SpeakerTurn {
                start_ms: base + t.start_ms,
                end_ms: (base + t.end_ms).min(end),
                speaker_id: mapping.get(&t.cluster).cloned().flatten(),
            };
            if turn.speaker_id.is_none() {
                if let Some(e) = detection.embeddings.get(&t.cluster) {
                    uncertain.push((turn.clone(), e.clone()));
                }
            }
            turns.push(turn);
        }
        let _ = out.send(Update::Change(Change::Window {
            start_ms: base,
            end_ms: end,
            turns,
        }));
    }
    let resolved = uncertain
        .into_iter()
        .filter_map(|(mut t, e)| {
            t.speaker_id = match_profile(&mut profiles, &e, false);
            t.speaker_id.as_ref()?;
            Some(t)
        })
        .collect();
    let _ = out.send(Update::Change(Change::Refined(resolved)));
    let _ = out.send(Update::Change(Change::Finished));
}

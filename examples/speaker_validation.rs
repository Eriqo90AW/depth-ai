//! Model smoke checks with two existing local, different-speaker WAV fixtures.
use depth::{
    capture,
    config::{Config, Language},
    logging::Logger,
    recording::Recording,
    speakers::{
        self,
        backend::LocalDiarizer,
        worker::{self, Diarizer, WindowInput},
    },
};
use std::{path::Path, sync::Arc};
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 2,
        "usage: speaker_validation <voice-a.wav> <voice-b.wav>"
    );
    let a = capture::read_wav(Path::new(&args[0]))?;
    let b = capture::read_wav(Path::new(&args[1]))?;
    anyhow::ensure!(
        a.len() >= 96000 && b.len() >= 96000,
        "Need six seconds of each voice"
    );
    let mut backend = LocalDiarizer::load(&Config::default())?;
    let silent = backend.analyze(&vec![0.; 160000])?;
    anyhow::ensure!(
        silent.turns.is_empty() && silent.embeddings.is_empty(),
        "Silence detected as a voice"
    );
    let music: Vec<_> = (0..160000)
        .map(|i| {
            let t = i as f32 / 16000.;
            [220., 277.18, 329.63]
                .iter()
                .map(|f| (t * f * std::f32::consts::TAU).sin() * 0.08)
                .sum::<f32>()
        })
        .collect();
    let tone = backend.analyze(&music)?;
    println!(
        "SILENCE voices=0; SYNTHETIC MUSIC turns={} voice_profiles={}",
        tone.turns.len(),
        tone.embeddings.len()
    );
    let mut pcm = vec![];
    pcm.extend_from_slice(&a[..96000]);
    pcm.extend_from_slice(&b[..96000]);
    pcm.extend_from_slice(&a[..96000]);
    let (tx, rx) = crossbeam_channel::unbounded();
    let (out, updates) = crossbeam_channel::unbounded();
    let thread = std::thread::spawn(move || {
        worker::run(
            Box::new(backend),
            vec![],
            0,
            rx,
            out,
            Arc::new(Logger::disabled()),
        )
    });
    let mut input = WindowInput::new(tx);
    for (i, chunk) in pcm.chunks(1600).enumerate() {
        input.push(chunk, i as u64 * 100);
    }
    input.finish();
    let mut r = Recording::new(Language::En, false);
    for update in updates {
        if let worker::Update::Change(change) = update {
            speakers::apply(&mut r, &change);
        }
    }
    thread.join().unwrap();
    let dominant = |start: u64, end: u64| {
        let mut coverage = std::collections::BTreeMap::new();
        for t in &r.speaker_turns {
            if let Some(id) = &t.speaker_id {
                *coverage.entry(id.clone()).or_insert(0u64) +=
                    t.end_ms.min(end).saturating_sub(t.start_ms.max(start));
            }
        }
        coverage
            .into_iter()
            .max_by_key(|(_, ms)| *ms)
            .filter(|(_, ms)| *ms > 1000)
            .map(|(id, _)| id)
    };
    let first = dominant(0, 6000);
    let second = dominant(6000, 12000);
    let returning = dominant(12000, 18000);
    println!(
        "RETURNING VOICES first={first:?} second={second:?} returning={returning:?}; turns={:?}",
        r.speaker_turns
    );
    anyhow::ensure!(
        first.is_some() && second.is_some() && first != second && first == returning,
        "Real voice identity stability check failed"
    );
    println!("PASS: two voices in overlapping windows; returning voice kept its ID");
    Ok(())
}

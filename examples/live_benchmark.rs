//! Two-minute local engine benchmark using a supplied 16 kHz mono WAV.
//! This measures CPU latency/backlog, not recognition accuracy or browser routing.
use crossbeam_channel::{bounded, unbounded};
use depth::{
    capture,
    config::Config,
    engine::{AsrEngine, whisper_cli::WhisperCliEngine},
    logging::Logger,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wav = args
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: live_benchmark <16k-mono.wav> [seconds]"))?;
    let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(120);
    let pcm = capture::read_wav(&wav)?;
    anyhow::ensure!(!pcm.is_empty(), "empty WAV");
    let config = Config {
        base_dir: PathBuf::from("target/live-benchmark"),
        whisper_threads: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
        ..Default::default()
    };
    let logger = Arc::new(Logger::disabled());
    let mut final_engine = WhisperCliEngine::new(&config, &logger)?;
    let mut draft_engine = WhisperCliEngine::new_draft(&config, &logger)?;
    // Warm both engines, then measure simultaneous work.
    let warm: Vec<f32> = pcm.iter().copied().cycle().take(32000).collect();
    draft_engine.transcribe(&warm)?;
    final_engine.transcribe(&warm)?;
    let started = Instant::now();
    let (final_tx, final_rx) = unbounded::<(u64, Vec<f32>)>();
    let (wake_tx, wake_rx) = bounded(1);
    let slot = Arc::new(Mutex::new(None::<(u64, Vec<f32>)>));
    let draft_slot = slot.clone();
    let final_worker = std::thread::spawn(move || -> anyhow::Result<()> {
        while let Ok((chunk, audio)) = final_rx.recv() {
            let t = Instant::now();
            final_engine.transcribe(&audio)?;
            println!(
                "final chunk {chunk}: {:.3}s inference, queue {}",
                t.elapsed().as_secs_f64(),
                final_rx.len()
            );
        }
        Ok(())
    });
    let draft_worker = std::thread::spawn(move || -> anyhow::Result<()> {
        let mut first = true;
        while wake_rx.recv().is_ok() {
            let Some((at, audio)) = draft_slot.lock().unwrap().take() else {
                continue;
            };
            let t = Instant::now();
            let mut saw_text = false;
            draft_engine.transcribe_with_updates(&audio, &mut |text| {
                if first && !text.trim().is_empty() {
                    println!(
                        "FIRST_DRAFT {:.3}s since speech start",
                        started.elapsed().as_secs_f64()
                    );
                    first = false;
                }
                saw_text |= !text.trim().is_empty();
            })?;
            println!(
                "draft at {at}s: {:.3}s inference, text {saw_text}",
                t.elapsed().as_secs_f64()
            );
        }
        Ok(())
    });
    for at in (2..=seconds).step_by(2) {
        std::thread::sleep(Duration::from_secs(at).saturating_sub(started.elapsed()));
        let len = if at % 6 == 0 { 6 } else { at % 6 };
        let offset = ((at - len) * 16000) as usize;
        let audio: Vec<f32> = pcm
            .iter()
            .copied()
            .cycle()
            .skip(offset % pcm.len())
            .take(len as usize * 16000)
            .collect();
        *slot.lock().unwrap() = Some((at, audio.clone()));
        let _ = wake_tx.try_send(());
        if at % 6 == 0 {
            final_tx.send((at / 6, audio))?;
        }
    }
    drop(final_tx);
    drop(wake_tx);
    final_worker.join().unwrap()?;
    draft_worker.join().unwrap()?;
    println!(
        "DONE {:.3}s elapsed for {seconds}s playback",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

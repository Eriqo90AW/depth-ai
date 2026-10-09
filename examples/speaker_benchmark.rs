//! Local regression benchmark: speakers alongside final ASR and Indonesian drafts.
//! Usage: speaker_benchmark <en|id> <16k-mono.wav> [seconds]
//! Samples this process and its direct ASR children every 100 ms.
use depth::{
    capture,
    config::{Config, Language, Processing},
    engine::{self, AsrEngine},
    logging::Logger,
    speakers::{
        Change,
        backend::LocalDiarizer,
        worker::{self, Detection, Diarizer, WindowInput},
    },
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};
use windows::Win32::System::{
    ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
    Threading::{GetCurrentProcess, GetProcessTimes},
};
fn process_stats(process: HANDLE) -> (f64, usize) {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let mut memory = PROCESS_MEMORY_COUNTERS::default();
    memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    unsafe {
        let _ = GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        let cb = memory.cb;
        let _ = GetProcessMemoryInfo(process, &mut memory, cb);
    }
    let seconds = |t: FILETIME| {
        ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 10_000_000.0
    };
    (seconds(kernel) + seconds(user), memory.WorkingSetSize)
}
fn monitor(done: Arc<AtomicBool>) -> std::thread::JoinHandle<(f64, usize)> {
    std::thread::spawn(move || {
        let mut children = std::collections::BTreeMap::new();
        let mut peak = 0;
        loop {
            unsafe {
                if let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
                    let mut entry = PROCESSENTRY32W::default();
                    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
                    if Process32FirstW(snapshot, &mut entry).is_ok() {
                        loop {
                            if entry.th32ParentProcessID == std::process::id()
                                && !children.contains_key(&entry.th32ProcessID)
                            {
                                if let Ok(handle) = OpenProcess(
                                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                                    false,
                                    entry.th32ProcessID,
                                ) {
                                    children.insert(entry.th32ProcessID, handle);
                                }
                            }
                            if Process32NextW(snapshot, &mut entry).is_err() {
                                break;
                            }
                        }
                    }
                    let _ = CloseHandle(snapshot);
                }
            }
            let mut total = process_stats(unsafe { GetCurrentProcess() });
            for &handle in children.values() {
                let child = process_stats(handle);
                total.0 += child.0;
                total.1 += child.1;
            }
            peak = peak.max(total.1);
            if done.load(Ordering::Acquire) {
                for handle in children.into_values() {
                    let _ = unsafe { CloseHandle(handle) };
                }
                return (total.0, peak);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    })
}
struct Measured {
    inner: LocalDiarizer,
    times: Arc<Mutex<Vec<f64>>>,
}
impl Diarizer for Measured {
    fn analyze(&mut self, audio: &[f32]) -> anyhow::Result<Detection> {
        let start = Instant::now();
        let r = self.inner.analyze(audio);
        self.times
            .lock()
            .unwrap()
            .push(start.elapsed().as_secs_f64());
        r
    }
}
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 2,
        "usage: speaker_benchmark <en|id> <16k-mono.wav> [seconds] [cpu-stock|cpu-id|cuda-turbo]"
    );
    let language = match args[0].as_str() {
        "en" => Language::En,
        "id" => Language::Id,
        _ => anyhow::bail!("Choose en or id"),
    };
    let source = capture::read_wav(&PathBuf::from(&args[1]))?;
    anyhow::ensure!(!source.is_empty(), "Empty audio");
    let seconds: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let pcm: Vec<f32> = source
        .iter()
        .copied()
        .cycle()
        .take(seconds * 16000)
        .collect();
    let profile = args.get(3).map(String::as_str).unwrap_or("default");
    let config = Config {
        language,
        detect_speakers: true,
        word_timestamps: true,
        base_dir: PathBuf::from("target/speaker-benchmark")
            .join(profile)
            .join(language.code()),
        indonesian_processing: if profile.starts_with("cpu") {
            Processing::Cpu
        } else {
            Processing::Auto
        },
        indonesian_model: match profile {
            "cpu-stock" => "small",
            "cpu-id" => "small-id",
            "cuda-turbo" => "turbo",
            _ => "recommended",
        }
        .parse()?,
        ..Default::default()
    };
    let logger = Arc::new(Logger::disabled());
    let backend = LocalDiarizer::load(&config)?;
    let mut final_engine = engine::load(language, &config, &logger)?;
    let mut draft = if language == Language::Id {
        Some(engine::whisper_cli::WhisperCliEngine::new_draft(
            &config, &logger,
        )?)
    } else {
        None
    };
    let times = Arc::new(Mutex::new(vec![]));
    let measured = Measured {
        inner: backend,
        times: times.clone(),
    };
    let (audio_tx, audio_rx) = crossbeam_channel::bounded(2);
    let (out_tx, out_rx) = crossbeam_channel::unbounded();
    let start = Instant::now();
    let collector = std::thread::spawn(move || {
        let mut profiles = std::collections::BTreeSet::new();
        let mut notices = vec![];
        let mut windows = 0;
        let mut first = 0.;
        let mut latency = 0.0f64;
        for update in out_rx {
            match update {
                worker::Update::Change(Change::Profile(s)) => {
                    profiles.insert(s.id);
                }
                worker::Update::Change(Change::Window {
                    start_ms,
                    end_ms,
                    turns,
                }) => {
                    let elapsed = start.elapsed().as_secs_f64();
                    if windows == 0 {
                        first = elapsed;
                    }
                    windows += 1;
                    let delay = (elapsed - end_ms as f64 / 1000.).max(0.);
                    latency = latency.max(delay);
                    println!("WINDOW {start_ms} delay={delay:.3}s: {turns:?}");
                }
                worker::Update::Notice(n) => notices.push(n),
                _ => {}
            }
        }
        (profiles.len(), windows, notices, first, latency)
    });
    let worker_logger = logger.clone();
    let speaker_worker = std::thread::spawn(move || {
        worker::run(
            Box::new(measured),
            vec![],
            0,
            audio_rx,
            out_tx,
            worker_logger,
        )
    });
    let (final_tx, final_rx) = crossbeam_channel::bounded::<Vec<f32>>(8);
    let (draft_tx, draft_rx) = crossbeam_channel::bounded::<Vec<f32>>(8);
    let asr_worker = std::thread::spawn(move || -> anyhow::Result<(usize, f64, f64, bool)> {
        let mut total_words = 0;
        let mut inference = 0.0;
        let mut audio = 0.0;
        let mut cuda_used = true;
        for chunk in final_rx {
            let start = Instant::now();
            let r = final_engine.transcribe(&chunk)?;
            inference += start.elapsed().as_secs_f64();
            audio += chunk.len() as f64 / 16000.0;
            cuda_used &= final_engine.status().contains("NVIDIA");
            for notice in final_engine.take_notices() {
                println!("ASR NOTICE {notice}");
            }
            total_words += r.words.len();
            println!(
                "ASR {:.3}s {} timed tokens: {}",
                start.elapsed().as_secs_f64(),
                r.words.len(),
                r.text
            );
        }
        Ok((total_words, inference, audio, cuda_used))
    });
    let draft_worker = std::thread::spawn(move || -> anyhow::Result<()> {
        if let Some(engine) = draft.as_mut() {
            for chunk in draft_rx {
                engine.transcribe(&chunk)?;
            }
        }
        Ok(())
    });
    let baseline = process_stats(unsafe { GetCurrentProcess() });
    let done = Arc::new(AtomicBool::new(false));
    let monitor = monitor(done.clone());
    let mut input = WindowInput::new(audio_tx);
    let mut submitted = 0;
    let mut dropped = 0;
    let mut peak_queue = 0;
    for (i, chunk) in pcm.chunks(1600).enumerate() {
        input.push(chunk, i as u64 * 100);
        let end = ((i + 1) * 1600).min(pcm.len());
        if end % (2 * 16000) == 0 && language == Language::Id {
            let len = if end % (6 * 16000) == 0 {
                6 * 16000
            } else {
                end % (6 * 16000)
            };
            let _ = draft_tx.try_send(pcm[end - len..end].to_vec());
        }
        if end % (6 * 16000) == 0 {
            if final_tx.try_send(pcm[submitted..end].to_vec()).is_err() {
                dropped += 1;
            }
            peak_queue = peak_queue.max(final_tx.len());
            submitted = end;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if submitted < pcm.len() {
        final_tx.send(pcm[submitted..].to_vec())?;
    }
    drop(final_tx);
    drop(draft_tx);
    let skipped = input.finish();
    speaker_worker
        .join()
        .map_err(|_| anyhow::anyhow!("speaker worker panicked"))?;
    let (timed_words, inference, audio, cuda_used) = asr_worker
        .join()
        .map_err(|_| anyhow::anyhow!("ASR worker panicked"))??;
    draft_worker
        .join()
        .map_err(|_| anyhow::anyhow!("draft worker panicked"))??;
    let (speakers, windows, notices, first_label, added_latency) = collector.join().unwrap();
    done.store(true, Ordering::Release);
    let finish = monitor.join().unwrap();
    let times = times.lock().unwrap();
    let max = times.iter().copied().fold(0.0, f64::max);
    let mean = times.iter().sum::<f64>() / times.len().max(1) as f64;
    let wall = start.elapsed().as_secs_f64();
    println!(
        "RESULT language={} audio={}s wall={wall:.3}s windows={windows} speakers={} max={max:.3}s mean={mean:.3}s skipped={skipped} cpu_seconds={:.3} average_cores={:.3} peak_combined_working_set_mib={:.1} first_label={first_label:.3}s max_window_label_delay={added_latency:.3}s timed_tokens={timed_words} notices={notices:?}",
        language.code(),
        seconds,
        speakers,
        finish.0 - baseline.0,
        (finish.0 - baseline.0) / wall,
        finish.1 as f64 / 1048576.0
    );
    println!(
        "ASR_RESULT profile={profile} inference={inference:.3}s audio={audio:.3}s rtf={:.3} dropped={dropped} peak_queue={peak_queue} cuda_used={cuda_used}",
        inference / audio.max(0.001)
    );
    if profile == "cuda-turbo" {
        let gpu = depth::gpu::discover();
        if let Some(gpu) = gpu.device {
            depth::models::save_qualification(
                &config,
                &gpu,
                cuda_used && inference < audio && dropped == 0,
                inference,
                audio,
            )?;
        }
    }
    anyhow::ensure!(timed_words > 0, "ASR produced no usable word timing");
    anyhow::ensure!(
        max < 5.0 && skipped == 0,
        "Speaker analysis did not keep pace with five-second updates"
    );
    Ok(())
}

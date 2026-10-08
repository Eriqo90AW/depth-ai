//! A control thread owns recording lifetimes. Capture closes its queue before inference drains.
use crate::{
    capture,
    config::{Config, Language},
    engine::{self, AsrEngine},
    live::LiveState,
    logging::Logger,
    recording::{PipelineEvent, Recording, RecordingStatus, RecordingStore, TranscriptSegment},
    vad::{Segment, Segmenter, SegmenterConfig},
};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, bounded, select, unbounded};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Starting,
    Listening,
    Paused,
    Transcribing(usize),
    Error(String),
}
impl Status {
    pub fn label(&self) -> String {
        match self {
            Self::Starting => "Starting".into(),
            Self::Listening => "Recording".into(),
            Self::Paused => "Ready to record".into(),
            Self::Transcribing(_) => "Finishing transcription".into(),
            Self::Error(e) => format!("Error: {e}"),
        }
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
}
#[derive(Debug, Clone)]
pub enum Control {
    Resume,
    Continue(Box<Recording>),
    Delete(Box<Recording>),
    Pause,
    Toggle,
    SetLanguage(Language),
    SetSaveEnabled(bool),
    Configure(Box<Config>),
    Shutdown,
}
pub struct PipelineHandle {
    status: Arc<Mutex<Status>>,
    paused: Arc<AtomicBool>,
    control: Sender<Control>,
    records: Arc<Mutex<Vec<Arc<Mutex<Recording>>>>>,
    last_text: Arc<Mutex<String>>,
    live: Arc<Mutex<LiveState>>,
    events: Receiver<PipelineEvent>,
    save_enabled: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}
impl PipelineHandle {
    pub fn live_state(&self) -> LiveState {
        lock(&self.live).clone()
    }
    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }
    pub fn set_status(&self, s: Status) {
        *lock(&self.status) = s;
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }
    pub fn send(&self, c: Control) {
        let _ = self.control.send(c);
    }
    pub fn control_sender(&self) -> Sender<Control> {
        self.control.clone()
    }
    pub fn status_handle(&self) -> Arc<Mutex<Status>> {
        self.status.clone()
    }
    pub fn last_text_handle(&self) -> Arc<Mutex<String>> {
        self.last_text.clone()
    }
    pub fn last_text(&self) -> String {
        lock(&self.last_text).clone()
    }
    pub fn request_stop(&self) {
        self.send(Control::Shutdown);
    }
    pub fn set_paused(&self, p: bool) {
        self.send(if p { Control::Pause } else { Control::Resume });
    }
    pub fn set_language(&self, l: Language) {
        self.send(Control::SetLanguage(l));
    }
    pub fn set_save_enabled(&self, e: bool) {
        self.save_enabled.store(e, Ordering::Release);
        self.send(Control::SetSaveEnabled(e));
    }
    pub fn save_enabled(&self) -> bool {
        self.save_enabled.load(Ordering::Acquire)
    }
    pub fn today_path(&self) -> Option<PathBuf> {
        lock(&self.records).last().and_then(|r| {
            let r = lock(r);
            r.source
                .as_ref()
                .and_then(|s| s.parent())
                .and_then(|p| p.parent())
                .map(|p| p.join(format!("{}.md", r.id)))
        })
    }
    pub fn memory_markdown(&self) -> String {
        lock(&self.records)
            .last()
            .map(|r| lock(r).markdown())
            .unwrap_or_default()
    }
    pub fn export_memory(&self, p: &Path) -> std::io::Result<()> {
        std::fs::write(p, self.memory_markdown())
    }
    pub fn recordings(&self) -> Vec<Recording> {
        lock(&self.records)
            .iter()
            .map(|r| {
                let r = lock(r);
                r.metadata()
            })
            .collect()
    }
    pub fn recording(&self, id: &str) -> Option<Recording> {
        lock(&self.records).iter().find_map(|r| {
            let r = lock(r);
            if r.id == id {
                if r.segments.is_empty()
                    && r.autosave
                    && r.source.is_some()
                    && matches!(
                        r.status,
                        RecordingStatus::Completed | RecordingStatus::Incomplete
                    )
                {
                    crate::recording::load_recording(&r).ok()
                } else {
                    Some(r.clone())
                }
            } else {
                None
            }
        })
    }
    pub fn events(&self) -> Receiver<PipelineEvent> {
        self.events.clone()
    }
    pub fn mark_exported(&self, id: &str) {
        for r in lock(&self.records).iter() {
            let mut r = lock(r);
            if r.id == id {
                r.exported = true;
            }
        }
    }
    pub fn rename(&self, id: &str, title: &str, dir: &Path) -> std::io::Result<()> {
        for r in lock(&self.records).iter() {
            let mut r = lock(r);
            if r.id == id {
                if r.segments.is_empty()
                    && r.autosave
                    && r.source.is_some()
                    && matches!(
                        r.status,
                        RecordingStatus::Completed | RecordingStatus::Incomplete
                    )
                {
                    let mut full = crate::recording::load_recording(&r)?;
                    crate::recording::rename(&mut full, title, dir)?;
                    r.title = full.title;
                    return Ok(());
                }
                return crate::recording::rename(&mut r, title, dir);
            }
        }
        Err(std::io::Error::other("Recording not found"))
    }
    pub fn shutdown(mut self) {
        self.request_stop();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
fn segmenter_config(c: &Config) -> SegmenterConfig {
    SegmenterConfig {
        sample_rate: capture::TARGET_RATE,
        frame_ms: c.frame_ms,
        threshold_db: c.vad_threshold_db,
        speech_start_ms: c.speech_start_ms,
        silence_close_ms: c.silence_close_ms,
        min_segment_ms: c.min_segment_ms,
        max_segment_secs: if c.language == Language::Id {
            c.max_segment_secs
                .min(6.0)
                .max(c.min_segment_ms as f32 / 1000.0 + 0.02)
        } else {
            c.max_segment_secs
        },
        tail_keep_ms: c.tail_keep_ms,
    }
}
pub fn start(config: Config, logger: Arc<Logger>) -> Result<PipelineHandle> {
    config.validate()?;
    let status = Arc::new(Mutex::new(Status::Paused));
    let paused = Arc::new(AtomicBool::new(true));
    let records = Arc::new(Mutex::new(vec![]));
    let last_text = Arc::new(Mutex::new(String::new()));
    let live = Arc::new(Mutex::new(LiveState::default()));
    let save_enabled = Arc::new(AtomicBool::new(config.save_transcript));
    let (tx, rx) = unbounded();
    let (events_tx, events_rx) = unbounded();
    let worker = {
        let status = status.clone();
        let paused = paused.clone();
        let records = records.clone();
        let last_text = last_text.clone();
        let live = live.clone();
        std::thread::Builder::new()
            .name("recording-controller".into())
            .spawn(move || {
                controller(
                    config, rx, status, paused, records, last_text, live, events_tx, logger,
                )
            })?
    };
    Ok(PipelineHandle {
        status,
        paused,
        control: tx,
        records,
        last_text,
        live,
        events: events_rx,
        save_enabled,
        threads: vec![worker],
    })
}
struct Active {
    recording: Arc<Mutex<Recording>>,
    stop: Arc<AtomicBool>,
    started: Instant,
    duration_before_ms: u64,
    finishing: bool,
    thread: JoinHandle<()>,
}
fn prepare_recording(
    config: &Config,
    previous: Option<Recording>,
) -> Result<(Recording, RecordingStore)> {
    let mut store = RecordingStore::new(config.transcripts_dir());
    if let Some(mut recording) = previous {
        anyhow::ensure!(
            recording.can_continue(),
            "This recording cannot be continued"
        );
        if recording.autosave {
            recording = crate::recording::load_recording(&recording)?;
        }
        recording.duration_ms = recording.duration_ms.max(
            recording
                .segments
                .iter()
                .map(|segment| segment.end_ms)
                .max()
                .unwrap_or(0),
        );
        store.resume(&mut recording)?;
        return Ok((recording, store));
    }
    let mut recording = Recording::new(config.language, config.save_transcript);
    if let Err(e) = store.begin(&mut recording) {
        recording.autosave = false;
        recording.warnings.push(format!(
            "Autosave failed: {e}. Export this recording before quitting."
        ));
    }
    Ok((recording, store))
}
fn controller(
    mut config: Config,
    control: Receiver<Control>,
    status: Arc<Mutex<Status>>,
    paused: Arc<AtomicBool>,
    records: Arc<Mutex<Vec<Arc<Mutex<Recording>>>>>,
    last_text: Arc<Mutex<String>>,
    live: Arc<Mutex<LiveState>>,
    events: Sender<PipelineEvent>,
    logger: Arc<Logger>,
) {
    let (done_tx, done_rx) = unbounded::<Option<Box<dyn AsrEngine>>>();
    let mut active: Option<Active> = None;
    let mut shutting_down = false;
    let mut deleted = std::collections::HashSet::new();
    let mut engine = None;
    let mut engine_language = config.language;
    let mut engine_dirty = false;
    let mut idle = Instant::now();
    let initial = config.start_listening;
    let (initial_tx, initial_rx) = unbounded();
    if initial {
        let _ = initial_tx.send(Control::Resume);
    }
    drop(initial_tx);
    loop {
        let message=initial_rx.try_recv().ok().or_else(|| {
            select! {recv(control)->c=>c.ok(), recv(done_rx)->result=>{
                if let Some(a)=active.take() {let _=a.thread.join();}
                engine=result.ok().flatten(); idle=Instant::now(); paused.store(true,Ordering::Release); *lock(&status)=Status::Paused;
                None
            },default(Duration::from_millis(100))=>None}
        });
        if let Some(c) = message {
            match c {
                Control::Delete(requested) => {
                    // Completion status is published before the final save returns.
                    // Keep this recording protected until its worker has been joined.
                    if active.as_ref().is_some_and(|a| lock(&a.recording).id == requested.id) {
                        let _ = events.send(PipelineEvent::Warning {
                            id: requested.id.clone(),
                            message: "Could not delete recording: wait for transcription and saving to finish.".into(),
                        });
                        continue;
                    }
                    let existing = lock(&records)
                        .iter()
                        .find(|r| lock(r).id == requested.id)
                        .cloned();
                    let recording = existing
                        .as_ref()
                        .map(|r| lock(r).metadata())
                        .unwrap_or(*requested);
                    let id = recording.id.clone();
                    match crate::recording::delete(&recording, &config.transcripts_dir()) {
                        Ok(()) => {
                            deleted.insert(id.clone());
                            lock(&records).retain(|r| lock(r).id != id);
                            let _ = events.send(PipelineEvent::Deleted { id });
                        }
                        Err(error) => {
                            let message = format!("Could not delete recording: {error}");
                            logger.error(&message);
                            let _ = events.send(PipelineEvent::Warning { id, message });
                        }
                    }
                }
                Control::Configure(c) => {
                    config = *c;
                    engine_dirty = true;
                }
                Control::SetLanguage(l) => config.language = l,
                Control::SetSaveEnabled(e) => config.save_transcript = e,
                Control::Pause | Control::Shutdown => {
                    if matches!(c, Control::Shutdown) {
                        shutting_down = true;
                    }
                    if let Some(a) = active.as_mut() {
                        finish_capture(a, &status, &paused, &events);
                    }
                }
                Control::Resume | Control::Toggle | Control::Continue(_) => {
                    if let Some(a) = active.as_mut() {
                        if matches!(c, Control::Toggle) && !a.finishing {
                            finish_capture(a, &status, &paused, &events);
                        }
                    } else if !shutting_down {
                        let previous = match c {
                            Control::Continue(recording) => Some(*recording),
                            _ => None,
                        };
                        let id = previous.as_ref().map(|r| r.id.clone()).unwrap_or_default();
                        if deleted.contains(&id) {
                            let _ = events.send(PipelineEvent::Warning {
                                id,
                                message: "This recording was deleted.".into(),
                            });
                            continue;
                        }
                        let (recording, store) = match prepare_recording(&config, previous) {
                            Ok(prepared) => prepared,
                            Err(error) => {
                                let message = format!("Could not continue recording: {error}");
                                logger.error(&message);
                                let _ = events.send(PipelineEvent::Warning { id, message });
                                continue;
                            }
                        };
                        let duration_before_ms = recording.duration_ms;
                        let mut session_config = config.clone();
                        session_config.language = recording.language;
                        let started_recording = recording.metadata();
                        lock(&live).begin(recording.id.clone());
                        logger.info(format!(
                            "recording started: {} language {}",
                            recording.id,
                            session_config.language.label()
                        ));
                        let existing = lock(&records)
                            .iter()
                            .find(|r| lock(r).id == recording.id)
                            .cloned();
                        let recording = if let Some(existing) = existing {
                            *lock(&existing) = recording;
                            existing
                        } else {
                            let recording = Arc::new(Mutex::new(recording));
                            lock(&records).push(recording.clone());
                            recording
                        };
                        *lock(&last_text) = String::new();
                        paused.store(false, Ordering::Release);
                        *lock(&status) = Status::Listening;
                        let _ = events.send(PipelineEvent::Started(started_recording));
                        if engine_dirty || engine_language != session_config.language {
                            engine = None;
                        }
                        engine_language = session_config.language;
                        engine_dirty = false;
                        let stop = Arc::new(AtomicBool::new(false));
                        let started = Instant::now();
                        let thread = {
                            let r = recording.clone();
                            let c = session_config;
                            let s = stop.clone();
                            let ev = events.clone();
                            let log = logger.clone();
                            let done = done_tx.clone();
                            let text = last_text.clone();
                            let live = live.clone();
                            let eng = engine.take();
                            std::thread::spawn(move || {
                                let engine = transcribe_recording(
                                    c,
                                    r,
                                    duration_before_ms,
                                    s,
                                    store,
                                    ev,
                                    log,
                                    text,
                                    live,
                                    eng,
                                );
                                let _ = done.send(engine);
                            })
                        };
                        active = Some(Active {
                            recording,
                            stop,
                            started,
                            duration_before_ms,
                            finishing: false,
                            thread,
                        });
                    }
                }
            }
        }
        if shutting_down && active.is_none() {
            break;
        }
        if active.is_none()
            && config.engine_idle_unload_secs > 0
            && idle.elapsed().as_secs() >= config.engine_idle_unload_secs
        {
            engine = None;
        }
    }
}
fn finish_capture(
    a: &mut Active,
    status: &Mutex<Status>,
    paused: &AtomicBool,
    events: &Sender<PipelineEvent>,
) {
    if a.finishing {
        return;
    }
    a.finishing = true;
    a.stop.store(true, Ordering::Release);
    paused.store(true, Ordering::Release);
    let mut r = lock(&a.recording);
    r.duration_ms = a
        .duration_before_ms
        .saturating_add(a.started.elapsed().as_millis() as u64);
    r.status = RecordingStatus::Processing;
    *lock(status) = Status::Transcribing(0);
    let _ = events.send(PipelineEvent::State {
        id: r.id.clone(),
        status: r.status.clone(),
        duration_ms: r.duration_ms,
    });
}
fn warn(
    r: &Arc<Mutex<Recording>>,
    store: &mut RecordingStore,
    events: &Sender<PipelineEvent>,
    logger: &Logger,
    message: String,
) {
    logger.error(&message);
    let mut r = lock(r);
    if r.warnings.contains(&message) {
        return;
    }
    r.warnings.push(message.clone());
    let ev = PipelineEvent::Warning {
        id: r.id.clone(),
        message,
    };
    let _ = store.append(&ev);
    let _ = events.send(ev);
}
fn transcribe_recording(
    config: Config,
    recording: Arc<Mutex<Recording>>,
    offset_ms: u64,
    stop: Arc<AtomicBool>,
    store: RecordingStore,
    events: Sender<PipelineEvent>,
    logger: Arc<Logger>,
    last_text: Arc<Mutex<String>>,
    live: Arc<Mutex<LiveState>>,
    engine: Option<Box<dyn AsrEngine>>,
) -> Option<Box<dyn AsrEngine>> {
    let capture_live = live.clone();
    transcribe_with_capture(
        config,
        recording,
        offset_ms,
        stop,
        store,
        events,
        logger,
        last_text,
        live,
        engine,
        move |config, stop, logger| capture_recording(config, stop, logger, capture_live),
    )
}
fn capture_recording(
    config: &Config,
    stop: Arc<AtomicBool>,
    logger: Arc<Logger>,
    live: Arc<Mutex<LiveState>>,
) -> (Receiver<Segment>, Receiver<String>, JoinHandle<()>) {
    let (tx, rx) = bounded::<Segment>(config.queue_capacity);
    let (warnings_tx, warnings_rx) = unbounded::<String>();
    let capture_stop = stop.clone();
    let cfg = config.clone();
    let log = logger.clone();
    let capture_thread = std::thread::spawn(move || {
        let segmenter = Arc::new(Mutex::new(Segmenter::new(segmenter_config(&cfg))));
        let gate = segmenter.clone();
        let queue = tx.clone();
        let warning = warnings_tx.clone();
        let preview_slot = Arc::new(Mutex::new(None::<(Segment, u64)>));
        let (preview_wake, preview_rx) = bounded(1);
        let preview_stop = Arc::new(AtomicBool::new(false));
        let preview_thread = spawn_preview(
            &cfg,
            &log,
            live.clone(),
            preview_slot.clone(),
            preview_rx,
            preview_stop.clone(),
        );
        let gate_live = live.clone();
        let gate_log = log.clone();
        let mut last_preview_end = 0u64;
        let mut revision = 0u64;
        let mut was_speech = false;
        let sink = Box::new(move |audio: &[f32], start_ms: u64| {
            let mut segmenter = lock(&gate);
            let segments = segmenter.push_at(audio, start_ms * capture::TARGET_RATE as u64 / 1000);
            let in_speech = segmenter.in_speech();
            if in_speech != was_speech {
                gate_log.info(format!(
                    "speech detection: {}",
                    if in_speech { "speech" } else { "silence" }
                ));
                lock(&gate_live).speech = in_speech;
                was_speech = in_speech;
            }
            for segment in segments {
                gate_log.info(format!(
                    "segment submitted: {:.2}s, start {:.3}s, queue {}",
                    segment.duration_secs(),
                    segment.start_sample as f64 / 16000.0,
                    queue.len()
                ));
                if cfg.language == Language::Id {
                    revision += 1;
                    *lock(&preview_slot) = Some((segment.clone(), revision));
                    let _ = preview_wake.try_send(());
                    last_preview_end = segment.end_sample;
                }
                submit_final(&queue, segment, &gate_live, &warning);
            }
            if cfg.language == Language::Id {
                if let Some(snapshot) = segmenter.snapshot() {
                    if snapshot.samples.len() >= 2 * capture::TARGET_RATE as usize
                        && snapshot.end_sample.saturating_sub(last_preview_end)
                            >= 2 * capture::TARGET_RATE as u64
                    {
                        revision += 1;
                        last_preview_end = snapshot.end_sample;
                        *lock(&preview_slot) = Some((snapshot, revision));
                        let _ = preview_wake.try_send(());
                    }
                }
            }
        });
        let health_live = live.clone();
        let observer = Box::new(move |update| {
            let mut state = lock(&health_live);
            match update {
                capture::CaptureUpdate::Source(device) => state.source = device.name,
                capture::CaptureUpdate::Level { packets, peak_db } => state.level(packets, peak_db),
                capture::CaptureUpdate::Notice(message) => state.capture_notice = message,
            }
        });
        if let Err(e) = capture::run_timed_selected(
            sink,
            &capture_stop,
            &log,
            config_output(&cfg),
            Some(observer),
        ) {
            let message = format!("Missing audio: capture failed: {e:#}");
            lock(&live).capture_error = Some(message.clone());
            let _ = warnings_tx.send(message);
        }
        // Flush is blocking here: Stop must keep the final utterance even when the queue is full.
        if let Some(segment) = lock(&segmenter).flush() {
            let _ = tx.send(segment);
        }
        preview_stop.store(true, Ordering::Release);
        if let Some(thread) = preview_thread {
            let _ = thread.join();
        }
    });

    (rx, warnings_rx, capture_thread)
}

fn config_output(config: &Config) -> Option<&str> {
    config.audio_output_device.as_deref()
}
fn submit_final(
    queue: &Sender<Segment>,
    segment: Segment,
    live: &Mutex<LiveState>,
    warnings: &Sender<String>,
) {
    let chunk = segment.start_sample;
    if queue.try_send(segment).is_err() {
        lock(live).finish_chunk(chunk);
        let _ = warnings.send("Missing audio: transcription queue overflowed. Increase queue capacity or reduce engine load.".into());
    }
    lock(live).queued = queue.len();
}

fn spawn_preview(
    config: &Config,
    logger: &Arc<Logger>,
    live: Arc<Mutex<LiveState>>,
    slot: Arc<Mutex<Option<(Segment, u64)>>>,
    wake: Receiver<()>,
    stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    if config.language != Language::Id {
        return None;
    }
    #[cfg(feature = "whisper-sidecar")]
    {
        let config = config.clone();
        let logger = logger.clone();
        return Some(std::thread::spawn(move || {
            let recording_id = lock(&live).recording_id.clone();
            let mut engine =
                match engine::whisper_cli::WhisperCliEngine::new_draft(&config, &logger) {
                    Ok(engine) => engine,
                    Err(e) => {
                        lock(&live).notice = format!("Live previews unavailable: {e:#}");
                        logger.warn(format!("draft engine unavailable: {e:#}"));
                        return;
                    }
                };
            while !stop.load(Ordering::Acquire) {
                let _ = wake.recv_timeout(Duration::from_millis(100));
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let Some((segment, revision)) = lock(&slot).take() else {
                    continue;
                };
                let chunk = segment.start_sample;
                if lock(&live).chunk_finished(chunk) {
                    continue;
                }
                let mut update = |text: &str| {
                    lock(&live).update_draft_for(&recording_id, chunk, revision, text.to_string())
                };
                if let Err(e) = engine.transcribe_with_updates(&segment.samples, &mut update) {
                    let mut state = lock(&live);
                    state.drafts.remove(&chunk);
                    state.notice = format!("Live previews unavailable: {e:#}");
                    logger.warn(format!("draft inference failed: {e:#}"));
                } else {
                    lock(&live).notice.clear();
                }
            }
        }));
    }
    #[cfg(not(feature = "whisper-sidecar"))]
    {
        let _ = (config, logger, slot, wake, stop);
        lock(&live).notice = "Live previews require the whisper-sidecar build.".into();
        None
    }
}
fn transcribe_with_capture(
    config: Config,
    recording: Arc<Mutex<Recording>>,
    offset_ms: u64,
    stop: Arc<AtomicBool>,
    mut store: RecordingStore,
    events: Sender<PipelineEvent>,
    logger: Arc<Logger>,
    last_text: Arc<Mutex<String>>,
    live: Arc<Mutex<LiveState>>,
    mut engine: Option<Box<dyn AsrEngine>>,
    capture: impl FnOnce(
        &Config,
        Arc<AtomicBool>,
        Arc<Logger>,
    ) -> (Receiver<Segment>, Receiver<String>, JoinHandle<()>),
) -> Option<Box<dyn AsrEngine>> {
    let mut sequence = lock(&recording)
        .segments
        .iter()
        .map(|s| s.sequence)
        .max()
        .map_or(0, |s| s + 1);
    let (rx, warnings_rx, capture_thread) = capture(&config, stop, logger.clone());
    loop {
        let segment = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(segment) => segment,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                for warning in warnings_rx.try_iter() {
                    warn(&recording, &mut store, &events, &logger, warning);
                }
                let result = {
                    let r = lock(&recording);
                    store.render(&r, false)
                };
                if let Err(e) = result {
                    warn(
                        &recording,
                        &mut store,
                        &events,
                        &logger,
                        format!("Markdown save failed: {e}"),
                    );
                }
                continue;
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        for warning in warnings_rx.try_iter() {
            warn(&recording, &mut store, &events, &logger, warning);
        }
        lock(&live).queued = rx.len();
        if engine.is_none() {
            match engine::load(config.language, &config, &logger) {
                Ok(e) => engine = Some(e),
                Err(e) => {
                    lock(&live).finish_chunk(segment.start_sample);
                    warn(
                        &recording,
                        &mut store,
                        &events,
                        &logger,
                        format!("Missing audio: engine could not load: {e:#}"),
                    );
                    continue;
                }
            }
        }
        let Some(asr) = engine.as_mut() else {
            continue;
        };
        match asr.transcribe(&segment.samples) {
            Ok(result) => {
                if !result.text.trim().is_empty() {
                    let s = TranscriptSegment {
                        recording_id: lock(&recording).id.clone(),
                        sequence,
                        start_ms: offset_ms
                            + segment.start_sample * 1000 / capture::TARGET_RATE as u64,
                        end_ms: offset_ms + segment.end_sample * 1000 / capture::TARGET_RATE as u64,
                        text: result.text.trim().into(),
                        clock_time: None,
                    };
                    sequence += 1;
                    *lock(&last_text) = s.text.clone();
                    {
                        let mut r = lock(&recording);
                        if r.preview.is_empty() {
                            r.preview = s.text.chars().take(110).collect();
                        }
                        r.exported = false;
                        r.segments.push(s.clone());
                    }
                    if let Err(e) = store.append(&PipelineEvent::Segment(s.clone())) {
                        warn(
                            &recording,
                            &mut store,
                            &events,
                            &logger,
                            format!("Save failed: {e}. Export before quitting."),
                        );
                        lock(&recording).autosave = false;
                    }
                    let render_result = {
                        let r = lock(&recording);
                        store.render(&r, false)
                    };
                    if let Err(e) = render_result {
                        warn(
                            &recording,
                            &mut store,
                            &events,
                            &logger,
                            format!("Markdown save failed: {e}"),
                        );
                    }
                    let _ = events.send(PipelineEvent::Segment(s));
                }
            }
            Err(e) => {
                engine = None;
                warn(
                    &recording,
                    &mut store,
                    &events,
                    &logger,
                    format!("Missing audio: transcription failed: {e:#}"),
                );
            }
        }
        lock(&live).finish_chunk(segment.start_sample);
    }
    let _ = capture_thread.join();
    lock(&live).finish();
    for warning in warnings_rx.try_iter() {
        warn(&recording, &mut store, &events, &logger, warning);
    }
    let event = {
        let mut r = lock(&recording);
        r.duration_ms = r
            .duration_ms
            .max(r.segments.iter().map(|s| s.end_ms).max().unwrap_or(0));
        r.status = if r.warnings.is_empty() {
            RecordingStatus::Completed
        } else {
            RecordingStatus::Incomplete
        };
        PipelineEvent::State {
            id: r.id.clone(),
            status: r.status.clone(),
            duration_ms: r.duration_ms,
        }
    };
    let completion_result = store.append(&event).and_then(|_| {
        let r = lock(&recording);
        store.render(&r, true)
    });
    if let Err(e) = completion_result {
        warn(
            &recording,
            &mut store,
            &events,
            &logger,
            format!("Completion save failed: {e}. Export before quitting."),
        );
        let mut r = lock(&recording);
        r.status = RecordingStatus::Incomplete;
        r.autosave = false;
    }
    let r = lock(&recording);
    let final_event = PipelineEvent::State {
        id: r.id.clone(),
        status: r.status.clone(),
        duration_ms: r.duration_ms,
    };
    drop(r);
    let _ = events.send(final_event);
    {
        let mut r = lock(&recording);
        if r.autosave && r.source.is_some() {
            r.segments.clear();
        }
    }
    engine
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AsrResult;
    #[test]
    fn queue_pressure_reports_loss_and_retires_the_dropped_draft() {
        let (tx, rx) = bounded(1);
        let (warnings, notices) = unbounded();
        let live = Mutex::new(LiveState::default());
        let segment = |start| Segment {
            samples: vec![0.5; 320],
            sample_rate: 16000,
            start_sample: start,
            end_sample: start + 320,
        };
        submit_final(&tx, segment(0), &live, &warnings);
        lock(&live).update_draft(320, 1, "Dropped preview".into());
        submit_final(&tx, segment(320), &live, &warnings);
        assert_eq!(rx.recv().unwrap().start_sample, 0);
        assert!(notices.recv().unwrap().contains("overflowed"));
        assert!(lock(&live).chunk_finished(320));
        lock(&live).update_draft(320, 2, "Late preview".into());
        assert!(lock(&live).draft_text().is_empty());
    }
    struct FailingEngine;
    impl AsrEngine for FailingEngine {
        fn name(&self) -> &'static str {
            "failed child"
        }
        fn transcribe(&mut self, _: &[f32]) -> Result<AsrResult> {
            anyhow::bail!("child process exited")
        }
    }
    #[test]
    fn failed_final_retires_draft_and_finishes_with_a_visible_warning() {
        let config = Config {
            save_transcript: false,
            ..Default::default()
        };
        let recording = Arc::new(Mutex::new(Recording::new(Language::Id, false)));
        let live = Arc::new(Mutex::new(LiveState::default()));
        lock(&live).update_draft(0, 1, "Temporary text".into());
        let (events, received) = unbounded();
        transcribe_with_capture(
            config,
            recording.clone(),
            0,
            Arc::new(AtomicBool::new(false)),
            RecordingStore::new(PathBuf::from("unused")),
            events,
            Arc::new(Logger::disabled()),
            Arc::new(Mutex::new(String::new())),
            live.clone(),
            Some(Box::new(FailingEngine)),
            |_, _, _| {
                let (tx, rx) = bounded(1);
                let (_, warnings) = unbounded();
                let thread = std::thread::spawn(move || {
                    tx.send(Segment {
                        samples: vec![0.5; 320],
                        sample_rate: 16000,
                        start_sample: 0,
                        end_sample: 320,
                    })
                    .unwrap();
                });
                (rx, warnings, thread)
            },
        );
        assert_eq!(lock(&recording).status, RecordingStatus::Incomplete);
        assert!(lock(&recording).text(false).is_empty());
        assert!(lock(&live).draft_text().is_empty());
        assert!(received.try_iter().any(|event| matches!(event, PipelineEvent::Warning { message, .. } if message.contains("child process exited"))));
    }
    struct DelayedEngine {
        entered: Sender<()>,
        resume: Receiver<()>,
        calls: usize,
    }
    impl AsrEngine for DelayedEngine {
        fn name(&self) -> &'static str {
            "test"
        }
        fn transcribe(&mut self, _: &[f32]) -> Result<AsrResult> {
            self.calls += 1;
            if self.calls == 1 {
                self.entered.send(()).unwrap();
                self.resume.recv().unwrap();
            }
            Ok(AsrResult {
                text: format!("Result {}.", self.calls),
                ..Default::default()
            })
        }
    }
    #[test]
    fn stop_during_inference_drains_queued_and_flushed_audio() {
        let config = Config {
            save_transcript: false,
            ..Default::default()
        };
        let r = Arc::new(Mutex::new(Recording::new(Language::En, false)));
        let recorded = r.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let request = stop.clone();
        let (entered_tx, entered_rx) = bounded(1);
        let (resume_tx, resume_rx) = bounded(1);
        let (events, _) = unbounded();
        let worker = std::thread::spawn(move || {
            transcribe_with_capture(
                config,
                recorded,
                0,
                stop,
                RecordingStore::new(PathBuf::from("unused")),
                events,
                Arc::new(Logger::disabled()),
                Arc::new(Mutex::new(String::new())),
                Arc::new(Mutex::new(LiveState::default())),
                Some(Box::new(DelayedEngine {
                    entered: entered_tx,
                    resume: resume_rx,
                    calls: 0,
                })),
                |_, stop, _| {
                    let (tx, rx) = bounded(2);
                    let (_warning, warnings) = unbounded();
                    let thread = std::thread::spawn(move || {
                        for n in 0..2 {
                            tx.send(Segment {
                                samples: vec![0.5; 320],
                                sample_rate: 16000,
                                start_sample: n * 32000,
                                end_sample: n * 32000 + 320,
                            })
                            .unwrap();
                        }
                        while !stop.load(Ordering::Acquire) {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        tx.send(Segment {
                            samples: vec![0.5; 320],
                            sample_rate: 16000,
                            start_sample: 64000,
                            end_sample: 64320,
                        })
                        .unwrap();
                    });
                    (rx, warnings, thread)
                },
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        request.store(true, Ordering::Release);
        resume_tx.send(()).unwrap();
        worker.join().unwrap();
        let r = lock(&r);
        assert_eq!(r.status, RecordingStatus::Completed);
        assert_eq!(r.segments.len(), 3);
        assert_eq!(r.segments[2].start_ms, 4000);
        assert!(r.segments.iter().all(|s| s.recording_id == r.id));
        assert_eq!(r.text(false), "Result 1. Result 2. Result 3.");
    }
    #[test]
    fn finishing_blocks_a_second_start() {
        let recording = Arc::new(Mutex::new(Recording::new(Language::En, false)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn(|| {});
        let mut a = Active {
            recording: recording.clone(),
            stop: stop.clone(),
            started: Instant::now(),
            duration_before_ms: 10000,
            finishing: false,
            thread,
        };
        let status = Mutex::new(Status::Listening);
        let paused = AtomicBool::new(false);
        let (events, _) = unbounded();
        finish_capture(&mut a, &status, &paused, &events);
        finish_capture(&mut a, &status, &paused, &events);
        assert!(a.finishing);
        assert!(stop.load(Ordering::Acquire));
        assert_eq!(lock(&recording).status, RecordingStatus::Processing);
        assert!(lock(&recording).duration_ms >= 10000);
        assert_eq!(*lock(&status), Status::Transcribing(0));
        a.thread.join().unwrap();
    }
}

#[cfg(test)]
mod library_tests {
    use super::*;
    fn handle(recording: Recording) -> PipelineHandle {
        let (control, _) = unbounded();
        let (_, events) = unbounded();
        PipelineHandle {
            status: Arc::new(Mutex::new(Status::Paused)),
            paused: Arc::new(AtomicBool::new(true)),
            control,
            records: Arc::new(Mutex::new(vec![Arc::new(Mutex::new(recording))])),
            last_text: Arc::new(Mutex::new(String::new())),
            live: Arc::new(Mutex::new(LiveState::default())),
            events,
            save_enabled: Arc::new(AtomicBool::new(false)),
            threads: vec![],
        }
    }
    #[test]
    fn failed_autosave_empty_recording_stays_in_memory() {
        let mut r = Recording::new(Language::En, false);
        r.source = Some(PathBuf::from("missing-partial.jsonl"));
        r.status = RecordingStatus::Incomplete;
        r.warnings.push("Save failed".into());
        let id = r.id.clone();
        let p = handle(r);
        assert_eq!(p.recording(&id).unwrap().warnings, vec!["Save failed"]);
    }
    #[test]
    fn renaming_an_unloaded_recording_keeps_its_text() {
        let mut r = Recording::new(Language::En, true);
        let dir = std::env::temp_dir().join(&r.id);
        let mut store = RecordingStore::new(dir.clone());
        store.begin(&mut r).unwrap();
        r.segments.push(TranscriptSegment {
            recording_id: r.id.clone(),
            sequence: 0,
            start_ms: 0,
            end_ms: 1000,
            text: "Keep these words.".into(),
            clock_time: None,
        });
        store
            .append(&PipelineEvent::Segment(r.segments[0].clone()))
            .unwrap();
        r.status = RecordingStatus::Completed;
        store
            .append(&PipelineEvent::State {
                id: r.id.clone(),
                status: r.status.clone(),
                duration_ms: 1000,
            })
            .unwrap();
        store.render(&r, true).unwrap();
        drop(store);
        r.segments.clear();
        let id = r.id.clone();
        let p = handle(r);
        p.rename(&id, "New title", &dir).unwrap();
        let loaded = p.recording(&id).unwrap();
        assert_eq!(loaded.title, "New title");
        assert_eq!(loaded.text(false), "Keep these words.");
        assert!(
            std::fs::read_to_string(dir.join(format!("{id}.md")))
                .unwrap()
                .contains("Keep these words.")
        );
        drop(p);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use crate::engine::AsrResult;

    struct AppendingEngine;
    impl AsrEngine for AppendingEngine {
        fn name(&self) -> &'static str {
            "test continuation"
        }
        fn transcribe(&mut self, _: &[f32]) -> Result<AsrResult> {
            Ok(AsrResult {
                text: "More words.".into(),
                ..Default::default()
            })
        }
    }

    fn append_audio(config: Config, recording: Recording, store: RecordingStore) -> Recording {
        let offset = recording.duration_ms;
        let recording = Arc::new(Mutex::new(recording));
        let (events, _) = unbounded();
        transcribe_with_capture(
            config,
            recording.clone(),
            offset,
            Arc::new(AtomicBool::new(false)),
            store,
            events,
            Arc::new(Logger::disabled()),
            Arc::new(Mutex::new(String::new())),
            Arc::new(Mutex::new(LiveState::default())),
            Some(Box::new(AppendingEngine)),
            |_, _, _| {
                let (tx, rx) = bounded(1);
                let (_, warnings) = unbounded();
                let thread = std::thread::spawn(move || {
                    tx.send(Segment {
                        samples: vec![0.5; 16000],
                        sample_rate: 16000,
                        start_sample: 0,
                        end_sample: 16000,
                    })
                    .unwrap();
                });
                (rx, warnings, thread)
            },
        );
        let result = lock(&recording).clone();
        result
    }

    fn previous(autosave: bool) -> Recording {
        let mut r = Recording::new(Language::En, autosave);
        r.title = "Meeting notes".into();
        r.duration_ms = 5000;
        r.status = RecordingStatus::Completed;
        r.segments.push(TranscriptSegment {
            recording_id: r.id.clone(),
            sequence: 7,
            start_ms: 4000,
            end_ms: 4500,
            text: "Keep these words.".into(),
            clock_time: None,
        });
        r
    }

    fn clean_up(dir: &Path) {
        let resolved = std::fs::canonicalize(dir).unwrap();
        assert!(resolved.starts_with(std::fs::canonicalize(std::env::temp_dir()).unwrap()));
        std::fs::remove_dir_all(resolved).unwrap();
    }

    #[test]
    fn continuation_reopens_one_journal_without_replacing_old_text() {
        let mut r = previous(true);
        let config = Config {
            base_dir: std::env::temp_dir().join(&r.id),
            language: Language::Id,
            save_transcript: false,
            ..Default::default()
        };
        let id = r.id.clone();
        let started = r.started;
        let dir = config.transcripts_dir();
        let mut store = RecordingStore::new(dir.clone());
        store.begin(&mut r).unwrap();
        store
            .append(&PipelineEvent::Segment(r.segments[0].clone()))
            .unwrap();
        store
            .append(&PipelineEvent::State {
                id: id.clone(),
                status: r.status.clone(),
                duration_ms: r.duration_ms,
            })
            .unwrap();
        store.render(&r, true).unwrap();
        drop(store);
        for expected_duration in [6000, 7000] {
            // Metadata has no segments, as it would after restarting the app.
            let (prepared, store) = prepare_recording(&config, Some(r.metadata())).unwrap();
            assert_eq!(prepared.status, RecordingStatus::Recording);
            assert_eq!(prepared.language, Language::En);
            assert!(prepared.autosave);
            r = append_audio(config.clone(), prepared, store);
            assert_eq!(r.duration_ms, expected_duration);
            assert_eq!(r.status, RecordingStatus::Completed);
        }
        let entries = crate::recording::library(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        let loaded = crate::recording::load_recording(&entries[0]).unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(loaded.started, started);
        assert_eq!(loaded.title, "Meeting notes");
        assert_eq!(loaded.duration_ms, 7000);
        assert_eq!(
            loaded.text(false),
            "Keep these words. More words. More words."
        );
        assert_eq!(
            loaded
                .segments
                .iter()
                .map(|s| s.sequence)
                .collect::<Vec<_>>(),
            [7, 8, 9]
        );
        assert_eq!(
            loaded
                .segments
                .iter()
                .map(|s| s.start_ms)
                .collect::<Vec<_>>(),
            [4000, 5000, 6000]
        );
        let journal = std::fs::read_to_string(loaded.source.unwrap()).unwrap();
        assert_eq!(
            journal
                .lines()
                .filter(|line| matches!(
                    serde_json::from_str::<PipelineEvent>(line).unwrap(),
                    PipelineEvent::Started(_)
                ))
                .count(),
            1
        );
        let markdown = std::fs::read_to_string(dir.join(format!("{id}.md"))).unwrap();
        assert!(markdown.contains("Keep these words. More words. More words."));
        clean_up(&config.base_dir);
    }

    #[test]
    fn unsaved_continuation_keeps_the_original_text_and_save_preference() {
        let r = previous(false);
        let config = Config {
            base_dir: std::env::temp_dir().join(&r.id),
            save_transcript: true,
            ..Default::default()
        };
        let id = r.id.clone();
        let (prepared, store) = prepare_recording(&config, Some(r)).unwrap();
        let result = append_audio(config.clone(), prepared, store);
        assert_eq!(result.id, id);
        assert_eq!(result.text(false), "Keep these words. More words.");
        assert_eq!(result.segments[1].sequence, 8);
        assert_eq!(result.segments[1].start_ms, 5000);
        assert_eq!(result.duration_ms, 6000);
        assert!(!result.autosave);
        assert!(result.source.is_none());
        assert!(!config.base_dir.exists());
    }

    #[test]
    fn damaged_journal_is_rejected_without_modifying_saved_results() {
        for damaged_tail in [b"{broken".as_slice(), b"\n".as_slice()] {
            let mut r = previous(true);
            let config = Config {
                base_dir: std::env::temp_dir().join(&r.id),
                ..Default::default()
            };
            let mut store = RecordingStore::new(config.transcripts_dir());
            store.begin(&mut r).unwrap();
            store
                .append(&PipelineEvent::Segment(r.segments[0].clone()))
                .unwrap();
            drop(store);
            let path = r.source.as_ref().unwrap();
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(path)
                .unwrap()
                .write_all(damaged_tail)
                .unwrap();
            let before = std::fs::read(path).unwrap();
            assert!(prepare_recording(&config, Some(r.clone())).is_err());
            assert_eq!(std::fs::read(path).unwrap(), before);
            assert_eq!(
                crate::recording::load_recording(&r).unwrap().text(false),
                "Keep these words."
            );
            clean_up(&config.base_dir);
        }
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    fn run(recording: Recording, commands: Vec<Control>) -> (Vec<Recording>, Vec<PipelineEvent>) {
        let records = Arc::new(Mutex::new(vec![Arc::new(Mutex::new(recording))]));
        let (control, input) = unbounded();
        let (events, output) = unbounded();
        for command in commands {
            control.send(command).unwrap();
        }
        control.send(Control::Shutdown).unwrap();
        controller(
            Config {
                save_transcript: false,
                audio_output_device: Some("invalid-test-output-device".into()),
                ..Default::default()
            },
            input,
            Arc::new(Mutex::new(Status::Paused)),
            Arc::new(AtomicBool::new(true)),
            records.clone(),
            Arc::new(Mutex::new(String::new())),
            Arc::new(Mutex::new(LiveState::default())),
            events,
            Arc::new(Logger::disabled()),
        );
        let remaining = lock(&records).iter().map(|r| lock(r).clone()).collect();
        (remaining, output.try_iter().collect())
    }
    #[test]
    fn delete_then_pending_continue_cannot_resurrect_an_unsaved_recording() {
        let mut r = Recording::new(Language::En, false);
        r.status = RecordingStatus::Completed;
        let id = r.id.clone();
        let (remaining, events) = run(
            r.clone(),
            vec![
                Control::Delete(Box::new(r.clone())),
                Control::Continue(Box::new(r)),
            ],
        );
        assert!(remaining.is_empty());
        assert!(events.iter().any(
            |event| matches!(event, PipelineEvent::Deleted { id: deleted } if deleted == &id)
        ));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, PipelineEvent::Started(_)))
        );
    }
    #[test]
    fn stale_completed_snapshot_cannot_delete_an_active_recording() {
        let mut active = Recording::new(Language::En, false);
        let mut stale = active.clone();
        stale.status = RecordingStatus::Completed;
        active.status = RecordingStatus::Recording;
        let (remaining, events) = run(active, vec![Control::Delete(Box::new(stale))]);
        assert_eq!(remaining.len(), 1);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, PipelineEvent::Deleted { .. }))
        );
        assert!(events.iter().any(|event| matches!(event, PipelineEvent::Warning { message, .. } if message.contains("Stop recording"))));
    }
}

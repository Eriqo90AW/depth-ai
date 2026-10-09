//! WASAPI loopback capture of the default output device, resampled to 16 kHz mono.
//!
//! Loopback capture records exactly what the machine is playing, so no virtual audio cable or
//! driver is involved. Shared mode hands us the device mix format (typically 48 kHz stereo
//! float), which is downmixed and resampled here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use rubato::{FftFixedIn, Resampler};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eCommunications, eConsole,
    eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::logging::Logger;

#[derive(Debug, Clone)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Desktop,
    Output,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    DefaultSilent,
    EndpointDisconnected,
}
#[derive(Debug, Clone)]
pub enum CaptureUpdate {
    Source(OutputDevice),
    Mode {
        mode: CaptureMode,
        endpoint: OutputDevice,
        reason: Option<FallbackReason>,
    },
    Level {
        packets: u64,
        peak_db: f32,
    },
    Notice(String),
}
type Observer = Box<dyn FnMut(CaptureUpdate) + Send>;

enum CaptureExit {
    Stopped,
    Reconnect,
    Switch(Box<EndpointProbe>),
}

struct FallbackDetection {
    last_signal: Instant,
    last_probe: Option<Instant>,
}
impl Default for FallbackDetection {
    fn default() -> Self {
        Self {
            last_signal: Instant::now(),
            last_probe: None,
        }
    }
}
impl FallbackDetection {
    fn signal(&mut self, now: Instant) {
        self.last_signal = now;
    }
    fn reset_silence(&mut self) {
        self.last_signal = Instant::now();
    }
    fn begin_probe(&mut self, now: Instant, stopped: bool) -> bool {
        if stopped
            || now.duration_since(self.last_signal) < Duration::from_secs(5)
            || self
                .last_probe
                .is_some_and(|last| now.duration_since(last) < Duration::from_secs(10))
        {
            return false;
        }
        self.last_probe = Some(now);
        true
    }
}

#[derive(Default)]
struct ProbeLevel {
    peak: f32,
    previous: Option<f32>,
    qualified: Option<f32>,
}
impl ProbeLevel {
    fn window(&mut self) {
        let db = if self.peak > 0.0 {
            20.0 * self.peak.log10()
        } else {
            f32::NEG_INFINITY
        };
        if db > -60.0 && self.previous.is_some_and(|previous| previous > -60.0) {
            self.qualified = Some(db.min(self.previous.unwrap()));
        }
        self.previous = Some(db);
        self.peak = 0.0;
    }
}

fn choose_endpoint<'a>(
    candidates: &'a [(String, f32)],
    communications: Option<&str>,
    default: Option<&str>,
) -> Option<&'a str> {
    candidates
        .iter()
        .min_by(|a, b| {
            let rank = |id: &str| {
                if Some(id) == communications {
                    0
                } else if Some(id) == default {
                    1
                } else {
                    2
                }
            };
            rank(&a.0)
                .cmp(&rank(&b.0))
                .then_with(|| b.1.total_cmp(&a.1))
                .then_with(|| a.0.cmp(&b.0))
        })
        .map(|candidate| candidate.0.as_str())
}

// Probes own independent WASAPI clients but never a transcription sink or a resampler.
// Dropping a batch stops every stream, including when Stop or a recovered default cancels it.
struct EndpointProbe {
    device: OutputDevice,
    _resources: CaptureResources,
    capture: IAudioCaptureClient,
    mix: MixFormat,
    mono: Vec<f32>,
    level: ProbeLevel,
}
impl EndpointProbe {
    unsafe fn open(device: &IMMDevice) -> Result<Self> {
        unsafe {
            let description = describe_device(device)?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let format = client.GetMixFormat()?;
            let mix_result = read_mix_format(format);
            let initialized = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                10_000_000,
                0,
                format,
                None,
            );
            CoTaskMemFree(Some(format.cast()));
            let mix = mix_result?;
            initialized?;
            let event = CreateEventW(None, false, false, None)?;
            let resources = CaptureResources {
                event,
                client: client.clone(),
            };
            client.SetEventHandle(event)?;
            let capture = client.GetService()?;
            client.Start()?;
            Ok(Self {
                device: description,
                _resources: resources,
                capture,
                mix,
                mono: Vec::new(),
                level: ProbeLevel::default(),
            })
        }
    }
    unsafe fn poll(&mut self, stop: &AtomicBool) -> Result<()> {
        unsafe {
            while !stop.load(Ordering::Relaxed) && self.capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 && !data.is_null() {
                    decode_interleaved(data, frames as usize, &self.mix, &mut self.mono);
                    for sample in &self.mono {
                        self.level.peak = self.level.peak.max(sample.abs());
                    }
                }
                self.capture.ReleaseBuffer(frames)?;
            }
            Ok(())
        }
    }
}
trait ProbeAudio {
    fn id(&self) -> &str;
    fn level(&mut self) -> &mut ProbeLevel;
    unsafe fn read(&mut self, stop: &AtomicBool) -> Result<()>;
}
impl ProbeAudio for EndpointProbe {
    fn id(&self) -> &str {
        &self.device.id
    }
    fn level(&mut self) -> &mut ProbeLevel {
        &mut self.level
    }
    unsafe fn read(&mut self, stop: &AtomicBool) -> Result<()> {
        unsafe { self.poll(stop) }
    }
}
struct ProbeBatch<P = EndpointProbe> {
    probes: Vec<P>,
    communications: Option<String>,
    default: Option<String>,
    window_at: Instant,
    windows: u8,
    finished: bool,
}
impl ProbeBatch {
    unsafe fn open(
        enumerator: &IMMDeviceEnumerator,
        stop: &AtomicBool,
        logger: &Logger,
    ) -> Result<Self> {
        unsafe {
            let endpoint_id = |role| {
                enumerator
                    .GetDefaultAudioEndpoint(eRender, role)
                    .ok()
                    .and_then(|device| describe_device(&device).ok())
                    .map(|device| device.id)
            };
            let communications = endpoint_id(eCommunications);
            let default = endpoint_id(eConsole);
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut probes = Vec::new();
            for i in 0..devices.GetCount()? {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match devices
                    .Item(i)
                    .map_err(anyhow::Error::from)
                    .and_then(|device| EndpointProbe::open(&device))
                {
                    Ok(mut probe) => {
                        // Discard setup audio before the verification windows begin.
                        probe.poll(stop)?;
                        probe.level.peak = 0.0;
                        probes.push(probe);
                    }
                    Err(error) => logger.warn(format!("endpoint probe unavailable: {error:#}")),
                }
            }
            // All clients share the same two one-second verification windows.
            for probe in &mut probes {
                probe.poll(stop)?;
                probe.level.peak = 0.0;
            }
            Ok(Self {
                probes,
                communications,
                default,
                window_at: Instant::now(),
                windows: 0,
                finished: false,
            })
        }
    }
}
impl<P: ProbeAudio> ProbeBatch<P> {
    unsafe fn poll(&mut self, stop: &AtomicBool, logger: &Logger) -> Result<Option<P>> {
        unsafe { self.poll_at(stop, logger, Instant::now()) }
    }
    unsafe fn poll_at(
        &mut self,
        stop: &AtomicBool,
        logger: &Logger,
        now: Instant,
    ) -> Result<Option<P>> {
        unsafe {
            if stop.load(Ordering::Relaxed) {
                self.finished = true;
                return Ok(None);
            }
            self.probes.retain_mut(|probe| match probe.read(stop) {
                Ok(()) => true,
                Err(error) => {
                    logger.warn(format!(
                        "probe [{}] failed; keeping current capture: {error:#}",
                        probe.id()
                    ));
                    false
                }
            });
            if now.duration_since(self.window_at) < Duration::from_secs(1) {
                return Ok(None);
            }
            for probe in &mut self.probes {
                probe.level().window();
                let peak_db = probe.level().previous.unwrap();
                logger.info(format!(
                    "endpoint probe [{}]: peak {:.1} dBFS, window {}",
                    probe.id(),
                    peak_db,
                    self.windows + 1
                ));
            }
            self.windows += 1;
            self.window_at = now;
            if self.windows < 2 {
                return Ok(None);
            }
            self.finished = true;
            let candidates: Vec<_> = self
                .probes
                .iter_mut()
                .filter_map(|probe| {
                    probe
                        .level()
                        .qualified
                        .map(|level| (probe.id().to_owned(), level))
                })
                .collect();
            let selected = choose_endpoint(
                &candidates,
                self.communications.as_deref(),
                self.default.as_deref(),
            );
            Ok(selected
                .and_then(|id| self.probes.iter().position(|probe| probe.id() == id))
                .map(|index| self.probes.swap_remove(index)))
        }
    }
}

unsafe fn detect_endpoint(
    probes: &mut Option<ProbeBatch>,
    detection: &mut FallbackDetection,
    enumerator: &IMMDeviceEnumerator,
    paused: bool,
    stop: &AtomicBool,
    logger: &Logger,
) -> Option<EndpointProbe> {
    unsafe {
        if paused || stop.load(Ordering::Relaxed) {
            *probes = None;
            detection.reset_silence();
            return None;
        }
        if let Some(batch) = probes {
            let selected = match batch.poll(stop, logger) {
                Ok(selected) => selected,
                Err(error) => {
                    logger.warn(format!(
                        "endpoint probes failed; keeping current capture: {error:#}"
                    ));
                    *probes = None;
                    return None;
                }
            };
            if batch.finished {
                *probes = None;
            }
            selected
        } else {
            if detection.begin_probe(Instant::now(), false) {
                match ProbeBatch::open(enumerator, stop, logger) {
                    Ok(batch) => *probes = Some(batch),
                    Err(error) => logger.warn(format!(
                        "endpoint probes failed; keeping current capture: {error:#}"
                    )),
                }
            }
            None
        }
    }
}

unsafe fn describe_device(device: &IMMDevice) -> Result<OutputDevice> {
    unsafe {
        let raw_id = device.GetId()?;
        let id = raw_id.to_string()?;
        CoTaskMemFree(Some(raw_id.0.cast()));
        let key = windows::Win32::Foundation::PROPERTYKEY {
            fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
            pid: 14,
        };
        let name = (|| -> Result<String> {
            use windows::Win32::System::Com::StructuredStorage::{
                PropVariantClear, PropVariantToStringAlloc,
            };
            let store = device.OpenPropertyStore(STGM_READ)?;
            let mut value = store.GetValue(&key)?;
            let raw = PropVariantToStringAlloc(&value);
            let _ = PropVariantClear(&mut value);
            let raw = raw?;
            let name = raw.to_string();
            CoTaskMemFree(Some(raw.0.cast()));
            Ok(name?)
        })()
        .unwrap_or_else(|_| id.clone());
        Ok(OutputDevice { id, name })
    }
}

/// Enumerate active playback endpoints for Settings without changing Windows routing.
pub fn output_devices() -> Result<Vec<OutputDevice>> {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        let owns_com = hr.is_ok();
        if hr.is_err() && hr.0 != 0x80010106u32 as i32 {
            return Err(anyhow!("initializing COM: {hr:?}"));
        }
        let result = (|| -> Result<Vec<OutputDevice>> {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut result = Vec::new();
            for i in 0..devices.GetCount()? {
                result.push(describe_device(&devices.Item(i)?)?);
            }
            Ok(result)
        })();
        if owns_com {
            CoUninitialize();
        }
        result
    }
}

/// Read-only routing diagnostics: identifies playback sessions on each endpoint.
pub fn output_diagnostics() -> Result<Vec<String>> {
    use windows::Win32::Media::Audio::{
        IAudioSessionControl2, IAudioSessionManager2, ISimpleAudioVolume,
    };
    use windows::core::Interface;
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() && hr.0 != 0x80010106u32 as i32 {
            return Err(anyhow!("initializing COM: {hr:?}"));
        }
        let result = (|| -> Result<Vec<String>> {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let default =
                describe_device(&enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?)?.id;
            let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            let mut lines = Vec::new();
            for i in 0..devices.GetCount()? {
                let device = devices.Item(i)?;
                let info = describe_device(&device)?;
                lines.push(format!(
                    "{}{} [{}]",
                    if info.id == default { "DEFAULT " } else { "" },
                    info.name,
                    info.id
                ));
                let endpoint: windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume =
                    device.Activate(CLSCTX_ALL, None)?;
                lines.push(format!(
                    "  speaker mute {} master volume {:.2}",
                    endpoint.GetMute()?.as_bool(),
                    endpoint.GetMasterVolumeLevelScalar()?
                ));
                let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
                let sessions = manager.GetSessionEnumerator()?;
                for j in 0..sessions.GetCount()? {
                    let session = sessions.GetSession(j)?;
                    let control: IAudioSessionControl2 = session.cast()?;
                    let volume: ISimpleAudioVolume = session.cast()?;
                    lines.push(format!(
                        "  pid {} state {:?} mute {} volume {:.2}",
                        control.GetProcessId()?,
                        session.GetState()?,
                        volume.GetMute()?.as_bool(),
                        volume.GetMasterVolume()?
                    ));
                }
            }
            Ok(lines)
        })();
        if hr.is_ok() {
            CoUninitialize();
        }
        result
    }
}

struct CaptureResources {
    event: HANDLE,
    client: IAudioClient,
}
impl Drop for CaptureResources {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

use windows::Win32::Media::Audio::{
    AUDIOCLIENT_ACTIVATION_PARAMS, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
};
use windows::Win32::System::Com::{IAgileObject, IAgileObject_Impl};

#[windows::core::implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct LoopbackActivation {
    ready: crossbeam_channel::Sender<std::result::Result<usize, windows::core::HRESULT>>,
    // Keep the blob alive even if activation times out and completion arrives later.
    _params: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl IAgileObject_Impl for LoopbackActivation_Impl {}
impl IActivateAudioInterfaceCompletionHandler_Impl for LoopbackActivation_Impl {
    fn ActivateCompleted(
        &self,
        operation: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        use windows::core::Interface;
        let result = (|| -> windows::core::Result<usize> {
            let operation = operation.as_ref().ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT(0x80004003u32 as i32))
            })?;
            let mut status = windows::core::HRESULT(0);
            let mut unknown = None;
            unsafe {
                operation.GetActivateResult(&mut status, &mut unknown)?;
            }
            status.ok()?;
            let client: IAudioClient = unknown
                .ok_or_else(|| {
                    windows::core::Error::from_hresult(windows::core::HRESULT(0x80004003u32 as i32))
                })?
                .cast()?;
            // Both the completion callback and capture thread are in the MTA. Transfer
            // the owned COM reference once; no apartment-bound interface crosses to the GUI.
            Ok(client.into_raw() as usize)
        })()
        .map_err(|e| e.code());
        if let Err(crossbeam_channel::SendError(Ok(raw))) = self.ready.send(result) {
            unsafe {
                drop(IAudioClient::from_raw(raw as *mut _));
            }
        }
        Ok(())
    }
}

unsafe fn activate_desktop_loopback(stop: &AtomicBool) -> Result<IAudioClient> {
    use windows::Win32::Media::Audio::{
        AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, ActivateAudioInterfaceAsync,
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    };
    use windows::Win32::System::Com::{BLOB, StructuredStorage::PROPVARIANT};
    use windows::core::Interface;
    unsafe {
        let mut params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: std::process::id(),
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
                },
            },
        });
        // This blob borrows Rust-owned activation parameters. PROPVARIANT's normal
        // Drop would pass them to CoTaskMemFree; the handler owns their lifetime.
        let mut variant = std::mem::ManuallyDrop::new(PROPVARIANT::default());
        (*variant.Anonymous.Anonymous).vt = windows::Win32::System::Variant::VT_BLOB;
        (*variant.Anonymous.Anonymous).Anonymous.blob = BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: (&mut *params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
        };
        let (ready, rx) = crossbeam_channel::bounded(1);
        let handler: IActivateAudioInterfaceCompletionHandler = LoopbackActivation {
            ready,
            _params: params,
        }
        .into();
        let _operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &handler,
        )
        .context("activating mute-safe desktop loopback")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let raw = loop {
            anyhow::ensure!(!stop.load(Ordering::Relaxed), "capture cancelled");
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => break result.map_err(windows::core::Error::from_hresult)?,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(error) => return Err(anyhow!("desktop loopback activation failed: {error}")),
            }
        };
        Ok(IAudioClient::from_raw(raw as *mut _))
    }
}

/// The rate both engines require.
pub const TARGET_RATE: u32 = 16_000;
/// How much audio WASAPI buffers per event.
const BUFFER_HNS: i64 = 10_000_000 / 10; // 100 ms in 100 ns units

/// Audio format of the loopback stream, after decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleFormat {
    F32,
    I16,
    I32,
}

impl SampleFormat {
    fn bytes(self) -> usize {
        match self {
            SampleFormat::F32 | SampleFormat::I32 => 4,
            SampleFormat::I16 => 2,
        }
    }
}

/// A capture device description plus the decoded format.
#[derive(Debug, Clone, Copy)]
struct MixFormat {
    channels: usize,
    rate: u32,
    format: SampleFormat,
}

/// Start capturing on a background thread. `sink` receives 16 kHz mono blocks.
///
/// The thread exits when `stop` becomes true, and logs (rather than panics) if capture cannot
/// start, so the tray can report the problem.
pub fn spawn_loopback(
    sink: Box<dyn FnMut(&[f32]) + Send>,
    stop: Arc<AtomicBool>,
    logger: Arc<Logger>,
) -> std::thread::JoinHandle<()> {
    spawn_loopback_inner(sink, None, stop, logger)
}

/// Start capturing, skipping the resampler while `paused` is set.
///
/// While paused the pipeline discards everything anyway, so decoding/resampling
/// those blocks is pure CPU waste. `sink` still gets one empty call per wake so
/// the segmentation layer can observe the pause transition (flush + status).
pub fn spawn_loopback_gated(
    sink: Box<dyn FnMut(&[f32]) + Send>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    logger: Arc<Logger>,
) -> std::thread::JoinHandle<()> {
    spawn_loopback_inner(sink, Some(paused), stop, logger)
}

fn spawn_loopback_inner(
    sink: Box<dyn FnMut(&[f32]) + Send>,
    paused: Option<Arc<AtomicBool>>,
    stop: Arc<AtomicBool>,
    logger: Arc<Logger>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("wasapi-loopback".to_string())
        .spawn(move || {
            if let Err(err) = run_gated(sink, paused.as_ref(), &stop, &logger) {
                logger.error(format!("loopback capture stopped: {err:#}"));
            }
        })
        .expect("spawning the capture thread")
}

/// Capture from the default render device until `stop` is set.
pub fn run(
    sink: Box<dyn FnMut(&[f32]) + Send>,
    stop: &AtomicBool,
    logger: &Arc<Logger>,
) -> Result<()> {
    run_gated(sink, None, stop, logger)
}

/// Capture with an optional pause gate (see [`spawn_loopback_gated`]).
pub fn run_gated(
    sink: Box<dyn FnMut(&[f32]) + Send>,
    paused: Option<&Arc<AtomicBool>>,
    stop: &AtomicBool,
    logger: &Arc<Logger>,
) -> Result<()> {
    run_with_clock(sink, paused, stop, logger, None, None, None)
}
/// Packet timestamps are capture-relative, measured from WASAPI's QPC clock.
pub fn run_timed(
    sink: Box<dyn FnMut(&[f32], u64) + Send>,
    stop: &AtomicBool,
    logger: &Arc<Logger>,
) -> Result<()> {
    run_timed_selected(sink, stop, logger, None, None)
}

pub fn run_timed_selected(
    sink: Box<dyn FnMut(&[f32], u64) + Send>,
    stop: &AtomicBool,
    logger: &Arc<Logger>,
    output_device: Option<&str>,
    observer: Option<Observer>,
) -> Result<()> {
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let read = clock.clone();
    run_with_clock(
        timed_sink(sink, read),
        None,
        stop,
        logger,
        Some(clock),
        output_device,
        observer,
    )
}
fn timed_sink(
    mut sink: Box<dyn FnMut(&[f32], u64) + Send>,
    clock: Arc<std::sync::atomic::AtomicU64>,
) -> Box<dyn FnMut(&[f32]) + Send> {
    Box::new(move |audio| {
        let start = clock.load(Ordering::Acquire);
        sink(audio, start);
        clock.store(
            start + (audio.len() as u64 * 1000).div_ceil(TARGET_RATE as u64),
            Ordering::Release,
        );
    })
}

fn run_with_clock(
    mut sink: Box<dyn FnMut(&[f32]) + Send>,
    paused: Option<&Arc<AtomicBool>>,
    stop: &AtomicBool,
    logger: &Arc<Logger>,
    clock: Option<Arc<std::sync::atomic::AtomicU64>>,
    output_device: Option<&str>,
    mut observer: Option<Observer>,
) -> Result<()> {
    // SAFETY: COM is initialised for this thread and uninitialised before returning.
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        // RPC_E_CHANGED_MODE means COM was already initialised with another model; that is fine
        // for these interfaces, so only hard failures abort.
        if hr.is_err() && hr.0 != 0x8001_0106u32 as i32 {
            return Err(anyhow!("CoInitializeEx failed: {hr:?}"));
        }
        let mut counter = 0i64;
        let mut frequency = 0i64;
        windows::Win32::System::Performance::QueryPerformanceCounter(&mut counter)?;
        windows::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)?;
        let origin_hns = (counter as u128 * 10_000_000 / frequency.max(1) as u128) as u64;
        let mut selected = output_device.map(str::to_owned);
        let mut fallback = false;
        let mut prepared = None;
        let mut reconnect_reason = None;
        let mut detection = FallbackDetection::default();
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            match capture_inner(
                &mut sink,
                paused,
                stop,
                logger,
                clock.as_ref(),
                selected.as_deref(),
                &mut observer,
                origin_hns,
                output_device.is_none(),
                fallback,
                &mut detection,
                &mut prepared,
                reconnect_reason,
            ) {
                Ok(CaptureExit::Switch(probe)) => {
                    let id = probe.device.id.clone();
                    prepared = Some(*probe);
                    logger.info(format!(
                        "capture transition: default -> output [{id}], default silent"
                    ));
                    selected = Some(id);
                    fallback = true;
                    reconnect_reason = None;
                }
                Ok(CaptureExit::Reconnect) => {
                    logger.info("capture transition: reconnecting default capture");
                    selected = None;
                    fallback = false;
                    detection.reset_silence();
                }
                Ok(CaptureExit::Stopped) => break Ok(()),
                Err(e) if fallback => {
                    logger.warn(format!(
                        "fallback endpoint unavailable; returning to default: {e:#}"
                    ));
                    selected = None;
                    fallback = false;
                    detection.reset_silence();
                    reconnect_reason = Some(FallbackReason::EndpointDisconnected);
                    if let Some(observer) = &mut observer {
                        observer(CaptureUpdate::Mode {
                            mode: CaptureMode::Desktop,
                            endpoint: OutputDevice {
                                id: String::new(),
                                name: "Windows default".into(),
                            },
                            reason: Some(FallbackReason::EndpointDisconnected),
                        });
                    }
                }
                Err(e) => break Err(e),
            }
        };
        drop(prepared);
        if hr.is_ok() {
            CoUninitialize();
        }
        result
    }
}

unsafe fn capture_inner(
    sink: &mut Box<dyn FnMut(&[f32]) + Send>,
    paused: Option<&Arc<AtomicBool>>,
    stop: &AtomicBool,
    logger: &Arc<Logger>,
    clock: Option<&Arc<std::sync::atomic::AtomicU64>>,
    output_device: Option<&str>,
    observer: &mut Option<Observer>,
    origin_hns: u64,
    automatic: bool,
    fallback: bool,
    detection: &mut FallbackDetection,
    prepared: &mut Option<EndpointProbe>,
    reconnect_reason: Option<FallbackReason>,
) -> Result<CaptureExit> {
    // SAFETY: all COM calls below run on a thread that initialised COM.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("creating the audio device enumerator")?;
        let device = match output_device {
            Some(id) => {
                let id: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
                enumerator
                    .GetDevice(windows::core::PCWSTR(id.as_ptr()))
                    .context("selected output device is unavailable")?
            }
            None => enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .context("no default output device to capture")?,
        };
        let description = describe_device(&device)?;
        anyhow::ensure!(
            device.GetState()? == DEVICE_STATE_ACTIVE,
            "selected output device is disconnected: {}",
            description.name
        );
        logger.info(format!(
            "capture source: {} [{}]",
            description.name, description.id
        ));
        if let Some(observer) = observer {
            observer(CaptureUpdate::Notice(String::new()));
        }
        let verified = prepared.take();
        let (client, desktop_capture) = if let Some(probe) = &verified {
            (probe._resources.client.clone(), false)
        } else if output_device.is_none() {
            match activate_desktop_loopback(stop) {
                Ok(client) => (client, true),
                Err(e) => {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(CaptureExit::Stopped);
                    }
                    logger.warn(format!(
                        "mute-safe loopback unavailable; using output endpoint: {e:#}"
                    ));
                    if let Some(observer) = observer {
                        observer(CaptureUpdate::Notice("Mute-safe desktop capture is unavailable on this Windows version. Keep Windows speakers unmuted while recording.".into()));
                    }
                    (device.Activate::<IAudioClient>(CLSCTX_ALL, None)?, false)
                }
            }
        } else {
            (device.Activate::<IAudioClient>(CLSCTX_ALL, None)?, false)
        };
        let mut source = description.clone();
        if desktop_capture {
            source.name = format!("Desktop audio before speaker mute · {}", source.name);
        } else {
            let endpoint: windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume =
                device.Activate(CLSCTX_ALL, None)?;
            if endpoint.GetMute()?.as_bool() || endpoint.GetMasterVolumeLevelScalar()? == 0.0 {
                if let Some(observer) = observer {
                    observer(CaptureUpdate::Notice("This output device is muted. Select Windows default for mute-safe desktop capture, or unmute this output.".into()));
                }
            }
        }
        if let Some(observer) = observer {
            observer(CaptureUpdate::Source(source));
            observer(CaptureUpdate::Mode {
                mode: if desktop_capture {
                    CaptureMode::Desktop
                } else {
                    CaptureMode::Output
                },
                endpoint: description.clone(),
                reason: if fallback {
                    Some(FallbackReason::DefaultSilent)
                } else {
                    reconnect_reason
                },
            });
            if fallback {
                observer(CaptureUpdate::Notice(
                    "Keep this output unmuted while recording.".into(),
                ));
            }
        }
        let (mix, _resources, capture) = if let Some(mut probe) = verified {
            probe.poll(stop)?;
            (probe.mix, probe._resources, probe.capture)
        } else {
            let mut desktop_format = WAVEFORMATEX {
                wFormatTag: 1,
                nChannels: 2,
                nSamplesPerSec: 48000,
                nAvgBytesPerSec: 192000,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            };
            let mix_ptr: *mut WAVEFORMATEX = if desktop_capture {
                &mut desktop_format
            } else {
                client.GetMixFormat().context("reading the mix format")?
            };
            // If the format is unreadable the buffer is deliberately leaked: capture cannot proceed
            // anyway, and it is a few dozen bytes released when the process exits.
            let mix = read_mix_format(mix_ptr)?;

            // Shared-mode loopback needs a non-zero buffer duration and a zero period. The format
            // pointer has to stay valid until Initialize has copied it, so it is released *after*
            // the call — passing a freed pointer yields E_INVALIDARG (0x80070057).
            let initialised = client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | if desktop_capture {
                        windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    } else {
                        0
                    },
                if desktop_capture { 0 } else { BUFFER_HNS },
                0,
                mix_ptr,
                None,
            );
            // GetMixFormat allocates with CoTaskMemAlloc, so it is released with CoTaskMemFree once
            // Initialize is finished with it.
            if !desktop_capture {
                CoTaskMemFree(Some(mix_ptr as *const core::ffi::c_void));
            }
            initialised.context("initialising loopback capture")?;

            logger.info(format!(
                "capturing loopback: {} channel(s) at {} Hz, {:?}",
                mix.channels, mix.rate, mix.format
            ));

            let event: HANDLE =
                CreateEventW(None, false, false, None).context("creating the audio event")?;
            let _resources = CaptureResources {
                event,
                client: client.clone(),
            };
            client
                .SetEventHandle(event)
                .context("registering the audio event")?;
            let capture: IAudioCaptureClient =
                client.GetService().context("getting IAudioCaptureClient")?;

            client.Start().context("starting loopback capture")?;
            (mix, _resources, capture)
        };
        let event = _resources.event;
        let mut stage = ResampleStage::new(mix.rate)?;
        let mut mono = Vec::<f32>::with_capacity(4096);
        let mut peak = 0.0f32;
        let mut announced_audio = false;
        // Tracks the pause gate so a stale resampler tail is dropped on resume.
        let mut was_paused = false;

        let mut packets = 0u64;
        let mut meter_peak = 0.0f32;
        let mut meter_at = Instant::now();
        logger.info(format!(
            "loopback capture running: mode {}, endpoint [{}], fallback {}",
            if desktop_capture { "desktop" } else { "output" },
            description.id,
            fallback
        ));

        let mut probes: Option<ProbeBatch> = None;
        while !stop.load(Ordering::Relaxed) {
            // Also drain on timeout: the default must still be silent when probes qualify,
            // even if Windows missed an event notification.
            let _ = WaitForSingleObject(event, 200);
            if meter_at.elapsed() >= Duration::from_secs(1) {
                let peak_db = if meter_peak > 0.0 {
                    20.0 * meter_peak.log10()
                } else {
                    f32::NEG_INFINITY
                };
                logger.info(format!(
                    "capture health: {packets} packets, peak {peak_db:.1} dBFS"
                ));
                if let Some(observer) = observer {
                    observer(CaptureUpdate::Level { packets, peak_db });
                }
                meter_peak = 0.0;
                meter_at = Instant::now();
                if fallback {
                    anyhow::ensure!(
                        device.GetState()? == DEVICE_STATE_ACTIVE,
                        "fallback endpoint disconnected"
                    );
                }
                if output_device.is_none() {
                    let current = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
                    if describe_device(&current)?.id != description.id {
                        stage.flush(sink)?;
                        return Ok(CaptureExit::Reconnect);
                    }
                }
            }

            let now_paused = paused.is_some_and(|p| p.load(Ordering::Relaxed));
            if now_paused {
                // Drain the device queue without decoding or resampling: the
                // pipeline discards paused audio anyway. One empty sink call lets
                // the segmenter observe the pause edge (flush + status update).
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    if capture
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                        .is_err()
                    {
                        break;
                    }
                    let _ = capture.ReleaseBuffer(frames);
                    match capture.GetNextPacketSize() {
                        Ok(next) if next > 0 => {}
                        _ => break,
                    }
                }
                was_paused = true;
                probes = None;
                detection.reset_silence();
                sink(&[]);
                continue;
            }
            if was_paused {
                // Stale tail from before the pause must not glue to fresh audio.
                was_paused = false;
                stage.clear();
            }
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames: u32 = 0;
                let mut flags: u32 = 0;
                let mut qpc_hns = 0u64;
                if let Err(err) =
                    capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc_hns))
                {
                    if output_device.is_none() {
                        if let Ok(current) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                            if describe_device(&current)?.id != description.id {
                                stage.flush(sink)?;
                                return Ok(CaptureExit::Reconnect);
                            }
                        }
                    }
                    return Err(anyhow!("GetBuffer failed: {err}"));
                }
                if frames > 0 {
                    packets += 1;
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                        mono.clear();
                        mono.resize(frames as usize, 0.0);
                    } else {
                        decode_interleaved(data, frames as usize, &mix, &mut mono);
                    }
                    // Report the first real audio seen, so "nothing is transcribed yet" can be
                    // told apart from "the logged-on device is playing silence".
                    for &sample in mono.iter() {
                        let magnitude = sample.abs();
                        meter_peak = meter_peak.max(magnitude);
                        if magnitude > peak {
                            peak = magnitude;
                        }
                    }
                    if automatic && !fallback && mono.iter().any(|s| s.abs() > 0.001) {
                        detection.signal(Instant::now());
                        probes = None;
                    }
                    if !announced_audio && peak > 0.001 {
                        announced_audio = true;
                        logger.info(format!(
                            "capture is receiving audio (peak {:.1} dBFS)",
                            20.0 * peak.log10()
                        ));
                    }
                    if let Some(clock) = clock {
                        clock.fetch_max(
                            qpc_hns.saturating_sub(origin_hns) / 10_000,
                            Ordering::Release,
                        );
                    }
                    stage.push(&mono, sink)?;
                }
                let _ = capture.ReleaseBuffer(frames);

                // windows-rs returns the pending packet size directly (no out parameter).
                match capture.GetNextPacketSize() {
                    Ok(next) if next > 0 => {}
                    _ => break,
                }
            }
            if automatic
                && !fallback
                && let Some(selected) = detect_endpoint(
                    &mut probes,
                    detection,
                    &enumerator,
                    paused.is_some_and(|p| p.load(Ordering::Relaxed)),
                    stop,
                    logger,
                )
            {
                stage.clear();
                return Ok(CaptureExit::Switch(Box::new(selected)));
            }
        }

        stage.flush(sink)?;
        Ok(CaptureExit::Stopped)
    }
}

/// Read the fields we need out of a `WAVEFORMATEX`, resolving extensible sub-formats.
unsafe fn read_mix_format(ptr: *const WAVEFORMATEX) -> Result<MixFormat> {
    if ptr.is_null() {
        return Err(anyhow!("the device reported a null mix format"));
    }
    // SAFETY: the pointer comes from GetMixFormat and is valid until CoTaskMemFree.
    let wfx = unsafe { *ptr };
    const WAVE_FORMAT_PCM: u16 = 1;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    const SUBFORMAT_PCM: u32 = 0x0000_0001;
    const SUBFORMAT_FLOAT: u32 = 0x0000_0003;

    let mut tag = wfx.wFormatTag;
    if tag == WAVE_FORMAT_EXTENSIBLE && wfx.cbSize >= 22 {
        // WAVEFORMATEXTENSIBLE inserts wValidBitsPerSample (2 bytes) and dwChannelMask (4 bytes)
        // between WAVEFORMATEX and the SubFormat GUID, which starts at byte 24. Reading at 18
        // instead yields the valid-bits/channel-mask bytes and a nonsense format tag.
        const SUBFORMAT_OFFSET: usize = 18 + 2 + 4;
        let ext = unsafe { (ptr as *const u8).add(SUBFORMAT_OFFSET) };
        let sub = unsafe { std::ptr::read_unaligned(ext as *const u32) };
        tag = match sub {
            SUBFORMAT_FLOAT => WAVE_FORMAT_IEEE_FLOAT,
            SUBFORMAT_PCM => WAVE_FORMAT_PCM,
            other => (other & 0xFFFF) as u16,
        };
    }

    let format = match (tag, wfx.wBitsPerSample) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleFormat::F32,
        (WAVE_FORMAT_PCM, 16) => SampleFormat::I16,
        (WAVE_FORMAT_PCM, 32) => SampleFormat::I32,
        (tag, bits) => {
            return Err(anyhow!(
                "unsupported mix format: tag {tag}, {bits} bits per sample"
            ));
        }
    };

    Ok(MixFormat {
        channels: wfx.nChannels.max(1) as usize,
        rate: wfx.nSamplesPerSec,
        format,
    })
}

/// Downmix interleaved device audio to mono `f32`.
fn decode_interleaved(data: *const u8, frames: usize, mix: &MixFormat, out: &mut Vec<f32>) {
    let stride = mix.channels * mix.format.bytes();
    out.clear();
    out.reserve(frames);
    for frame in 0..frames {
        let mut sum = 0.0f32;
        for channel in 0..mix.channels {
            let offset = frame * stride + channel * mix.format.bytes();
            // SAFETY: the buffer holds `frames * stride` readable bytes, per the mix format.
            let value = unsafe {
                match mix.format {
                    SampleFormat::F32 => (data.add(offset) as *const f32).read_unaligned(),
                    SampleFormat::I16 => {
                        (data.add(offset) as *const i16).read_unaligned() as f32 / 32_768.0
                    }
                    SampleFormat::I32 => {
                        (data.add(offset) as *const i32).read_unaligned() as f32 / 2_147_483_648.0
                    }
                }
            };
            sum += value;
        }
        out.push(sum / mix.channels as f32);
    }
}

fn make_resampler(rate: u32) -> Result<FftFixedIn<f32>> {
    // ~85 ms of input per call; large enough to be efficient, small enough for low latency.
    let chunk = ((rate as usize * 85) / 1000).max(64);
    FftFixedIn::<f32>::new(rate as usize, TARGET_RATE as usize, chunk, 2, 1)
        .map_err(|e| anyhow!("building the resampler ({rate} -> {TARGET_RATE} Hz): {e}"))
}

/// Streaming resampler that carries the unprocessed tail from one capture block to the next.
///
/// A WASAPI block is rarely an exact multiple of the resampler's frame requirement, so leftovers
/// are buffered rather than dropped — otherwise every block would lose a slice of audio.
struct ResampleStage {
    resampler: FftFixedIn<f32>,
    pending: Vec<f32>,
    passthrough: bool,
}

impl ResampleStage {
    fn new(rate: u32) -> Result<Self> {
        Ok(Self {
            resampler: make_resampler(rate)?,
            pending: Vec::new(),
            passthrough: rate == TARGET_RATE,
        })
    }

    fn flush(&mut self, sink: &mut Box<dyn FnMut(&[f32]) + Send>) -> Result<()> {
        if !self.pending.is_empty() {
            let remaining = std::mem::take(&mut self.pending);
            let output = self
                .resampler
                .process_partial(Some(&[remaining]), None)
                .map_err(|e| anyhow!("flushing resampler: {e}"))?;
            if let Some(first) = output.first() {
                sink(first);
            }
        }
        Ok(())
    }
    /// Drop any buffered tail (used when resuming after a pause-skip).
    fn clear(&mut self) {
        self.pending.clear();
    }

    /// Resample one mono block and hand whole output blocks to `sink`.
    fn push(&mut self, mono: &[f32], sink: &mut Box<dyn FnMut(&[f32]) + Send>) -> Result<()> {
        if self.passthrough {
            if !mono.is_empty() {
                sink(mono);
            }
            return Ok(());
        }
        self.pending.extend_from_slice(mono);
        loop {
            // The requirement can change between calls, so re-read it every iteration.
            let needed = self.resampler.input_frames_next();
            if needed == 0 || self.pending.len() < needed {
                break;
            }
            let block: Vec<f32> = self.pending.drain(..needed).collect();
            match self.resampler.process(&[block], None) {
                Ok(channels) => {
                    if let Some(first) = channels.first()
                        && !first.is_empty()
                    {
                        sink(first);
                    }
                }
                Err(e) => return Err(anyhow!("resampling failed: {e}")),
            }
        }
        Ok(())
    }
}

/// Write mono 16 kHz float samples as a WAV, for the `--record` diagnostic mode.
pub fn write_wav(path: &std::path::Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating {}", path.display()))?;
    for &s in samples {
        writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Read a 16 kHz mono WAV into `f32` samples, for the `--once` diagnostic mode.
pub fn read_wav(path: &std::path::Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32_768.0))
            .collect::<Result<Vec<_>, _>>()?,
        (hound::SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / 2_147_483_648.0))
            .collect::<Result<Vec<_>, _>>()?,
        (hound::SampleFormat::Float, _) => {
            reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?
        }
        (fmt, bits) => return Err(anyhow!("unsupported WAV format: {fmt:?}, {bits} bits")),
    };

    // Downmix to mono if needed.
    let mut mono = if spec.channels <= 1 {
        samples
    } else {
        let channels = spec.channels as usize;
        samples
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    if spec.sample_rate != TARGET_RATE {
        let mut resampler = make_resampler(spec.sample_rate)?;
        let mut out = Vec::with_capacity(
            mono.len() * TARGET_RATE as usize / spec.sample_rate.max(1) as usize + 1024,
        );
        let mut offset = 0;
        let needed = resampler.input_frames_next();
        while offset + needed <= mono.len() {
            let channels = resampler.process(&[&mono[offset..offset + needed]], None)?;
            if let Some(first) = channels.first() {
                out.extend_from_slice(first);
            }
            offset += needed;
        }
        mono = out;
    }
    Ok(mono)
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    #[test]
    fn five_seconds_of_default_silence_then_ten_seconds_between_attempts() {
        let start = Instant::now();
        let mut detection = FallbackDetection {
            last_signal: start,
            last_probe: None,
        };
        assert!(!detection.begin_probe(start + Duration::from_millis(4999), false));
        assert!(detection.begin_probe(start + Duration::from_secs(5), false));
        // Both failed and all-silent probe batches use this same cooldown.
        assert!(!detection.begin_probe(start + Duration::from_millis(14999), false));
        assert!(detection.begin_probe(start + Duration::from_secs(15), false));
        detection.signal(start + Duration::from_secs(24));
        assert!(!detection.begin_probe(start + Duration::from_secs(28), false));
        assert!(detection.begin_probe(start + Duration::from_secs(29), false));
    }
    #[test]
    fn genuine_silence_and_one_window_transients_never_qualify() {
        for levels in [[0.0, 0.0], [0.02, 0.0], [0.0, 0.02], [0.001, 0.001]] {
            let mut meter = ProbeLevel::default();
            for peak in levels {
                meter.peak = peak;
                meter.window();
            }
            assert!(meter.qualified.is_none());
        }
        let mut meter = ProbeLevel::default();
        for peak in [0.02, 0.01] {
            meter.peak = peak;
            meter.window();
        }
        assert_eq!(meter.qualified, Some(-40.0));
    }
    #[test]
    fn communications_then_default_then_loudest_then_id() {
        let candidates = vec![
            ("z".into(), -30.0),
            ("a".into(), -30.0),
            ("regular".into(), -50.0),
            ("comms".into(), -55.0),
        ];
        assert_eq!(
            choose_endpoint(&candidates, Some("comms"), Some("regular")),
            Some("comms")
        );
        assert_eq!(
            choose_endpoint(&candidates, Some("absent"), Some("regular")),
            Some("regular")
        );
        assert_eq!(choose_endpoint(&candidates, None, None), Some("a"));
        assert_eq!(choose_endpoint(&[], None, None), None);
    }
    #[test]
    fn stop_cancels_pending_probe_before_a_window_or_selection() {
        let start = Instant::now();
        let mut detection = FallbackDetection {
            last_signal: start,
            last_probe: None,
        };
        assert!(!detection.begin_probe(start + Duration::from_secs(5), true));
        assert!(detection.last_probe.is_none());
        let mut batch: ProbeBatch = ProbeBatch {
            probes: Vec::new(),
            communications: None,
            default: None,
            window_at: start - Duration::from_secs(2),
            windows: 1,
            finished: false,
        };
        assert!(
            unsafe { batch.poll(&AtomicBool::new(true), &Logger::disabled()) }
                .unwrap()
                .is_none()
        );
        assert!(batch.finished);
        assert_eq!(batch.windows, 1);
    }
    #[test]
    fn disconnect_restarts_silence_detection_without_bypassing_retry_limit() {
        let now = Instant::now();
        let mut detection = FallbackDetection {
            last_signal: now - Duration::from_secs(30),
            last_probe: Some(now),
        };
        detection.reset_silence();
        let reset = detection.last_signal;
        assert!(!detection.begin_probe(reset + Duration::from_secs(4), false));
        assert!(!detection.begin_probe(reset + Duration::from_secs(5), false));
        assert!(detection.begin_probe(reset + Duration::from_secs(10), false));
    }
    struct FakeProbe {
        id: String,
        level: ProbeLevel,
        windows: std::collections::VecDeque<Option<f32>>,
        drops: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Drop for FakeProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl ProbeAudio for FakeProbe {
        fn id(&self) -> &str {
            &self.id
        }
        fn level(&mut self) -> &mut ProbeLevel {
            &mut self.level
        }
        unsafe fn read(&mut self, _: &AtomicBool) -> Result<()> {
            match self.windows.pop_front().flatten() {
                Some(peak) => {
                    self.level.peak = peak;
                    Ok(())
                }
                None => Err(anyhow!("simulated endpoint failure")),
            }
        }
    }
    #[test]
    fn failed_probe_is_discarded_and_verified_candidate_is_retained_without_reopening() {
        let start = Instant::now();
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let make = |id: &str, windows: Vec<Option<f32>>| FakeProbe {
            id: id.into(),
            level: ProbeLevel::default(),
            windows: windows.into(),
            drops: drops.clone(),
        };
        let mut batch = ProbeBatch {
            probes: vec![
                make("failed-comms", vec![None]),
                make("silent-default", vec![Some(0.0), Some(0.0)]),
                make("realtek", vec![Some(0.02), Some(0.02)]),
            ],
            communications: Some("failed-comms".into()),
            default: Some("silent-default".into()),
            window_at: start,
            windows: 0,
            finished: false,
        };
        let stop = AtomicBool::new(false);
        assert!(
            unsafe { batch.poll_at(&stop, &Logger::disabled(), start + Duration::from_secs(1)) }
                .unwrap()
                .is_none()
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        let selected =
            unsafe { batch.poll_at(&stop, &Logger::disabled(), start + Duration::from_secs(2)) }
                .unwrap()
                .unwrap();
        assert_eq!(selected.id(), "realtek");
        drop(batch);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        drop(selected);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }
    #[test]
    fn all_failed_probes_leave_no_candidate_and_stop_drops_unselected_streams() {
        let start = Instant::now();
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut batch = ProbeBatch {
            probes: vec![FakeProbe {
                id: "failed".into(),
                level: ProbeLevel::default(),
                windows: vec![None].into(),
                drops: drops.clone(),
            }],
            communications: None,
            default: None,
            window_at: start,
            windows: 0,
            finished: false,
        };
        let stop = AtomicBool::new(false);
        for second in [1, 2] {
            assert!(
                unsafe {
                    batch.poll_at(
                        &stop,
                        &Logger::disabled(),
                        start + Duration::from_secs(second),
                    )
                }
                .unwrap()
                .is_none()
            );
        }
        assert!(batch.finished);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        batch.probes.push(FakeProbe {
            id: "pending".into(),
            level: ProbeLevel::default(),
            windows: vec![Some(0.02)].into(),
            drops: drops.clone(),
        });
        stop.store(true, Ordering::Relaxed);
        assert!(
            unsafe { batch.poll_at(&stop, &Logger::disabled(), start + Duration::from_secs(3)) }
                .unwrap()
                .is_none()
        );
        assert_eq!(batch.probes[0].windows.len(), 1);
        drop(batch);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn handover_discards_resampler_tail_and_keeps_clock_and_segments_increasing() {
        use std::sync::Mutex;
        let blocks = Arc::new(Mutex::new(Vec::new()));
        let collected = blocks.clone();
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(1000));
        let mut sink = timed_sink(
            Box::new(move |samples, start| {
                collected.lock().unwrap().push((start, samples.to_vec()))
            }),
            clock.clone(),
        );
        let mut old = ResampleStage::new(48000).unwrap();
        old.push(&vec![0.8; 100], &mut sink).unwrap();
        old.clear();
        drop(old);
        let mut current = ResampleStage::new(TARGET_RATE).unwrap();
        current.push(&vec![0.1; 16000], &mut sink).unwrap();
        // A stale packet clock cannot move the sink back across the handover.
        clock.fetch_max(1100, Ordering::Release);
        current.push(&vec![0.0; 24000], &mut sink).unwrap();
        clock.fetch_max(5000, Ordering::Release);
        current.push(&vec![0.2; 16000], &mut sink).unwrap();
        current.push(&vec![0.0; 24000], &mut sink).unwrap();
        let blocks = blocks.lock().unwrap();
        assert_eq!(blocks.len(), 4);
        assert_eq!(
            blocks.iter().map(|(start, _)| *start).collect::<Vec<_>>(),
            [1000, 2000, 5000, 6000]
        );
        assert_eq!(
            blocks.iter().map(|(_, audio)| audio.len()).sum::<usize>(),
            80000
        );
        let mut segmenter = crate::vad::Segmenter::new(crate::vad::SegmenterConfig::default());
        let mut segments = Vec::new();
        for (start, audio) in blocks.iter() {
            segments.extend(segmenter.push_at(audio, start * 16));
        }
        assert_eq!(segments.len(), 2);
        assert!(segments[0].end_sample <= segments[1].start_sample);
        assert!(segments[1].start_sample >= 5000 * 16);
        assert!(segments[0].samples.iter().all(|sample| *sample != 0.8));
    }
}

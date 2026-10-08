//! Routing probe: <seconds> [default|endpoint-id] [wav-path] [--mute-test].
//! --mute-test temporarily mutes speakers and restores their prior mute state.
use depth::{capture, logging::Logger};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn main() -> anyhow::Result<()> {
    for line in capture::output_diagnostics()? {
        println!("{line}");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds = args.first().and_then(|s| s.parse().ok()).unwrap_or(8);
    let device = args
        .get(1)
        .filter(|s| s.as_str() != "default")
        .map(String::as_str);
    let wav = args.get(2).filter(|s| !s.starts_with("--"));
    let _mute = if args.iter().any(|s| s == "--mute-test") {
        Some(mute_speakers()?)
    } else {
        None
    };
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let audio = recorded.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let request = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        request.store(true, Ordering::Release);
    });
    let mut samples = 0usize;
    let mut peak = 0.0f32;
    capture::run_timed_selected(
        Box::new(move |pcm, start| {
            audio.lock().unwrap().extend_from_slice(pcm);
            samples += pcm.len();
            for sample in pcm {
                peak = peak.max(sample.abs());
            }
            if samples % 16000 < pcm.len() {
                println!("audio at {start} ms, {samples} samples, peak {peak:.5}");
            }
        }),
        &stop,
        &Arc::new(Logger::disabled()),
        device,
        Some(Box::new(|health| println!("{health:?}"))),
    )?;
    if let Some(wav) = wav {
        capture::write_wav(std::path::Path::new(wav), &recorded.lock().unwrap())?;
    }
    Ok(())
}

struct RestoreMute(
    windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume,
    bool,
);
impl Drop for RestoreMute {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.SetMute(self.1, std::ptr::null());
        }
        println!("Restored speaker mute to {}", self.1);
    }
}
fn mute_speakers() -> anyhow::Result<RestoreMute> {
    use windows::Win32::{Media::Audio::*, System::Com::*};
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoint: Endpoints::IAudioEndpointVolume = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)?
            .Activate(CLSCTX_ALL, None)?;
        let previous = endpoint.GetMute()?.as_bool();
        endpoint.SetMute(true, std::ptr::null())?;
        println!("Testing with Windows speaker mute TRUE (previous {previous})");
        Ok(RestoreMute(endpoint, previous))
    }
}

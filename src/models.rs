//! Pinned downloadable models shared by the application and asset builder.
use crate::config::{Config, Processing};
use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub const STARTER: &str = "small-id";
pub const TURBO: &str = "turbo";
#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub filename: String,
    pub revision: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub description: String,
    pub license: String,
    pub source: String,
}
pub fn catalog() -> &'static [Model] {
    static MODELS: OnceLock<Vec<Model>> = OnceLock::new();
    MODELS.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/asr-models.json"))
            .expect("validated model catalog")
    })
}
pub fn get(id: &str) -> Result<&'static Model> {
    catalog()
        .iter()
        .find(|m| m.id == id)
        .with_context(|| format!("Unknown model: {id}"))
}
pub fn installed(config: &Config, model: &Model) -> Option<PathBuf> {
    let mut roots = vec![config.base_dir.clone()];
    roots.extend(config.model_search_roots());
    roots
        .into_iter()
        .flat_map(|root| {
            [
                root.join("models").join(&model.filename),
                root.join(&model.filename),
            ]
        })
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == model.size))
}
pub fn cuda_runtime_exists(config: &Config) -> bool {
    if config.whisper_exe != Path::new("whisper-cli.exe") {
        return false;
    }
    config
        .resolve_model(
            Path::new("vendor/whisper/cuda/whisper-cli.exe"),
            "CUDA runtime",
            "",
        )
        .is_ok()
}
pub fn gpu_recommended(
    processing: Processing,
    gpu: Option<&crate::gpu::Gpu>,
    runtime: bool,
) -> bool {
    processing != Processing::Cpu
        && runtime
        && gpu.is_some_and(|g| g.total >= 4 * 1073741824 && g.free >= 2 * 1073741824)
}
pub fn selected(config: &Config, gpu: Option<&crate::gpu::Gpu>, runtime: bool) -> Result<PathBuf> {
    if config.indonesian_processing == Processing::Nvidia && (gpu.is_none() || !runtime) {
        return starter(config);
    }
    match config.indonesian_model.as_str() {
        "custom" => config
            .resolve_model(
                &config.whisper_model,
                "Whisper",
                "Check the custom model path",
            )
            .map_err(|e| anyhow::anyhow!(e.to_string())),
        "recommended" => {
            // Promotion requires the acceptance benchmark to write a hardware-bound qualification.
            if gpu_recommended(config.indonesian_processing, gpu, runtime)
                && qualified(config, gpu)
                && let Some(p) = installed(config, get(TURBO)?)
            {
                return Ok(p);
            }
            starter(config)
        }
        id => installed(config, get(id)?).with_context(|| {
            format!(
                "Model {} is not installed. Download it from Settings.",
                get(id).map(|m| m.name.as_str()).unwrap_or(id)
            )
        }),
    }
}
pub fn starter(config: &Config) -> Result<PathBuf> {
    installed(config,get(STARTER)?).context("Indonesian starter is missing. Download Indonesian Whisper small in Settings or repair the installation.")
}
pub fn save_qualification(
    config: &Config,
    gpu: &crate::gpu::Gpu,
    passed: bool,
    wall: f64,
    audio: f64,
) -> Result<()> {
    let value = serde_json::json!({"gpu":gpu.name,"revision":get(TURBO)?.revision,"runtime":"b5130","passed":passed,"inference_seconds":wall,"audio_seconds":audio});
    crate::recording::atomic_write(
        &config.base_dir.join("turbo-qualified.json"),
        &serde_json::to_vec_pretty(&value)?,
    )?;
    Ok(())
}
pub fn qualified(config: &Config, gpu: Option<&crate::gpu::Gpu>) -> bool {
    let Some(gpu) = gpu else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(config.base_dir.join("turbo-qualified.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    value["gpu"] == gpu.name
        && value["revision"] == get(TURBO).unwrap().revision
        && value["runtime"] == "b5130"
        && value["passed"] == true
}
pub fn verify(
    path: &Path,
    model: &Model,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    let mut input = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        anyhow::ensure!(!cancel.load(Ordering::Acquire), "Download cancelled");
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        count += n as u64;
        progress(count);
    }
    anyhow::ensure!(
        count == model.size,
        "Model size mismatch: expected {}, received {}",
        model.size,
        count
    );
    anyhow::ensure!(
        format!("{:x}", hash.finalize()) == model.sha256,
        "Model checksum mismatch"
    );
    Ok(())
}
#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Progress { id: String, bytes: u64, total: u64 },
    Complete(String),
    Failed { id: String, error: String },
}
pub struct Downloads {
    pub events: crossbeam_channel::Receiver<DownloadEvent>,
    tx: crossbeam_channel::Sender<DownloadEvent>,
    cancel: Option<Arc<AtomicBool>>,
    pub active: Option<String>,
}
impl Default for Downloads {
    fn default() -> Self {
        let (tx, events) = crossbeam_channel::unbounded();
        Self {
            events,
            tx,
            cancel: None,
            active: None,
        }
    }
}
impl Downloads {
    pub fn start(&mut self, config: &Config, id: &str) -> Result<()> {
        anyhow::ensure!(self.active.is_none(), "Another model download is running");
        let model = get(id)?.clone();
        let dir = config.base_dir.join("models");
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let tx = self.tx.clone();
        self.active = Some(id.into());
        self.cancel = Some(cancel);
        std::thread::spawn(move || {
            let result = download(&model, &dir, &flag, |bytes| {
                let _ = tx.send(DownloadEvent::Progress {
                    id: model.id.clone(),
                    bytes,
                    total: model.size,
                });
            });
            let event = match result {
                Ok(()) => DownloadEvent::Complete(model.id),
                Err(e) => DownloadEvent::Failed {
                    id: model.id,
                    error: format!("{e:#}"),
                },
            };
            let _ = tx.send(event);
        });
        Ok(())
    }
    pub fn cancel(&self) {
        if let Some(flag) = &self.cancel {
            flag.store(true, Ordering::Release);
        }
    }
    pub fn finish(&mut self) {
        self.active = None;
        self.cancel = None;
    }
}
impl Drop for Downloads {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn download(
    model: &Model,
    dir: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(&model.filename);
    let temp = dir.join(format!("{}.part", model.filename));
    let result = (|| {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(5))
            .build();
        let response = agent
            .get(&model.url)
            .set("Accept-Encoding", "identity")
            .call()?;
        let reader = response.into_reader();
        download_reader(reader, &temp, model, cancel, progress)?;
        anyhow::ensure!(!cancel.load(Ordering::Acquire), "Download cancelled");
        let licenses = dir.join("licenses");
        std::fs::create_dir_all(&licenses)?;
        let license = match model.license.as_str() {
            "Apache-2.0" => include_str!("../assets/licenses/Apache-2.0.txt"),
            "MIT" => include_str!("../assets/licenses/Whisper-MIT.txt"),
            other => anyhow::bail!("Missing license text: {other}"),
        };
        crate::recording::atomic_write(
            &licenses.join(format!("{}.txt", model.license)),
            license.as_bytes(),
        )?;
        let attribution = format!(
            "{}\nSource: {}\nRevision: {}\nLicense: {}\n",
            model.name, model.source, model.revision, model.license
        );
        crate::recording::atomic_write(
            &licenses.join(format!("{}-NOTICE.txt", model.id)),
            attribution.as_bytes(),
        )?;
        // Publish only verified checkpoint bytes; failures retain the prior installed copy.
        anyhow::ensure!(!cancel.load(Ordering::Acquire), "Download cancelled");
        crate::recording::replace_file(&temp, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
fn download_reader(
    mut reader: impl Read,
    temp: &Path,
    model: &Model,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<()> {
    let mut out = std::fs::File::create(temp)?;
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut buffer = [0u8; 65536];
    let mut last = std::time::Instant::now();
    loop {
        anyhow::ensure!(!cancel.load(Ordering::Acquire), "Download cancelled");
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        anyhow::ensure!(count <= model.size, "Model exceeds expected download size");
        out.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        if last.elapsed() >= Duration::from_millis(100) {
            progress(count);
            last = std::time::Instant::now();
        }
    }
    anyhow::ensure!(count == model.size, "Incomplete model download");
    anyhow::ensure!(
        format!("{:x}", hash.finalize()) == model.sha256,
        "Model checksum mismatch"
    );
    out.sync_all()?;
    progress(count);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_is_pinned_and_unique() {
        let mut ids = std::collections::BTreeSet::new();
        for m in catalog() {
            assert!(ids.insert(&m.id));
            assert_eq!(m.sha256.len(), 64);
            assert_eq!(m.revision.len(), 40);
            assert!(m.url.contains(&m.revision));
            assert!(m.size > 0);
            assert!(m.id.parse::<crate::config::ModelSelection>().is_ok());
        }
    }
    #[test]
    fn custom_runner_does_not_offer_bundled_cuda() {
        let config = Config {
            whisper_exe: PathBuf::from("custom-whisper.exe"),
            ..Default::default()
        };
        assert!(!cuda_runtime_exists(&config));
    }
    #[test]
    fn recommendation_requires_memory_and_gpu_mode() {
        let gpu = crate::gpu::Gpu {
            index: 0,
            name: "test".into(),
            total: 6 * 1073741824,
            free: 4 * 1073741824,
        };
        assert!(gpu_recommended(Processing::Auto, Some(&gpu), true));
        assert!(!gpu_recommended(Processing::Cpu, Some(&gpu), true));
        assert!(!gpu_recommended(Processing::Auto, None, true));
        assert!(!gpu_recommended(Processing::Auto, Some(&gpu), false));
        let occupied = crate::gpu::Gpu {
            free: 1073741824,
            ..gpu.clone()
        };
        assert!(!gpu_recommended(Processing::Auto, Some(&occupied), true));
        let tiny = crate::gpu::Gpu {
            total: 2 * 1073741824,
            ..gpu
        };
        assert!(!gpu_recommended(Processing::Auto, Some(&tiny), true));
    }
    #[test]
    fn downloads_check_bytes_checksum_and_cancel() {
        let dir = std::env::temp_dir().join(format!("depth-download-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.part");
        let mut model = get(STARTER).unwrap().clone();
        model.size = 3;
        model.sha256 = format!("{:x}", Sha256::digest(b"abc"));
        let flag = AtomicBool::new(false);
        assert!(download_reader(&b"abc"[..], &path, &model, &flag, |_| {}).is_ok());
        assert!(download_reader(&b"ab"[..], &path, &model, &flag, |_| {}).is_err());
        assert!(download_reader(&b"abd"[..], &path, &model, &flag, |_| {}).is_err());
        flag.store(true, Ordering::Release);
        assert!(download_reader(&b"abc"[..], &path, &model, &flag, |_| {}).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn selection_preserves_explicit_choices_and_requires_qualification() {
        let dir =
            std::env::temp_dir().join(format!("depth-model-selection-{}", std::process::id()));
        let mut config = Config {
            base_dir: dir.clone(),
            ..Default::default()
        };
        std::fs::create_dir_all(dir.join("models")).unwrap();
        for id in [STARTER, TURBO] {
            let m = get(id).unwrap();
            std::fs::File::create(dir.join("models").join(&m.filename))
                .unwrap()
                .set_len(m.size)
                .unwrap();
        }
        let gpu = crate::gpu::Gpu {
            index: 0,
            name: "fixture GPU".into(),
            total: 6 * 1073741824,
            free: 4 * 1073741824,
        };
        let name = |p: PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            name(selected(&config, None, false).unwrap()),
            get(STARTER).unwrap().filename
        );
        assert_eq!(
            name(selected(&config, Some(&gpu), true).unwrap()),
            get(STARTER).unwrap().filename
        );
        save_qualification(&config, &gpu, true, 15., 30.).unwrap();
        assert_eq!(
            name(selected(&config, Some(&gpu), true).unwrap()),
            get(TURBO).unwrap().filename
        );
        config.indonesian_processing = Processing::Cpu;
        assert_eq!(
            name(selected(&config, Some(&gpu), true).unwrap()),
            get(STARTER).unwrap().filename
        );
        config.indonesian_model = crate::config::ModelSelection::Turbo;
        assert_eq!(
            name(selected(&config, None, false).unwrap()),
            get(TURBO).unwrap().filename
        );
        config.indonesian_processing = Processing::Nvidia;
        assert_eq!(
            name(selected(&config, None, false).unwrap()),
            get(STARTER).unwrap().filename
        );
        config.indonesian_processing = Processing::Cpu;
        config.indonesian_model = crate::config::ModelSelection::Custom;
        config.whisper_model = dir.join("external.bin");
        std::fs::write(&config.whisper_model, b"custom").unwrap();
        assert_eq!(
            selected(&config, None, false).unwrap(),
            config.whisper_model
        );
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn download_publication_survives_failure_and_restart() {
        use std::net::TcpListener;
        fn response(model: &mut Model, body: Vec<u8>) -> std::thread::JoinHandle<()> {
            let server = TcpListener::bind("127.0.0.1:0").unwrap();
            model.url = format!("http://{}/fixture", server.local_addr().unwrap());
            std::thread::spawn(move || {
                let (mut socket, _) = server.accept().unwrap();
                let mut request = [0; 4096];
                let _ = socket.read(&mut request);
                socket.write_all(&body).unwrap();
            })
        }
        let dir =
            std::env::temp_dir().join(format!("depth-download-restart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut model = get(STARTER).unwrap().clone();
        model.filename = "fixture.bin".into();
        model.size = 3;
        model.sha256 = format!("{:x}", Sha256::digest(b"abc"));
        let target = dir.join(&model.filename);
        let partial = dir.join("fixture.bin.part");
        std::fs::write(&partial, b"interrupted prior run").unwrap();
        let flag = AtomicBool::new(false);
        let serve = response(
            &mut model,
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".to_vec(),
        );
        download(&model, &dir, &flag, |_| {}).unwrap();
        serve.join().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"abc");
        assert!(!partial.exists());
        for body in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nxyz".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\na".to_vec(),
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
        ] {
            let serve = response(&mut model, body);
            assert!(download(&model, &dir, &flag, |_| {}).is_err());
            serve.join().unwrap();
            assert_eq!(std::fs::read(&target).unwrap(), b"abc");
            assert!(!partial.exists());
        }
        flag.store(true, Ordering::Release);
        let serve = response(
            &mut model,
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".to_vec(),
        );
        assert!(download(&model, &dir, &flag, |_| {}).is_err());
        serve.join().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"abc");
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    #[ignore = "requires internet access; downloads the pinned base checkpoint"]
    fn https_background_download_cancel_retry_and_verify() {
        let config = Config {
            base_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".scratch/model-download-test"),
            ..Default::default()
        };
        let mut downloads = Downloads::default();
        downloads.start(&config, "base").unwrap();
        let mut cancelled = false;
        loop {
            match downloads
                .events
                .recv_timeout(Duration::from_secs(90))
                .unwrap()
            {
                DownloadEvent::Progress { .. } if !cancelled => {
                    downloads.cancel();
                    cancelled = true;
                }
                DownloadEvent::Failed { error, .. } => {
                    assert!(error.contains("cancelled"), "{error}");
                    break;
                }
                DownloadEvent::Complete(_) => panic!("download finished before cancellation"),
                _ => {}
            }
        }
        downloads.finish();
        downloads.start(&config, "base").unwrap();
        loop {
            match downloads
                .events
                .recv_timeout(Duration::from_secs(90))
                .unwrap()
            {
                DownloadEvent::Complete(id) => {
                    assert_eq!(id, "base");
                    break;
                }
                DownloadEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        downloads.finish();
        let model = get("base").unwrap();
        let path = config.base_dir.join("models").join(&model.filename);
        verify(&path, model, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(
            !path
                .with_file_name(format!("{}.part", model.filename))
                .exists()
        );
        assert!(
            config
                .base_dir
                .join("models/licenses/base-NOTICE.txt")
                .exists()
        );
        assert_eq!(
            config.indonesian_model,
            crate::config::ModelSelection::Recommended
        );
    }
}

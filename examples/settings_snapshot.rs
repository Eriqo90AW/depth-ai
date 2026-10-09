//! Headless Settings layout verification for model downloads.
use depth::gui::{SettingsWindow, Theme};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::io::Write;
fn main() -> anyhow::Result<()> {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))?;
    let panel = SettingsWindow::new()?;
    panel.global::<Theme>().set_dark(false);
    panel.global::<Theme>().set_backdrop_supported(false);
    panel.set_language(1);
    panel.set_gpu_status("NVIDIA GeForce RTX 2060 (6.0 GiB VRAM)".into());
    panel.set_inference_status("Active: NVIDIA GeForce RTX 2060 · Whisper large-v3-turbo".into());
    panel.set_asr_models(ModelRc::new(VecModel::from(vec![
        "Recommended for this device".into(),
        "Indonesian Whisper small · Installed · 252 MiB".into(),
        "Whisper large-v3-turbo · Download · 547 MiB".into(),
    ])));
    panel.set_model_details("Uses Indonesian Whisper small until turbo passes the device speed check. Lowercase output without punctuation.".into());
    panel.set_downloading(true);
    panel.set_download_status("Downloading turbo: 52% (284/547 MiB)".into());
    panel.set_output_devices(ModelRc::new(VecModel::from(vec!["Windows default".into()])));
    panel.show()?;
    std::fs::create_dir_all("target/settings-snapshots")?;
    for (name, w, h) in [("desktop", 690., 1040.), ("compact", 560., 760.)] {
        panel.window().set_size(slint::LogicalSize::new(w, h));
        let image = panel.window().take_snapshot()?;
        let mut out = std::fs::File::create(format!("target/settings-snapshots/{name}.ppm"))?;
        write!(out, "P6\n{} {}\n255\n", image.width(), image.height())?;
        for p in image.as_slice() {
            out.write_all(&[p.r, p.g, p.b])?;
        }
    }
    Ok(())
}

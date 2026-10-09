//! Render speaker management and a labeled transcript at desktop and compact sizes.
use depth::gui::{MainWindow, SpeakerRow, SpeakersWindow, Theme};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::io::Write;
fn save(window: &slint::Window, path: &str) -> anyhow::Result<()> {
    let image = window.take_snapshot()?;
    let mut f = std::fs::File::create(path)?;
    write!(f, "P6\n{} {}\n255\n", image.width(), image.height())?;
    for p in image.as_slice() {
        f.write_all(&[p.r, p.g, p.b])?;
    }
    Ok(())
}
fn main() -> anyhow::Result<()> {
    std::fs::create_dir_all("target/speaker-snapshots")?;
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))?;
    let ui = MainWindow::new()?;
    ui.global::<Theme>().set_dark(true);
    ui.global::<Theme>().set_backdrop_supported(false);
    ui.set_selected_id("speaker-preview".into());
    ui.set_recording_title("Speaker labels".into());
    ui.set_detail("English · Saved on this computer".into());
    ui.set_has_speakers(true);
    ui.set_can_delete(true);
    ui.set_hotkey("Ctrl+Alt+Space".into());
    ui.invoke_replace_document("[00:00:04] Sarah: Shall we start?\n\n[00:00:07] Budi: Yes, let's begin.\n\n[00:00:12] Sarah: We can review the names after recording.".into(),true);
    ui.show()?;
    for (name, w, h) in [("wide", 1180., 780.), ("compact", 800., 600.)] {
        ui.window().set_size(slint::LogicalSize::new(w, h));
        ui.set_compact(w < 1000.);
        save(ui.window(), &format!("target/speaker-snapshots/{name}.ppm"))?;
    }
    let panel = SpeakersWindow::new()?;
    panel.global::<Theme>().set_dark(true);
    panel.set_editing_enabled(true);
    panel.set_status("Rename a speaker to update all their turns. Merge duplicate groups or correct a turn below.".into());
    panel.set_speakers(ModelRc::new(VecModel::from(vec![
        SpeakerRow {
            id: "1".into(),
            name: "Sarah".into(),
            sample: "Shall we start?\nWe can review the names after recording.".into(),
        },
        SpeakerRow {
            id: "2".into(),
            name: "Budi".into(),
            sample: "Yes, let's begin.".into(),
        },
    ])));
    panel.set_names(ModelRc::new(VecModel::from(vec![
        "Sarah".into(),
        "Budi".into(),
    ])));
    panel.set_turn_choices(ModelRc::new(VecModel::from(vec![
        "00:00:04 Sarah: Shall we start?".into(),
        "00:00:07 Budi: Yes, let's begin.".into(),
    ])));
    panel.set_assignment_choices(ModelRc::new(VecModel::from(vec![
        "Unknown speaker".into(),
        "Sarah".into(),
        "Budi".into(),
    ])));
    panel.show()?;
    save(panel.window(), "target/speaker-snapshots/panel.ppm")?;
    Ok(())
}

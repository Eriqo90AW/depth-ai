//! Headless transcript editing layout checks at desktop and minimum window sizes.
use depth::gui::{EditorRow, MainWindow, Theme, TranscriptEditorWindow};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::io::Write;
fn main() -> anyhow::Result<()> {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))?;
    let panel = TranscriptEditorWindow::new()?;
    panel.set_recording_title("Rapat tim · Indonesian".into());
    panel.set_passages(ModelRc::new(VecModel::from(vec![
        EditorRow { label:"[00:00:00] Maya".into(), text:"Selamat pagi semuanya. Hari ini kita membahas jadwal peluncuran aplikasi dan masukan dari pengguna.".into() },
        EditorRow { label:"[00:00:06] Arif".into(), text:"Terima kasih, Maya. Saya sudah memperbarui daftar tugas dan hasil pengujian. Fitur koreksi transkrip siap diperiksa.".into() },
        EditorRow { label:"[00:00:12] Maya".into(), text:"Baik, mari kita mulai.".into() },
    ])));
    panel.set_dirty(true);
    panel.show()?;
    std::fs::create_dir_all("target/editor-snapshots")?;
    for (name, w, h, dark, confirm) in [
        ("desktop", 760., 720., false, false),
        ("compact", 500., 480., true, true),
    ] {
        panel.global::<Theme>().set_dark(dark);
        panel.set_discard_prompt(confirm);
        panel.window().set_size(slint::LogicalSize::new(w, h));
        let image = panel.window().take_snapshot()?;
        let mut out = std::fs::File::create(format!("target/editor-snapshots/{name}.ppm"))?;
        write!(out, "P6\n{} {}\n255\n", image.width(), image.height())?;
        for p in image.as_slice() {
            out.write_all(&[p.r, p.g, p.b])?;
        }
    }
    let ui = MainWindow::new()?;
    ui.global::<Theme>().set_dark(true);
    ui.set_selected_id("finished".into());
    ui.set_recording_title("Indonesian meeting notes".into());
    ui.set_detail("Indonesian · Completed".into());
    ui.set_can_edit(true);
    ui.set_can_delete(true);
    ui.set_can_continue(true);
    ui.set_compact(true);
    ui.set_sidebar_open(true);
    ui.invoke_replace_document("Ini adalah teks akhir yang sudah disimpan.\n\nParagraf berikutnya muncul setelah transkripsi selesai dan dapat dikoreksi oleh pengguna.".into(), true);
    ui.window().set_size(slint::LogicalSize::new(800., 600.));
    ui.show()?;
    // Allow a second frame after virtualized layout updates before capturing the settled view.
    let _ = ui.window().take_snapshot()?;
    let image = ui.window().take_snapshot()?;
    let mut out = std::fs::File::create("target/editor-snapshots/compact-library.ppm")?;
    write!(out, "P6\n{} {}\n255\n", image.width(), image.height())?;
    for p in image.as_slice() {
        out.write_all(&[p.r, p.g, p.b])?;
    }
    Ok(())
}

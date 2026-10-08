//! Render the live GUI with the same Slint components, without starting capture.
use depth::gui::{MainWindow, Theme};
use slint::ComponentHandle;
use std::io::Write;
fn main() -> anyhow::Result<()> {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))?;
    let ui = MainWindow::new()?;
    ui.global::<Theme>().set_dark(true);
    ui.global::<Theme>().set_backdrop_supported(false);
    ui.set_selected_id("verification".into());
    ui.set_recording_title("Indonesian YouTube verification".into());
    ui.set_detail("Indonesian · Saved on this computer".into());
    ui.set_language(1);
    ui.set_hotkey("Ctrl+Alt+Space".into());
    ui.set_recording(true);
    ui.set_state("Recording desktop audio".into());
    ui.set_elapsed("00:12".into());
    ui.set_capture_source("Desktop audio before speaker mute · Speaker (Realtek(R) Audio)".into());
    ui.invoke_replace_document("Ini adalah teks akhir yang sudah disimpan.\n\nTeks ini tetap bisa dipilih saat draf diperbarui.\n\nParagraf berikutnya muncul setelah transkripsi selesai.".into(), true);
    ui.invoke_select_match(0, 13);
    ui.set_live_draft("Jadi, kita bisa melihat draf secara langsung saat pembicara masih berbicara. Hasil ini bisa berubah.".into());
    ui.show()?;
    for (name, width, height) in [("wide", 1180., 780.), ("compact", 800., 600.)] {
        ui.window().set_size(slint::LogicalSize::new(width, height));
        ui.set_compact(width < 1000.);
        let pixels = ui.window().take_snapshot()?;
        let path = format!(".scratch/live-gui-{name}.ppm");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
        for p in pixels.as_slice() {
            file.write_all(&[p.r, p.g, p.b])?;
        }
        println!("Rendered {path}");
    }
    ui.set_recording(false);
    ui.set_state("Ready to record".into());
    ui.set_live_draft("".into());
    ui.set_capture_source("".into());
    for (name, width, height) in [("wide", 1180., 780.), ("compact", 800., 600.)] {
        ui.window().set_size(slint::LogicalSize::new(width, height));
        ui.set_compact(width < 1000.);
        let pixels = ui.window().take_snapshot()?;
        let path = format!(".scratch/ready-gui-{name}.ppm");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
        for p in pixels.as_slice() {
            file.write_all(&[p.r, p.g, p.b])?;
        }
        println!("Rendered {path}");
    }
    ui.set_can_continue(true);
    ui.set_can_delete(true);
    ui.set_state("Ready to record".into());
    ui.set_live_draft("".into());
    ui.set_capture_source("".into());
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    for (name, width, height, sidebar) in [
        ("wide", 1180., 780., false),
        ("compact", 800., 600., false),
        ("library", 800., 600., true),
    ] {
        ui.window().set_size(slint::LogicalSize::new(width, height));
        ui.set_compact(width < 1000.);
        ui.set_sidebar_open(sidebar);
        let pixels = ui.window().take_snapshot()?;
        let path = format!(".scratch/continue-gui-{name}.ppm");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
        for p in pixels.as_slice() {
            file.write_all(&[p.r, p.g, p.b])?;
        }
        println!("Rendered {path}");
    }
    ui.set_delete_prompt(true);
    let pixels = ui.window().take_snapshot()?;
    let path = ".scratch/delete-gui-confirm.ppm";
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
    for p in pixels.as_slice() {
        file.write_all(&[p.r, p.g, p.b])?;
    }
    println!("Rendered {path}");
    Ok(())
}

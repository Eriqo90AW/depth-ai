//! Render the compact overlay without starting audio capture.
use depth::gui::{OverlayWindow, Theme};
use slint::ComponentHandle;
use std::io::Write;

fn main() -> anyhow::Result<()> {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))?;
    let ui = OverlayWindow::new()?;
    ui.window().set_size(slint::LogicalSize::new(156.0, 48.0));
    ui.set_state("Recording desktop audio".into());
    ui.set_elapsed("00:12".into());
    ui.show()?;
    std::fs::create_dir_all(".scratch")?;
    for (name, dark, finishing) in [
        ("light", false, false),
        ("dark", true, false),
        ("finishing", true, true),
    ] {
        ui.global::<Theme>().set_dark(dark);
        ui.set_finishing(finishing);
        let pixels = ui.window().take_snapshot()?;
        let background_alpha = pixels.as_slice()[43 * pixels.width() as usize + 78].a;
        assert!(
            (191..=192).contains(&background_alpha),
            "overlay background should have 75% alpha"
        );
        std::fs::write(format!(".scratch/overlay-{name}.rgba"), pixels.as_bytes())?;
        let path = format!(".scratch/overlay-{name}.ppm");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
        write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
        for p in pixels.as_slice() {
            file.write_all(&[p.r, p.g, p.b])?;
        }
        let before = pixels.as_slice().to_vec();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(150));
        let after = ui.window().take_snapshot()?;
        assert_ne!(before, after.as_slice(), "waveform should animate");
        println!("Rendered {path}");
    }
    ui.global::<Theme>().set_reduced_motion(true);
    let before = ui.window().take_snapshot()?;
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(150));
    let after = ui.window().take_snapshot()?;
    assert_eq!(
        before.as_slice(),
        after.as_slice(),
        "reduced motion should freeze waveform"
    );
    Ok(())
}

//! Diagnose recording switching without capture or changing saved recordings.
use depth::{
    gui::{LibraryRow, MainWindow, Theme},
    recording,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{io::Write, path::PathBuf, time::Instant};
fn elapsed(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--native") {
        return native_profile();
    }
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            ..Default::default()
        },
    )))?;
    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".scratch/dev/transcripts"));
    let start = Instant::now();
    let records = recording::library(&dir)?;
    println!(
        "library: {:.3} ms, {} recordings",
        elapsed(start),
        records.len()
    );
    anyhow::ensure!(!records.is_empty(), "No recordings to profile");
    let ui = MainWindow::new()?;
    ui.global::<Theme>().set_dark(true);
    ui.window().set_size(slint::LogicalSize::new(1180., 780.));
    ui.set_recordings(ModelRc::new(VecModel::from(
        records
            .iter()
            .map(|r| LibraryRow {
                id: r.id.clone().into(),
                title: r.title.clone().into(),
                detail: r.label().into(),
                preview: r.preview.clone().into(),
                state: r.label().into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.show()?;
    ui.window().take_snapshot()?;
    println!("pass,id,segments,bytes,load_ms,text_ms,properties_ms,replace_ms,render_ms,total_ms");
    for pass in 0..8 {
        for meta in &records {
            let total = Instant::now();
            let start = Instant::now();
            let r = recording::load_recording(meta)?;
            let load = elapsed(start);
            let start = Instant::now();
            let text = r.text(false);
            let bytes = text.len();
            let format = elapsed(start);
            let start = Instant::now();
            ui.set_selected_id(r.id.clone().into());
            ui.set_recording_title(r.title.clone().into());
            ui.set_detail(r.label().into());
            ui.set_can_continue(r.can_continue());
            ui.set_can_delete(r.can_delete());
            ui.set_warning(r.warnings.join("\n").into());
            let props = elapsed(start);
            let start = Instant::now();
            ui.invoke_replace_document(text.into(), true);
            let replace = elapsed(start);
            let start = Instant::now();
            let pixels = ui.window().take_snapshot()?;
            let render = elapsed(start);
            println!(
                "{pass},{},{},{bytes},{load:.3},{format:.3},{props:.3},{replace:.3},{render:.3},{:.3}",
                r.id,
                r.segments.len(),
                elapsed(total)
            );
            if pass == 0 {
                let mut file = std::io::BufWriter::new(std::fs::File::create(format!(
                    ".scratch/switch-profile-{}.ppm",
                    r.id
                ))?);
                write!(file, "P6\n{} {}\n255\n", pixels.width(), pixels.height())?;
                for p in pixels.as_slice() {
                    file.write_all(&[p.r, p.g, p.b])?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct Measurement {
    id: String,
    segments: usize,
    bytes: usize,
    load: f64,
    format: f64,
    props: f64,
    replace: f64,
    started: Option<Instant>,
}
fn native_profile() -> anyhow::Result<()> {
    use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
    use slint::{Timer, TimerMode};
    use std::{cell::RefCell, rc::Rc, time::Duration};
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia".into())
        .select()?;
    let dir = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".scratch/dev/transcripts"));
    let records = Rc::new(recording::library(&dir)?);
    anyhow::ensure!(!records.is_empty(), "No recordings to profile");
    let ui = MainWindow::new()?;
    ui.set_recordings(ModelRc::new(VecModel::from(
        records
            .iter()
            .map(|r| LibraryRow {
                id: r.id.clone().into(),
                title: r.title.clone().into(),
                detail: r.label().into(),
                preview: r.preview.clone().into(),
                state: r.label().into(),
            })
            .collect::<Vec<_>>(),
    )));
    ui.window().set_size(slint::LogicalSize::new(1180., 780.));
    let pending = Rc::new(RefCell::new(Measurement::default()));
    let count = Rc::new(std::cell::Cell::new(0));
    let target = records.len() * 4;
    let observed = pending.clone();
    let finished = count.clone();
    // A zero-delay timer after RedrawRequested runs after the native frame has painted.
    // Unlike graphics notifiers, this also works with Skia's software fallback.
    ui.window().on_winit_window_event(move |_, event| {
        if matches!(event, WindowEvent::RedrawRequested) {
            let mut m = observed.borrow_mut();
            if m.started.is_some() {
                let m = std::mem::take(&mut *m);
                let finished = finished.clone();
                Timer::single_shot(Duration::ZERO, move || {
                    let total = elapsed(m.started.unwrap());
                    let render = total - m.load - m.format - m.props - m.replace;
                    println!(
                        "native,{},{},{},{:.3},{:.3},{:.3},{:.3},{render:.3},{total:.3}",
                        m.id, m.segments, m.bytes, m.load, m.format, m.props, m.replace
                    );
                    finished.set(finished.get() + 1);
                    if finished.get() == target {
                        let _ = slint::quit_event_loop();
                    }
                });
            }
        }
        EventResult::Propagate
    });
    Timer::single_shot(Duration::from_secs(20), || {
        eprintln!("Native profiler reached its 20-second limit");
        let _ = slint::quit_event_loop();
    });
    println!("mode,id,segments,bytes,load_ms,text_ms,properties_ms,replace_ms,render_ms,total_ms");
    let weak = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        if pending.borrow().started.is_some() {
            return;
        }
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let total = Instant::now();
        let start = Instant::now();
        let r = recording::load_recording(&records[count.get() % records.len()]).unwrap();
        let load = elapsed(start);
        let start = Instant::now();
        let text = r.text(false);
        let bytes = text.len();
        let format = elapsed(start);
        let start = Instant::now();
        ui.set_selected_id(r.id.clone().into());
        ui.set_recording_title(r.title.clone().into());
        ui.set_detail(r.label().into());
        ui.set_can_continue(r.can_continue());
        ui.set_can_delete(r.can_delete());
        ui.set_warning(r.warnings.join("\n").into());
        let props = elapsed(start);
        let start = Instant::now();
        ui.invoke_replace_document(text.into(), true);
        let replace = elapsed(start);
        *pending.borrow_mut() = Measurement {
            id: r.id,
            segments: r.segments.len(),
            bytes,
            load,
            format,
            props,
            replace,
            started: Some(total),
        };
    });
    let appearance_timer = Timer::default();
    if std::env::args().any(|a| a == "--theme-timer") {
        let weak = ui.as_weak();
        let config = depth::config::Config::default();
        appearance_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let start = Instant::now();
            ui.global::<Theme>()
                .set_dark(depth::appearance::dark(config.viewer_theme));
            ui.global::<Theme>()
                .set_reduced_motion(config.reduced_motion);
            ui.global::<Theme>()
                .set_reduced_transparency(config.reduced_transparency);
            let supported = depth::appearance::appearance(ui.window(), &config);
            ui.global::<Theme>().set_backdrop_supported(supported);
            println!("native_appearance_ms,{:.3}", elapsed(start));
        });
    }
    ui.show()?;
    slint::run_event_loop_until_quit()?;
    timer.stop();
    appearance_timer.stop();
    Ok(())
}

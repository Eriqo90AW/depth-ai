//! Slint windows and tray actions share this application controller.
use crate::{
    config::{Config, IndicatorPosition, Language, ViewerTheme},
    hotkey,
    logging::Logger,
    pipeline::{self, Control, PipelineHandle},
    recording::{self, PipelineEvent, Recording, RecordingStatus, TranscriptSegment},
};
use anyhow::Result;
use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
slint::include_modules!();
struct App {
    config: Config,
    pipeline: Option<PipelineHandle>,
    keys: Option<hotkey::Hotkey>,
    library: Vec<Recording>,
    selected: Option<Recording>,
    quitting: bool,
    started: Option<Instant>,
    started_duration_ms: u64,
    search_cursor: usize,
    search_query: String,
    completion_id: String,
    notification_until: Option<Instant>,
}
fn sync_live(ui: &MainWindow, live: Option<&crate::live::LiveState>, active: bool) {
    ui.set_capture_source(
        live.filter(|_| active)
            .map(|s| s.source.clone())
            .unwrap_or_default()
            .into(),
    );
    if let Some(live) = live.filter(|s| active && s.recording_id == ui.get_selected_id().as_str()) {
        ui.set_live_draft(live.draft_text().into());
        ui.set_capture_guidance(live.guidance_for(!ui.get_finishing()).into());
    } else {
        ui.set_live_draft("".into());
        ui.set_capture_guidance("".into());
    }
}
fn detail(r: &Recording) -> String {
    format!(
        "{} · {} · {}{}",
        r.started.format("%b %d, %Y %H:%M"),
        recording::timestamp(r.duration_ms),
        r.language.label(),
        if !r.autosave {
            " · Unsaved"
        } else if r.legacy_session.is_some() {
            " · Legacy clock timestamps"
        } else {
            ""
        }
    )
}
fn theme<H: ComponentHandle>(h: &H, c: &Config)
where
    for<'a> Theme<'a>: slint::Global<'a, H>,
{
    let t = h.global::<Theme>();
    t.set_dark(crate::appearance::dark(c.viewer_theme));
    t.set_reduced_motion(c.reduced_motion);
    t.set_reduced_transparency(c.reduced_transparency);
}
fn sync_theme(
    ui: &MainWindow,
    settings: &SettingsWindow,
    overlay: &OverlayWindow,
    note: &NotificationWindow,
    c: &Config,
) {
    theme(ui, c);
    theme(settings, c);
    theme(overlay, c);
    theme(note, c);
    if crate::appearance::hwnd(ui.window()).is_some() {
        ui.global::<Theme>()
            .set_backdrop_supported(crate::appearance::appearance(ui.window(), c));
    }
    if crate::appearance::hwnd(settings.window()).is_some() {
        settings
            .global::<Theme>()
            .set_backdrop_supported(crate::appearance::appearance(settings.window(), c));
    }
}
fn fill_library(ui: &MainWindow, app: &App) {
    let rows = app
        .library
        .iter()
        .map(|r| LibraryRow {
            id: r.id.clone().into(),
            title: r.title.clone().into(),
            detail: detail(r).into(),
            preview: r.preview.clone().into(),
            state: r.label().into(),
        })
        .collect::<Vec<_>>();
    ui.set_recordings(ModelRc::new(VecModel::from(rows)));
}
fn select(ui: &MainWindow, app: &mut App, id: &str, reset: bool) -> Result<()> {
    let (r, readable) = if let Some(r) = app.pipeline.as_ref().and_then(|p| p.recording(id)) {
        (r, true)
    } else {
        let meta = app
            .library
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("Recording not found"))?;
        match recording::load_recording(meta) {
            Ok(r) => (r, true),
            Err(error) => {
                let mut r = meta.clone();
                r.status = RecordingStatus::Incomplete;
                r.warnings.push(format!("Cannot read recording: {error}"));
                (r, false)
            }
        }
    };
    ui.set_can_continue(readable && r.can_continue());
    ui.set_can_delete(r.can_delete());
    if !r.can_delete() {
        ui.set_delete_prompt(false);
    }
    ui.set_legacy_recording(r.legacy_session.is_some());
    ui.set_selected_id(r.id.clone().into());
    ui.set_recording_title(r.title.clone().into());
    ui.set_detail(detail(&r).into());
    ui.set_warning(r.warnings.join("\n").into());
    let doc = r.text(ui.get_timed());
    ui.invoke_replace_document(doc.into(), reset);
    if reset {
        ui.set_delete_prompt(false);
        ui.set_query("".into());
        ui.set_matches("".into());
        app.search_query.clear();
        app.search_cursor = 0;
        ui.set_exporting(false);
        ui.set_export_path(
            app.config
                .base_dir
                .join(format!("{}.md", r.id))
                .display()
                .to_string()
                .into(),
        );
    }
    app.selected = Some(r);
    Ok(())
}
fn forget_recording(ui: &MainWindow, app: &mut App, id: &str) {
    app.library.retain(|r| r.id != id);
    if ui.get_selected_id().as_str() == id {
        app.selected = None;
        app.search_query.clear();
        app.search_cursor = 0;
        ui.set_selected_id("".into());
        ui.set_recording_title("".into());
        ui.set_detail("".into());
        ui.set_warning("".into());
        ui.set_live_draft("".into());
        ui.set_can_continue(false);
        ui.set_can_delete(false);
        ui.set_legacy_recording(false);
        ui.set_delete_prompt(false);
        ui.set_query("".into());
        ui.set_matches("".into());
        ui.set_exporting(false);
        ui.set_export_path("".into());
        ui.set_elapsed("00:00:00".into());
        ui.invoke_replace_document("".into(), true);
        if let Some(next) = app.library.first().map(|r| r.id.clone()) {
            if let Err(error) = select(ui, app, &next, true) {
                ui.set_warning(error.to_string().into());
            }
        }
    }
    fill_library(ui, app);
}
fn begin_quit(ui: &MainWindow, app: &mut App, confirmed: bool) {
    let unsaved = app
        .pipeline
        .as_ref()
        .is_some_and(|p| p.recordings().iter().any(|r| !r.autosave && !r.exported));
    if unsaved && !confirmed {
        ui.set_quit_prompt(true);
        let _ = ui.show();
        return;
    }
    ui.set_quit_prompt(false);
    app.quitting = true;
    if let Some(p) = &app.pipeline {
        p.request_stop();
    } else {
        let _ = slint::quit_event_loop();
    }
}
fn settings_values(s: &SettingsWindow, app: &App) {
    let c = &app.config;
    let mut devices = crate::capture::output_devices().unwrap_or_default();
    if let Some(id) = &c.audio_output_device {
        if !devices.iter().any(|d| &d.id == id) {
            devices.push(crate::capture::OutputDevice {
                id: id.clone(),
                name: "Selected output is unavailable".into(),
            });
        }
    }
    let mut names = vec![slint::SharedString::from("Windows default")];
    let mut ids = vec![slint::SharedString::from("")];
    for d in &devices {
        names.push(d.name.clone().into());
        ids.push(d.id.clone().into());
    }
    s.set_output_device(
        c.audio_output_device
            .as_ref()
            .and_then(|id| devices.iter().position(|d| &d.id == id))
            .map(|i| i as i32 + 1)
            .unwrap_or(0),
    );
    s.set_output_devices(ModelRc::new(VecModel::from(names)));
    s.set_output_device_ids(ModelRc::new(VecModel::from(ids)));
    s.set_language(if c.language == Language::En { 0 } else { 1 });
    s.set_theme(match c.viewer_theme {
        ViewerTheme::System => 0,
        ViewerTheme::Light => 1,
        ViewerTheme::Dark => 2,
    });
    s.set_autosave(c.save_transcript);
    s.set_startup(c.start_listening);
    s.set_overlay(c.show_indicator);
    s.set_notifications(c.show_result_popup);
    s.set_reduced_motion(c.reduced_motion);
    s.set_reduced_transparency(c.reduced_transparency);
    s.set_hotkey(c.hotkey.clone().into());
    s.set_corner(match c.indicator_position {
        IndicatorPosition::BottomRight => 0,
        IndicatorPosition::BottomLeft => 1,
        IndicatorPosition::TopRight => 2,
        IndicatorPosition::TopLeft => 3,
    });
    s.set_active_hotkey(
        app.keys
            .as_ref()
            .and_then(|k| k.active.as_ref())
            .map(|k| k.label.clone())
            .unwrap_or_else(|| "Unavailable".into())
            .into(),
    );
    let mut value = toml::Value::try_from(c).expect("serialize settings");
    for key in [
        "language",
        "audio_output_device",
        "save_transcript",
        "start_listening",
        "show_indicator",
        "show_result_popup",
        "viewer_theme",
        "hotkey",
        "indicator_position",
        "reduced_motion",
        "reduced_transparency",
    ] {
        value.as_table_mut().unwrap().remove(key);
    }
    s.set_advanced(toml::to_string_pretty(&value).unwrap_or_default().into());
    s.set_error("".into());
}
fn preview(state: &str) -> Vec<Recording> {
    if state == "empty" {
        return vec![];
    }
    let mut r = Recording::new(Language::En, false);
    r.title = "Design review · October 7".into();
    r.status = RecordingStatus::Completed;
    r.duration_ms = 3600000;
    let text = [
        "Let's keep the recording controls visible while people read. The document should feel quiet, with enough space around each paragraph to make a long conversation easy to follow.",
        "When a reader scrolls back to an earlier thought, new words should arrive without moving the page. We can offer a clear action to jump back to the latest text.",
        "Selamat pagi. Teks Unicode seperti 日本語 dan café tetap terbaca. We keep recognized words and punctuation, and we never invent summaries or headings.",
    ];
    let count = if state == "long" { 2501 } else { 12 };
    for n in 0..count {
        r.segments.push(TranscriptSegment {
            recording_id: r.id.clone(),
            sequence: n,
            start_ms: n * 1400,
            end_ms: n * 1400 + 1000,
            text: text[n as usize % text.len()].into(),
            clock_time: None,
        });
    }
    r.preview = text[0].chars().take(110).collect();
    if state == "recording" {
        r.status = RecordingStatus::Recording;
    }
    if state == "error" {
        r.status = RecordingStatus::Incomplete;
        r.warnings.push(
            "Missing audio: the transcription queue overflowed. This recording is incomplete."
                .into(),
        );
    }
    let mut second = Recording::new(Language::Id, false);
    second.title = "Morning notes".into();
    second.status = RecordingStatus::Completed;
    second.duration_ms = 324000;
    second.preview = "Selamat pagi, mari kita mulai.".into();
    second.segments.push(TranscriptSegment {
        recording_id: second.id.clone(),
        sequence: 0,
        start_ms: 0,
        end_ms: 3000,
        text: second.preview.clone(),
        clock_time: None,
    });
    vec![r, second]
}
pub fn run(
    config: Config,
    logger: Arc<Logger>,
    background: bool,
    preview_state: Option<String>,
) -> Result<()> {
    use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
    let requested = std::env::var("SLINT_BACKEND").unwrap_or_else(|_| "winit".into());
    let renderer = requested.strip_prefix("winit-").unwrap_or("");
    let backend = slint::BackendSelector::new().backend_name("winit".into());
    let backend = if renderer.is_empty() {
        backend
    } else {
        backend.renderer_name(renderer.into())
    };
    backend
        .with_winit_window_attributes_hook(|a| {
            if a.title == "Depth overlay" || a.title == "Depth completion" {
                a.with_active(false).with_skip_taskbar(true)
            } else {
                a
            }
        })
        .select()?;
    let ui = MainWindow::new()?;
    crate::appearance::round_main_window(ui.window());
    let settings = SettingsWindow::new()?;
    let overlay = OverlayWindow::new()?;
    let note = NotificationWindow::new()?;
    let previewing = preview_state.is_some();
    let library = if let Some(s) = &preview_state {
        preview(s)
    } else {
        recording::library(&config.transcripts_dir()).unwrap_or_else(|e| {
            ui.set_warning(format!("Could not load recordings: {e}").into());
            vec![]
        })
    };
    let pipeline = if previewing {
        None
    } else {
        Some(pipeline::start(config.clone(), logger.clone())?)
    };
    let keys = pipeline.as_ref().map(|p| {
        hotkey::spawn(
            &config.hotkey,
            &config.hotkey_fallback,
            p.control_sender(),
            logger.clone(),
        )
    });
    let app = Rc::new(RefCell::new(App {
        config,
        pipeline,
        keys,
        library,
        selected: None,
        quitting: false,
        started: None,
        started_duration_ms: 0,
        search_cursor: 0,
        search_query: String::new(),
        completion_id: String::new(),
        notification_until: None,
    }));
    sync_theme(&ui, &settings, &overlay, &note, &app.borrow().config);
    ui.set_language(if app.borrow().config.language == Language::En {
        0
    } else {
        1
    });
    fill_library(&ui, &app.borrow());
    ui.set_hotkey(
        app.borrow()
            .keys
            .as_ref()
            .and_then(|k| k.active.as_ref())
            .map(|k| k.label.clone())
            .unwrap_or_else(|| {
                if previewing {
                    "Preview".into()
                } else {
                    "Hotkey unavailable. Use Start.".into()
                }
            })
            .into(),
    );
    let first_id = { app.borrow().library.first().map(|r| r.id.clone()) };
    if let Some(id) = first_id {
        if let Err(e) = select(&ui, &mut app.borrow_mut(), &id, true) {
            ui.set_warning(e.to_string().into());
        }
    }
    let weak = ui.as_weak();
    {
        let state = app.clone();
        let weak = ui.as_weak();
        let sw = settings.as_weak();
        ui.on_language_selected(move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut app = state.borrow_mut();
            let mut next = app.config.clone();
            next.language = if index == 0 {
                Language::En
            } else {
                Language::Id
            };
            match next.save() {
                Ok(()) => {
                    if let Some(pipeline) = &app.pipeline {
                        pipeline.set_language(next.language);
                    }
                    app.config = next;
                    if let Some(settings) = sw.upgrade() {
                        settings.set_language(index);
                    }
                }
                Err(error) => {
                    ui.set_language(if app.config.language == Language::En {
                        0
                    } else {
                        1
                    });
                    ui.set_warning(format!("Could not save language: {error}").into());
                }
            }
        });
    }
    ui.window().on_close_requested(move || {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.hide();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_select_recording(move |id| {
            if let Some(ui) = weak.upgrade() {
                if let Err(e) = select(&ui, &mut app.borrow_mut(), &id, true) {
                    ui.set_warning(e.to_string().into());
                }
            }
        });
    }
    {
        let app = app.clone();
        ui.on_toggle_recording(move || {
            if let Some(p) = &app.borrow().pipeline {
                p.send(Control::Toggle);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_continue_recording(move || {
            if let Some(ui) = weak.upgrade() {
                if ui.get_recording() || ui.get_finishing() {
                    return;
                }
                let app = app.borrow();
                if let (Some(p), Some(recording)) = (&app.pipeline, &app.selected) {
                    if recording.can_continue() {
                        p.send(Control::Continue(Box::new(recording.clone())));
                    }
                }
            }
        });
    }
    {
        let app = app.clone();
        overlay.on_stop(move || {
            if let Some(p) = &app.borrow().pipeline {
                p.send(Control::Pause);
            }
        });
    }
    {
        let weak = ui.as_weak();
        overlay.on_open_app(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.show();
            }
        });
    }
    {
        let app = app.clone();
        let s = settings.as_weak();
        ui.on_settings(move || {
            if let Some(s) = s.upgrade() {
                settings_values(&s, &app.borrow());
                let _ = s.show();
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_quit(move || {
            if let Some(ui) = weak.upgrade() {
                begin_quit(&ui, &mut app.borrow_mut(), false);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_confirm_quit(move || {
            if let Some(ui) = weak.upgrade() {
                begin_quit(&ui, &mut app.borrow_mut(), true);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_copy_document(move |timed| {
            if let Some(r) = &app.borrow().selected {
                if let Err(e) = crate::clipboard::copy_text_to_clipboard(&r.text(timed)) {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_warning(e.to_string().into());
                    }
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_export_document(move |markdown| {
            if let Some(ui) = weak.upgrade() {
                let result = (|| -> Result<()> {
                    let mut app = app.borrow_mut();
                    let r = app
                        .selected
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Choose a recording"))?;
                    let id = r.id.clone();
                    let mut path = PathBuf::from(ui.get_export_path().as_str());
                    anyhow::ensure!(!path.as_os_str().is_empty(), "Enter an export path");
                    path.set_extension(if markdown { "md" } else { "txt" });
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent)?;
                        }
                    }
                    recording::atomic_write(
                        &path,
                        if markdown {
                            r.markdown()
                        } else {
                            r.text(false)
                        }
                        .as_bytes(),
                    )?;
                    if let Some(p) = &app.pipeline {
                        p.mark_exported(&id);
                    }
                    if let Some(r) = app.selected.as_mut() {
                        r.exported = true;
                    }
                    ui.set_exporting(false);
                    ui.set_warning(format!("Exported to {}", path.display()).into());
                    Ok(())
                })();
                if let Err(e) = result {
                    ui.set_warning(e.to_string().into());
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_delete_recording(move || {
            if let Some(ui) = weak.upgrade() {
                let mut app = app.borrow_mut();
                let Some(recording) = app.selected.as_ref().filter(|r| r.can_delete()).cloned()
                else {
                    return;
                };
                if let Some(p) = &app.pipeline {
                    p.send(Control::Delete(Box::new(recording)));
                } else {
                    match recording::delete(&recording, &app.config.transcripts_dir()) {
                        Ok(()) => forget_recording(&ui, &mut app, &recording.id),
                        Err(error) => {
                            ui.set_warning(format!("Could not delete recording: {error}").into())
                        }
                    }
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_rename_recording(move |title| {
            if let Some(ui) = weak.upgrade() {
                let mut app = app.borrow_mut();
                let dir = app.config.transcripts_dir();
                let id = app
                    .selected
                    .as_ref()
                    .map(|r| r.id.clone())
                    .unwrap_or_default();
                let result = if let Some(p) = &app.pipeline {
                    if p.recording(&id).is_some() {
                        p.rename(&id, &title, &dir)
                    } else if let Some(r) = app.selected.as_mut() {
                        recording::rename(r, &title, &dir)
                    } else {
                        Ok(())
                    }
                } else if let Some(r) = app.selected.as_mut() {
                    recording::rename(r, &title, &dir)
                } else {
                    Ok(())
                };
                if let Err(e) = result {
                    ui.set_warning(e.to_string().into());
                } else {
                    if let Some(r) = app.selected.as_mut() {
                        r.title = title.to_string();
                    }
                    if let Some(r) = app.library.iter_mut().find(|r| r.id == id) {
                        r.title = title.to_string();
                    }
                    fill_library(&ui, &app);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_view_timed(move |_| {
            if let Some(ui) = weak.upgrade() {
                let app = app.borrow();
                if let Some(r) = &app.selected {
                    ui.invoke_replace_document(r.text(ui.get_timed()).into(), true);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_find(move |direction| {
            if let Some(ui) = weak.upgrade() {
                let mut app = app.borrow_mut();
                let query = ui.get_query().to_string();
                let text = ui.get_document().to_string();
                let matches = find_matches(&text, &query);
                if matches.is_empty() {
                    ui.set_matches(if query.is_empty() { "" } else { "0 matches" }.into());
                    return;
                }
                if query != app.search_query || direction == 0 {
                    app.search_cursor = 0;
                    app.search_query = query;
                } else if direction < 0 {
                    app.search_cursor = (app.search_cursor + matches.len() - 1) % matches.len();
                } else {
                    app.search_cursor = (app.search_cursor + 1) % matches.len();
                }
                let index = app.search_cursor.min(matches.len() - 1);
                let (start, end) = matches[index];
                if direction == 0 {
                    ui.invoke_highlight_match(start as i32, end as i32);
                } else {
                    ui.invoke_select_match(start as i32, end as i32);
                }
                ui.set_matches(format!("{} / {}", index + 1, matches.len()).into());
            }
        });
    }
    {
        let app = app.clone();
        let weak = ui.as_weak();
        let sw = settings.as_weak();
        let ow = overlay.as_weak();
        let nw = note.as_weak();
        settings.on_appearance(move |index, transparency, motion| {
            let mut app = app.borrow_mut();
            app.config.viewer_theme = match index {
                1 => ViewerTheme::Light,
                2 => ViewerTheme::Dark,
                _ => ViewerTheme::System,
            };
            app.config.reduced_transparency = transparency;
            app.config.reduced_motion = motion;
            if let (Some(ui), Some(s), Some(o), Some(n)) =
                (weak.upgrade(), sw.upgrade(), ow.upgrade(), nw.upgrade())
            {
                sync_theme(&ui, &s, &o, &n, &app.config);
            }
        });
    }
    {
        let app = app.clone();
        let sw = settings.as_weak();
        let weak = ui.as_weak();
        let logger = logger.clone();
        settings.on_save_settings(move || {
            if let Some(s) = sw.upgrade() {
                let result = (|| -> Result<()> {
                    let mut app = app.borrow_mut();
                    let mut next: Config = toml::from_str(s.get_advanced().as_str())?;
                    next.base_dir = app.config.base_dir.clone();
                    next.language = if s.get_language() == 0 {
                        Language::En
                    } else {
                        Language::Id
                    };
                    let id = s
                        .get_output_device_ids()
                        .row_data(s.get_output_device().max(0) as usize)
                        .unwrap_or_default();
                    next.audio_output_device = if id.is_empty() {
                        None
                    } else {
                        Some(id.to_string())
                    };
                    next.viewer_theme = match s.get_theme() {
                        1 => ViewerTheme::Light,
                        2 => ViewerTheme::Dark,
                        _ => ViewerTheme::System,
                    };
                    next.save_transcript = s.get_autosave();
                    next.start_listening = s.get_startup();
                    next.show_indicator = s.get_overlay();
                    next.show_result_popup = s.get_notifications();
                    next.reduced_motion = s.get_reduced_motion();
                    next.reduced_transparency = s.get_reduced_transparency();
                    next.hotkey = s.get_hotkey().to_string();
                    next.indicator_position = match s.get_corner() {
                        1 => IndicatorPosition::BottomLeft,
                        2 => IndicatorPosition::TopRight,
                        3 => IndicatorPosition::TopLeft,
                        _ => IndicatorPosition::BottomRight,
                    };
                    next.validate()?;
                    next.save()?;
                    if next.hotkey != app.config.hotkey
                        || next.hotkey_fallback != app.config.hotkey_fallback
                    {
                        if let Some(mut k) = app.keys.take() {
                            k.shutdown();
                        }
                        if let Some(p) = &app.pipeline {
                            app.keys = Some(hotkey::spawn(
                                &next.hotkey,
                                &next.hotkey_fallback,
                                p.control_sender(),
                                logger.clone(),
                            ));
                        }
                    }
                    if let Some(p) = &app.pipeline {
                        p.send(Control::Configure(Box::new(next.clone())));
                    }
                    app.config = next;
                    settings_values(&s, &app);
                    if let Some(ui) = weak.upgrade() {
                        ui.set_language(s.get_language());
                        ui.set_hotkey(s.get_active_hotkey());
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => s.set_error("Settings saved.".into()),
                    Err(e) => s.set_error(e.to_string().into()),
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        let nw = note.as_weak();
        note.on_open_recording(move || {
            if let Some(ui) = weak.upgrade() {
                let mut app = app.borrow_mut();
                let id = app.completion_id.clone();
                if let Err(e) = select(&ui, &mut app, &id, true) {
                    ui.set_warning(e.to_string().into());
                }
                let _ = ui.show();
                if let Some(n) = nw.upgrade() {
                    let _ = n.hide();
                }
            }
        });
    }
    {
        let nw = note.as_weak();
        note.on_dismiss(move || {
            if let Some(n) = nw.upgrade() {
                let _ = n.hide();
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_review_unsaved(move || {
            if let Some(ui) = weak.upgrade() {
                let mut app = app.borrow_mut();
                let id = app
                    .pipeline
                    .as_ref()
                    .and_then(|p| {
                        p.recordings()
                            .into_iter()
                            .find(|r| !r.autosave && !r.exported)
                    })
                    .map(|r| r.id);
                if let Some(id) = id {
                    if let Err(e) = select(&ui, &mut app, &id, true) {
                        ui.set_warning(e.to_string().into());
                    }
                    ui.set_exporting(true);
                    ui.set_quit_prompt(false);
                }
            }
        });
    }
    let (menu, _open, toggle, _quit) = crate::tray::menu()?;
    let _tray = crate::tray::create(menu)?;
    let weak = ui.as_weak();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(event, tray_icon::TrayIconEvent::DoubleClick { .. }) {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    let _ = ui.show();
                }
            });
        }
    }));
    let timer = Timer::default();
    let weak = ui.as_weak();
    let sw = settings.as_weak();
    let ow = overlay.as_weak();
    let nw = note.as_weak();
    let state = app.clone();
    timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
        let (Some(ui), Some(s), Some(o), Some(n)) =
            (weak.upgrade(), sw.upgrade(), ow.upgrade(), nw.upgrade())
        else {
            return;
        };
        let mut app = state.borrow_mut();
        ui.set_compact((ui.window().size().width as f32 / ui.window().scale_factor()) < 1000.0);
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            match event.id.0.as_str() {
                "open" => {
                    let _ = ui.show();
                }
                "toggle" => {
                    if let Some(p) = &app.pipeline {
                        p.send(Control::Toggle);
                    }
                }
                "quit" => begin_quit(&ui, &mut app, false),
                _ => {}
            }
        }
        let mut changed = false;
        let mut completed = None;
        let mut started = None;
        let mut notice = None;
        let mut deleted = Vec::new();
        if let Some(p) = &app.pipeline {
            for ev in p.events().try_iter() {
                changed = true;
                match ev {
                    PipelineEvent::Started(r) => started = Some((r.id, r.duration_ms)),
                    PipelineEvent::Deleted { id } => deleted.push(id),
                    PipelineEvent::Warning { id, message }
                        if id == ui.get_selected_id().as_str() =>
                    {
                        notice = Some(message)
                    }
                    PipelineEvent::State { id, status, .. } => {
                        if matches!(
                            status,
                            RecordingStatus::Completed | RecordingStatus::Incomplete
                        ) {
                            completed = Some(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        for id in &deleted {
            forget_recording(&ui, &mut app, id);
            if app.completion_id == *id {
                app.completion_id.clear();
                app.notification_until = None;
                let _ = n.hide();
            }
        }
        if changed {
            let fresh = app
                .pipeline
                .as_ref()
                .map(|p| p.recordings())
                .unwrap_or_default();
            for r in fresh {
                if let Some(old) = app.library.iter_mut().find(|v| v.id == r.id) {
                    *old = r;
                } else {
                    app.library.push(r);
                }
            }
            app.library.sort_by(|a, b| b.started.cmp(&a.started));
            fill_library(&ui, &app);
            if let Some((id, duration_ms)) = started.filter(|(id, _)| !deleted.contains(id)) {
                app.started = Some(Instant::now());
                app.started_duration_ms = duration_ms;
                let reset = ui.get_selected_id().as_str() != id;
                if let Err(e) = select(&ui, &mut app, &id, reset) {
                    ui.set_warning(e.to_string().into());
                }
            } else {
                let id = ui.get_selected_id();
                if !id.is_empty() {
                    if let Err(e) = select(&ui, &mut app, &id, false) {
                        ui.set_warning(e.to_string().into());
                    }
                }
            }
        }
        if let Some(message) = notice {
            let warning = ui.get_warning();
            if !warning.contains(&message) {
                ui.set_warning(
                    if warning.is_empty() {
                        message
                    } else {
                        format!("{warning}\n{message}")
                    }
                    .into(),
                );
            }
        }
        let active = app.library.iter().find(|r| {
            matches!(
                r.status,
                RecordingStatus::Recording | RecordingStatus::Processing
            )
        });
        let recording = active.is_some_and(|r| r.status == RecordingStatus::Recording);
        let finishing = active.is_some_and(|r| r.status == RecordingStatus::Processing);
        let live = app.pipeline.as_ref().map(|p| p.live_state());
        ui.set_recording(recording);
        ui.set_finishing(finishing);
        sync_live(&ui, live.as_ref(), recording || finishing);
        let elapsed = if recording {
            app.started
                .map(|t| {
                    app.started_duration_ms
                        .saturating_add(t.elapsed().as_millis() as u64)
                })
                .unwrap_or(0)
        } else {
            active
                .map(|r| r.duration_ms)
                .unwrap_or_else(|| app.selected.as_ref().map(|r| r.duration_ms).unwrap_or(0))
        };
        ui.set_elapsed(recording::timestamp(elapsed).into());
        ui.set_state(
            if finishing {
                "Finishing transcription"
            } else if recording {
                if live.as_ref().is_some_and(|l| l.capture_error.is_some()) {
                    "Capture unavailable"
                } else if live.as_ref().is_some_and(|l| l.waiting_for_audio()) {
                    "Waiting for desktop audio"
                } else {
                    "Recording desktop audio"
                }
            } else {
                "Ready to record"
            }
            .into(),
        );
        toggle.set_text(if finishing {
            "Finishing transcription"
        } else if recording {
            "Stop recording"
        } else {
            "Start recording"
        });
        toggle.set_enabled(!finishing);
        if app.config.show_indicator && (recording || finishing) {
            o.set_state(ui.get_state());
            o.set_elapsed(ui.get_elapsed());
            o.set_finishing(finishing);
            let _ = o.show();
            crate::appearance::place_floating(o.window(), ui.window(), &app.config);
        } else {
            let _ = o.hide();
        }
        if let Some(id) = completed.filter(|id| !deleted.contains(id)) {
            app.started = None;
            if app.config.show_result_popup {
                app.completion_id = id.clone();
                n.set_message(
                    if app
                        .library
                        .iter()
                        .find(|r| r.id == id)
                        .is_some_and(|r| r.status == RecordingStatus::Incomplete)
                    {
                        "Recording finished with warnings"
                    } else {
                        "Your transcript is ready"
                    }
                    .into(),
                );
                let _ = n.show();
                crate::appearance::place_floating(n.window(), ui.window(), &app.config);
                app.notification_until = Some(Instant::now() + Duration::from_secs(8));
            }
        }
        if app.notification_until.is_some_and(|t| Instant::now() >= t) {
            let _ = n.hide();
            app.notification_until = None;
        }
        sync_theme(&ui, &s, &o, &n, &app.config);
        if app.quitting && !recording && !finishing {
            if let Some(k) = app.keys.as_mut() {
                k.shutdown();
            }
            let _ = slint::quit_event_loop();
        }
    });
    if !background {
        ui.show()?;
    }
    if preview_state.as_deref() == Some("settings") {
        settings_values(&settings, &app.borrow());
        settings.show()?;
    }
    slint::run_event_loop_until_quit()?;
    timer.stop();
    let mut app = app.borrow_mut();
    if let Some(mut k) = app.keys.take() {
        k.shutdown();
    }
    if let Some(p) = app.pipeline.take() {
        p.shutdown();
    }
    Ok(())
}
// Match Unicode strings by character boundaries, retaining UTF-8 offsets used by Slint.
fn find_matches(text: &str, query: &str) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return vec![];
    }
    let needle = query.to_lowercase();
    let mut result = vec![];
    for (start, _) in text.char_indices() {
        let mut folded = String::new();
        for (offset, ch) in text[start..].char_indices() {
            folded.extend(ch.to_lowercase());
            if folded == needle {
                result.push((start, start + offset + ch.len_utf8()));
                break;
            }
            if !needle.starts_with(&folded) {
                break;
            }
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_search_offsets() {
        assert_eq!(
            find_matches("世界 café CAFÉ", "café"),
            vec![(7, 12), (13, 18)]
        );
        assert_eq!(find_matches("İstanbul", "İ"), vec![(0, 2)]);
    }
}
#[cfg(test)]
mod document_tests {
    use super::*;
    #[test]
    fn updates_preserve_selection_and_reader_scroll() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(1180.0, 780.0));
        ui.set_selected_id("test".into());
        ui.show().unwrap();
        let text = (0..2501)
            .map(|n| format!("Paragraph {n}. Unicode 世界 café.\n\n"))
            .collect::<String>();
        ui.invoke_replace_document(text.clone().into(), true);
        assert_eq!(ui.get_selection_cursor(), 0);
        ui.invoke_select_match(0, 9);
        ui.set_document_scroll(-400.0);
        let mut live = crate::live::LiveState::default();
        live.begin("test".into());
        live.update_draft(0, 1, "Halo, ini draf pertama.".into());
        sync_live(&ui, Some(&live), true);
        live.update_draft(0, 2, "Halo, ini draf yang dikoreksi.".into());
        sync_live(&ui, Some(&live), true);
        assert_eq!(ui.get_document().as_str(), text);
        assert_eq!(ui.get_selection_cursor(), 9);
        assert_eq!(ui.get_document_scroll(), -400.0);
        ui.set_selected_id("other".into());
        sync_live(&ui, Some(&live), true);
        assert!(ui.get_live_draft().is_empty());
        ui.set_selected_id("test".into());
        ui.set_timed(true);
        sync_live(&ui, Some(&live), true);
        assert!(ui.get_live_draft().contains("dikoreksi"));
        assert_eq!(ui.get_selection_cursor(), 9);
        assert_eq!(ui.get_document_scroll(), -400.0);
        sync_live(&ui, Some(&live), false);
        assert!(ui.get_live_draft().is_empty());
        ui.invoke_replace_document(format!("{text}New result.").into(), false);
        assert_eq!(ui.get_selection_anchor(), 0);
        assert_eq!(ui.get_selection_cursor(), 9);
        assert_eq!(ui.get_document_scroll(), -400.0);
        assert!(ui.get_unread());
        ui.invoke_replace_document(text.clone().into(), true);
        ui.invoke_jump_latest();
        ui.invoke_replace_document(format!("{text}Latest result.").into(), false);
        assert!(ui.get_at_bottom());
        assert!(!ui.get_unread());
    }
}

#[cfg(test)]
mod caption_tests {
    use super::*;
    use slint::platform::{PointerEventButton, WindowEvent};
    use std::cell::Cell;

    fn window() -> MainWindow {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(1180.0, 780.0));
        ui.show().unwrap();
        ui
    }

    fn click(ui: &MainWindow, x: f32) {
        let position = slint::LogicalPosition::new(x, 22.0);
        let button = PointerEventButton::Left;
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        ui.window()
            .dispatch_event(WindowEvent::PointerPressed { position, button });
        ui.window()
            .dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    #[test]
    fn minimize_works_on_first_click() {
        let ui = window();
        click(&ui, 1065.0);
        assert!(ui.window().is_minimized());
    }

    #[test]
    fn maximize_and_restore_work_on_first_click() {
        let ui = window();
        click(&ui, 1111.0);
        assert!(ui.window().is_maximized());
        click(&ui, 1111.0);
        assert!(!ui.window().is_maximized());
    }

    #[test]
    fn close_works_on_first_click() {
        let ui = window();
        let requests = Rc::new(Cell::new(0));
        let observed = requests.clone();
        ui.window().on_close_requested(move || {
            observed.set(observed.get() + 1);
            slint::CloseRequestResponse::KeepWindowShown
        });
        click(&ui, 1157.0);
        assert_eq!(requests.get(), 1);
    }

    #[test]
    fn clicked_caption_button_keeps_keyboard_access() {
        let ui = window();
        click(&ui, 1111.0);
        for (key, maximized) in [(" ", false), ("\n", true)] {
            ui.window()
                .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            ui.window()
                .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
            assert_eq!(ui.window().is_maximized(), maximized);
        }
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    use i_slint_backend_testing::ElementHandle;
    use slint::platform::PointerEventButton;
    use std::cell::Cell;

    fn fixture(records: Vec<Recording>) -> (MainWindow, App) {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().unwrap();
        ui.window().set_size(slint::LogicalSize::new(1180., 780.));
        ui.show().unwrap();
        let app = App {
            config: Config::default(),
            pipeline: None,
            keys: None,
            library: records,
            selected: None,
            quitting: false,
            started: None,
            started_duration_ms: 0,
            search_cursor: 0,
            search_query: String::new(),
            completion_id: String::new(),
            notification_until: None,
        };
        (ui, app)
    }
    fn click(ui: &MainWindow, label: &str) {
        ElementHandle::find_by_accessible_label(ui, label)
            .next()
            .unwrap()
            .mock_single_click(PointerEventButton::Left);
    }
    fn completed() -> Recording {
        let mut r = Recording::new(Language::En, false);
        r.status = RecordingStatus::Completed;
        r
    }
    #[test]
    fn delete_requires_confirmation_and_cancel_does_not_delete() {
        let (ui, mut app) = fixture(vec![completed()]);
        let id = app.library[0].id.clone();
        select(&ui, &mut app, &id, true).unwrap();
        let requests = Rc::new(Cell::new(0));
        let observed = requests.clone();
        ui.on_delete_recording(move || observed.set(observed.get() + 1));
        click(&ui, "Delete recording");
        assert!(ui.get_delete_prompt());
        assert_eq!(requests.get(), 0);
        click(&ui, "Cancel");
        assert!(!ui.get_delete_prompt());
        assert_eq!(requests.get(), 0);
        click(&ui, "Delete recording");
        click(&ui, "Confirm delete recording");
        assert_eq!(requests.get(), 1);
        assert!(!ui.get_delete_prompt());
        ui.set_can_delete(false);
        click(&ui, "Delete recording");
        assert!(!ui.get_delete_prompt());
        assert_eq!(requests.get(), 1);
    }
    #[test]
    fn compact_confirmation_keeps_actions_visible() {
        let (ui, mut app) = fixture(vec![completed()]);
        ui.window().set_size(slint::LogicalSize::new(800., 600.));
        ui.set_compact(true);
        ui.set_sidebar_open(true);
        let id = app.library[0].id.clone();
        select(&ui, &mut app, &id, true).unwrap();
        click(&ui, "Delete recording");
        for label in ["Cancel", "Confirm delete recording", "Continue recording"] {
            let element = ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .unwrap();
            assert!(element.absolute_position().y + element.size().height <= 600., "{label} is below the window");
        }
    }
    #[test]
    fn deleting_selected_recording_selects_next_and_last_delete_clears_reader() {
        let (ui, mut app) = fixture(vec![completed(), completed()]);
        let first = app.library[0].id.clone();
        let second = app.library[1].id.clone();
        select(&ui, &mut app, &first, true).unwrap();
        ui.set_delete_prompt(true);
        forget_recording(&ui, &mut app, &first);
        assert_eq!(app.library.len(), 1);
        assert_eq!(ui.get_selected_id().as_str(), second);
        assert!(ui.get_can_delete());
        assert!(!ui.get_delete_prompt());
        forget_recording(&ui, &mut app, &second);
        assert!(app.library.is_empty());
        assert!(app.selected.is_none());
        assert!(ui.get_selected_id().is_empty());
        assert!(ui.get_document().is_empty());
        assert!(!ui.get_can_delete());
        assert!(!ui.get_can_continue());
    }
    #[test]
    fn unreadable_recording_can_be_selected_for_deletion() {
        let mut r = completed();
        r.autosave = true;
        r.source = Some(std::env::temp_dir().join(&r.id).join("missing.jsonl"));
        let id = r.id.clone();
        let (ui, mut app) = fixture(vec![r]);
        select(&ui, &mut app, &id, true).unwrap();
        assert_eq!(ui.get_selected_id().as_str(), id);
        assert!(ui.get_warning().contains("Cannot read recording"));
        assert!(ui.get_can_delete());
        assert!(!ui.get_can_continue());
    }
}

//! Slint windows and tray actions share this application controller.
use crate::{
    config::{Config, IndicatorPosition, Language, ViewerTheme},
    hotkey,
    logging::Logger,
    pipeline::{self, Control, PipelineHandle},
    recording::{self, PipelineEvent, Recording, RecordingStatus, TranscriptSegment},
};
use anyhow::{Context, Result};
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
    downloads: crate::models::Downloads,
    downloaded: Option<String>,
    gpu: crate::gpu::Discovery,
    gpu_rx: crossbeam_channel::Receiver<crate::gpu::Discovery>,
    pipeline: Option<PipelineHandle>,
    keys: Option<hotkey::Hotkey>,
    library: Vec<Recording>,
    selected: Option<Recording>,
    speaker_window: Option<SpeakersWindow>,
    editor: Option<TranscriptEditor>,
    quitting: bool,
    started: Option<Instant>,
    started_duration_ms: u64,
    search_cursor: usize,
    search_query: String,
    completion_id: String,
    notification_until: Option<Instant>,
}
struct TranscriptEditor {
    window: TranscriptEditorWindow,
    id: String,
    basis: String,
}
fn cancel_editor(window: &TranscriptEditorWindow) {
    if window.get_dirty() {
        window.set_discard_prompt(true);
    } else {
        let _ = window.hide();
    }
}
fn save_editor(ui: &MainWindow, app: &Rc<RefCell<App>>) -> Result<()> {
    let mut app = app.borrow_mut();
    let editor = app.editor.as_ref().context("Transcript editor is closed")?;
    let id = editor.id.clone();
    let request = crate::editing::Request {
        basis: editor.basis.clone(),
        texts: editor
            .window
            .get_passages()
            .iter()
            .map(|r| r.text.to_string())
            .collect(),
    };
    let result = if let Some(p) = app.pipeline.as_ref().filter(|p| p.recording(&id).is_some()) {
        p.correct_transcript(&id, request)
    } else {
        let meta = app
            .library
            .iter_mut()
            .find(|r| r.id == id)
            .context("Recording no longer exists")?;
        let mut full = recording::load_recording(meta)?;
        let result = crate::editing::correct(&mut full, request, &app.config.transcripts_dir());
        let meta = app
            .library
            .iter_mut()
            .find(|r| r.id == id)
            .expect("existing recording");
        *meta = if full.autosave && full.source.is_some() {
            full.metadata()
        } else {
            full
        };
        result
    };
    if let Some(full) = app.pipeline.as_ref().and_then(|p| p.recording(&id)) {
        if let Some(meta) = app.library.iter_mut().find(|r| r.id == id) {
            *meta = if full.autosave && full.source.is_some() {
                full.metadata()
            } else {
                full
            };
        }
    }
    fill_library(ui, &app);
    if ui.get_selected_id().as_str() == id {
        select(ui, &mut app, &id, false)?;
    }
    result?;
    if let Some(editor) = &app.editor {
        editor.window.set_dirty(false);
        editor.window.set_discard_prompt(false);
        editor.window.hide()?;
    }
    Ok(())
}
fn open_editor(ui: &MainWindow, app: &Rc<RefCell<App>>) -> Result<()> {
    if let Some(editor) = &app.borrow().editor {
        if editor.window.get_dirty() {
            editor.window.set_error(
                "Save or discard these changes before opening another transcript.".into(),
            );
            editor.window.show()?;
            return Ok(());
        }
    }
    let r = app
        .borrow()
        .selected
        .clone()
        .context("Select a finished recording first")?;
    if !crate::editing::available(&r) {
        anyhow::bail!("Wait for transcription and speaker detection to finish before editing.");
    }
    if app.borrow().editor.is_none() {
        let window = TranscriptEditorWindow::new()?;
        let a = Rc::downgrade(app);
        let u = ui.as_weak();
        let w = window.as_weak();
        window.on_save(move || {
            if let (Some(app), Some(ui), Some(window)) = (a.upgrade(), u.upgrade(), w.upgrade()) {
                if let Err(e) = save_editor(&ui, &app) {
                    window.set_error(e.to_string().into());
                }
            }
        });
        let w = window.as_weak();
        window.on_cancel(move || {
            if let Some(w) = w.upgrade() {
                cancel_editor(&w);
            }
        });
        let w = window.as_weak();
        window.on_discard(move || {
            if let Some(w) = w.upgrade() {
                w.set_dirty(false);
                w.set_discard_prompt(false);
                let _ = w.hide();
            }
        });
        let w = window.as_weak();
        window.window().on_close_requested(move || {
            if let Some(w) = w.upgrade() {
                cancel_editor(&w);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
        app.borrow_mut().editor = Some(TranscriptEditor {
            window,
            id: String::new(),
            basis: String::new(),
        });
    }
    let mut app = app.borrow_mut();
    let config = app.config.clone();
    let editor = app.editor.as_mut().expect("created editor");
    editor.id = r.id.clone();
    editor.basis = crate::editing::basis(&r);
    theme(&editor.window, &config);
    editor.window.set_recording_title(r.title.clone().into());
    editor.window.set_passages(ModelRc::new(VecModel::from(
        crate::editing::passages(&r)
            .into_iter()
            .map(|p| EditorRow {
                label: p.label.into(),
                text: p.text.into(),
            })
            .collect::<Vec<_>>(),
    )));
    editor.window.set_dirty(false);
    editor.window.set_discard_prompt(false);
    editor.window.set_error("".into());
    editor.window.show()?;
    Ok(())
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
fn fill_speakers(window: &SpeakersWindow, r: &Recording) {
    let speakers = crate::speakers::active_speakers(r);
    let turns = crate::speakers::display_turns(r);
    let rows = speakers
        .iter()
        .map(|s| SpeakerRow {
            id: s.id.clone().into(),
            name: s.name.clone().into(),
            sample: turns
                .iter()
                .filter(|t| t.speaker_id.as_deref() == Some(s.id.as_str()))
                .take(2)
                .map(|t| t.text.chars().take(140).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
                .into(),
        })
        .collect::<Vec<_>>();
    window.set_speakers(ModelRc::new(VecModel::from(rows)));
    let names = speakers
        .iter()
        .map(|s| slint::SharedString::from(s.name.as_str()))
        .collect::<Vec<_>>();
    window.set_names(ModelRc::new(VecModel::from(names.clone())));
    let mut assignments = vec![slint::SharedString::from("Unknown speaker")];
    assignments.extend(names);
    window.set_assignment_choices(ModelRc::new(VecModel::from(assignments)));
    window.set_turn_choices(ModelRc::new(VecModel::from(
        turns
            .iter()
            .map(|t| {
                slint::SharedString::from(format!(
                    "{}  {}: {}",
                    recording::timestamp(t.start_ms),
                    t.label,
                    t.text.chars().take(60).collect::<String>()
                ))
            })
            .collect::<Vec<_>>(),
    )));
    window.set_turn_index(
        window
            .get_turn_index()
            .min(turns.len().saturating_sub(1) as i32),
    );
    window.set_assignment_index(window.get_assignment_index().min(speakers.len() as i32));
    let editable = r.can_delete() && r.speaker_finished;
    window.set_editing_enabled(editable);
    window.set_status(if editable {"Rename a speaker to update all their turns. Merge duplicate groups or correct a turn below."} else {"Labels are provisional while recording. You can name and correct speakers after processing finishes."}.into());
}
fn speaker_correction(
    ui: &MainWindow,
    app: &Rc<RefCell<App>>,
    change: crate::speakers::Change,
) -> Result<()> {
    let mut app = app.borrow_mut();
    let previous = app
        .selected
        .as_ref()
        .context("No recording selected")?
        .clone();
    let id = previous.id.clone();
    let dir = app.config.transcripts_dir();
    if let Some(p) = app.pipeline.as_ref().filter(|p| p.recording(&id).is_some()) {
        p.correct_speaker(&id, change)?;
    } else {
        let mut updated = previous.clone();
        crate::speakers::correct(&mut updated, change, &dir)?;
        if let Some(meta) = app.library.iter_mut().find(|m| m.id == id) {
            *meta = if updated.source.is_some() {
                updated.metadata()
            } else {
                updated
            };
        }
    }
    // Keep the old source spans until selection has been mapped to the new rendering.
    app.selected = Some(previous);
    select(ui, &mut app, &id, false)?;
    Ok(())
}
fn open_speakers(ui: &MainWindow, app: &Rc<RefCell<App>>) -> Result<()> {
    if app.borrow().speaker_window.is_none() {
        let window = SpeakersWindow::new()?;
        let a = Rc::downgrade(app);
        let w = window.as_weak();
        let u = ui.as_weak();
        window.on_rename_speaker(move |id, name| {
            if let (Some(app), Some(ui), Some(window)) = (a.upgrade(), u.upgrade(), w.upgrade()) {
                let result = speaker_correction(
                    &ui,
                    &app,
                    crate::speakers::Change::Renamed {
                        speaker_id: id.to_string(),
                        name: name.trim().into(),
                    },
                );
                window.set_error(
                    result
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_default()
                        .into(),
                );
            }
        });
        let a = Rc::downgrade(app);
        let w = window.as_weak();
        let u = ui.as_weak();
        window.on_merge_speaker(move |from, index| {
            if let (Some(app), Some(ui), Some(window)) = (a.upgrade(), u.upgrade(), w.upgrade()) {
                let into = app.borrow().selected.as_ref().and_then(|r| {
                    crate::speakers::active_speakers(r)
                        .get(index.max(0) as usize)
                        .map(|s| s.id.clone())
                });
                if let Some(into) = into {
                    let result = speaker_correction(
                        &ui,
                        &app,
                        crate::speakers::Change::Merged {
                            from: from.to_string(),
                            into,
                        },
                    );
                    window.set_error(
                        result
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_default()
                            .into(),
                    );
                }
            }
        });
        let a = Rc::downgrade(app);
        let w = window.as_weak();
        let u = ui.as_weak();
        window.on_reassign_turn(move |turn_index, speaker_index| {
            if let (Some(app), Some(ui), Some(window)) = (a.upgrade(), u.upgrade(), w.upgrade()) {
                let change = app.borrow().selected.as_ref().and_then(|r| {
                    let turns = crate::speakers::display_turns(r);
                    let t = turns.get(turn_index.max(0) as usize)?;
                    let speaker_id = if speaker_index <= 0 {
                        None
                    } else {
                        Some(
                            crate::speakers::active_speakers(r)
                                .get(speaker_index as usize - 1)?
                                .id
                                .clone(),
                        )
                    };
                    Some(crate::speakers::Change::Reassigned(
                        crate::speakers::Assignment {
                            sequence: t.sequence,
                            word_start: t.word_start,
                            word_end: t.word_end,
                            speaker_id,
                        },
                    ))
                });
                if let Some(change) = change {
                    let result = speaker_correction(&ui, &app, change);
                    window.set_error(
                        result
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_default()
                            .into(),
                    );
                }
            }
        });
        app.borrow_mut().speaker_window = Some(window);
    }
    let app = app.borrow();
    if let (Some(window), Some(r)) = (&app.speaker_window, &app.selected) {
        theme(window, &app.config);
        fill_speakers(window, r);
        window.show()?;
    }
    Ok(())
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
    ui.set_can_edit(readable && crate::editing::available(&r));
    if !r.can_delete() {
        ui.set_delete_prompt(false);
    }
    ui.set_legacy_recording(r.legacy_session.is_some());
    ui.set_selected_id(r.id.clone().into());
    ui.set_recording_title(r.title.clone().into());
    ui.set_detail(detail(&r).into());
    let mut warnings = r.warnings.clone();
    if !r.speaker_notice.is_empty() {
        warnings.push(r.speaker_notice.clone());
    }
    ui.set_warning(warnings.join("\n").into());
    ui.set_has_speakers(r.detect_speakers);
    if let Some(window) = &app.speaker_window {
        theme(window, &app.config);
        fill_speakers(window, &r);
        if !r.detect_speakers {
            let _ = window.hide();
        }
    }
    let doc = r.text(ui.get_timed());
    let selection = if !reset {
        app.selected
            .as_ref()
            .filter(|old| old.detect_speakers && r.detect_speakers && old.id == r.id)
            .map(|old| {
                (
                    crate::speakers::map_position(
                        old,
                        &r,
                        ui.get_timed(),
                        ui.get_selection_anchor().max(0) as usize,
                    ),
                    crate::speakers::map_position(
                        old,
                        &r,
                        ui.get_timed(),
                        ui.get_selection_cursor().max(0) as usize,
                    ),
                )
            })
    } else {
        None
    };
    let old_doc = ui.get_document().to_string();
    ui.invoke_replace_document(doc.clone().into(), reset);
    if old_doc != doc && !reset {
        if let Some((anchor, cursor)) = selection {
            let boundary = |position: usize| {
                let mut p = position.min(doc.len());
                while !doc.is_char_boundary(p) {
                    p -= 1;
                }
                p as i32
            };
            ui.invoke_preserve_selection(boundary(anchor), boundary(cursor));
        }
        let matches = find_matches(&doc, ui.get_query().as_str());
        app.search_cursor = app.search_cursor.min(matches.len().saturating_sub(1));
        ui.set_matches(
            if ui.get_query().is_empty() {
                String::new()
            } else if matches.is_empty() {
                "0 matches".into()
            } else {
                format!("{} / {}", app.search_cursor + 1, matches.len())
            }
            .into(),
        );
    }
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
        if let Some(window) = &app.speaker_window {
            window.set_editing_enabled(false);
            window.set_speakers(ModelRc::new(VecModel::from(Vec::<SpeakerRow>::new())));
            window.set_error("".into());
            let _ = window.hide();
        }
        app.search_query.clear();
        app.search_cursor = 0;
        ui.set_selected_id("".into());
        ui.set_recording_title("".into());
        ui.set_detail("".into());
        ui.set_warning("".into());
        ui.set_live_draft("".into());
        ui.set_can_continue(false);
        ui.set_has_speakers(false);
        ui.set_can_delete(false);
        ui.set_can_edit(false);
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
    if let Some(editor) = &app.editor {
        if editor.window.get_dirty() {
            editor
                .window
                .set_error("Save or discard your transcript edits before quitting.".into());
            let _ = editor.window.show();
            return;
        }
    }
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
fn model_ids() -> Vec<String> {
    ["recommended".into(), "custom".into()]
        .into_iter()
        .chain(crate::models::catalog().iter().map(|m| m.id.clone()))
        .collect()
}
fn model_list(s: &SettingsWindow, app: &App) {
    let mut names = vec![
        "Recommended for this device".into(),
        "Custom checkpoint (Advanced)".into(),
    ];
    names.extend(crate::models::catalog().iter().map(|m| {
        format!(
            "{} · {} · {:.0} MiB",
            m.name,
            if crate::models::installed(&app.config, m).is_some() {
                "Installed"
            } else {
                "Download"
            },
            m.size as f64 / 1048576.0
        )
        .into()
    }));
    s.set_asr_models(ModelRc::new(VecModel::from(names)));
    s.set_asr_model_ids(ModelRc::new(VecModel::from(
        model_ids()
            .into_iter()
            .map(slint::SharedString::from)
            .collect::<Vec<_>>(),
    )));
    s.set_gpu_status(
        app.gpu
            .device
            .as_ref()
            .map(|g| g.label())
            .unwrap_or_else(|| app.gpu.reason.clone())
            .into(),
    );
}
fn model_details(s: &SettingsWindow, app: &App) {
    let ids = model_ids();
    let id = ids
        .get(s.get_asr_model().max(0) as usize)
        .map(String::as_str)
        .unwrap_or("recommended");
    let text = match id {
        "custom" => format!(
            "Uses the whisper_model path in Advanced: {}",
            app.config.whisper_model.display()
        ),
        "recommended" => {
            let mut config = app.config.clone();
            config.indonesian_processing = match s.get_processing() {
                1 => crate::config::Processing::Nvidia,
                2 => crate::config::Processing::Cpu,
                _ => crate::config::Processing::Auto,
            };
            if crate::models::gpu_recommended(
                config.indonesian_processing,
                app.gpu.device.as_ref(),
                crate::models::cuda_runtime_exists(&config),
            ) {
                if crate::models::qualified(&config, app.gpu.device.as_ref()) {
                    "Uses turbo when installed. Otherwise uses Indonesian Whisper small.".into()
                } else {
                    "Indonesian Whisper small is active until turbo passes a speed check. Download turbo, select it, save, and record at least 30 seconds to qualify this device.".into()
                }
            } else {
                "Uses Indonesian Whisper small. Lowercase output without punctuation.".into()
            }
        }
        id => crate::models::get(id)
            .map(|m| {
                format!(
                    "{} {:.0} MiB. {}. {}",
                    m.description,
                    m.size as f64 / 1048576.0,
                    m.license,
                    m.source
                )
            })
            .unwrap_or_default(),
    };
    s.set_model_details(text.into());
}
fn advanced_config(raw: &str) -> Result<Config> {
    let mut value: toml::Value = toml::from_str(raw)?;
    if let Some(table) = value.as_table_mut() {
        table.remove("indonesian_processing");
        table.remove("indonesian_model");
    }
    Ok(value.try_into()?)
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
    s.set_processing(match c.indonesian_processing {
        crate::config::Processing::Auto => 0,
        crate::config::Processing::Nvidia => 1,
        crate::config::Processing::Cpu => 2,
    });
    model_list(s, app);
    let ids = model_ids();
    s.set_asr_model(
        ids.iter()
            .position(|id| id == c.indonesian_model.as_str())
            .unwrap_or(1) as i32,
    );
    model_details(s, app);
    s.set_theme(match c.viewer_theme {
        ViewerTheme::System => 0,
        ViewerTheme::Light => 1,
        ViewerTheme::Dark => 2,
    });
    s.set_autosave(c.save_transcript);
    s.set_detect_speakers(c.detect_speakers);
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
        "indonesian_processing",
        "indonesian_model",
        "audio_output_device",
        "save_transcript",
        "detect_speakers",
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
        downloads: crate::models::Downloads::default(),
        downloaded: None,
        gpu: crate::gpu::Discovery {
            device: None,
            reason: "Checking NVIDIA availability…".into(),
        },
        gpu_rx: {
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send(crate::gpu::discover());
            });
            rx
        },
        pipeline,
        keys,
        library,
        selected: None,
        speaker_window: None,
        editor: None,
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
        ui.on_edit_transcript(move || {
            if let Some(ui) = weak.upgrade() {
                if let Err(e) = open_editor(&ui, &app) {
                    ui.set_warning(e.to_string().into());
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_manage_speakers(move || {
            if let Some(ui) = weak.upgrade() {
                if let Err(e) = open_speakers(&ui, &app) {
                    ui.set_warning(e.to_string().into());
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
                if let Some(window) = &app.speaker_window {
                    theme(window, &app.config);
                }
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
                    let mut next = advanced_config(s.get_advanced().as_str())?;
                    next.base_dir = app.config.base_dir.clone();
                    next.indonesian_processing = match s.get_processing() {
                        1 => crate::config::Processing::Nvidia,
                        2 => crate::config::Processing::Cpu,
                        _ => crate::config::Processing::Auto,
                    };
                    next.indonesian_model = model_ids()
                        .get(s.get_asr_model().max(0) as usize)
                        .cloned()
                        .unwrap_or_else(|| "recommended".into())
                        .parse()?;
                    if !matches!(next.indonesian_model.as_str(), "recommended" | "custom") {
                        let m = crate::models::get(next.indonesian_model.as_str())?;
                        anyhow::ensure!(
                            crate::models::installed(&next, m).is_some(),
                            "Download the selected model before saving."
                        );
                    }
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
                    next.detect_speakers = s.get_detect_speakers();
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
    {
        let app = app.clone();
        let sw = settings.as_weak();
        settings.on_model_selected(move || {
            if let Some(s) = sw.upgrade() {
                model_details(&s, &app.borrow());
            }
        });
    }
    for recommended in [false, true] {
        let app = app.clone();
        let sw = settings.as_weak();
        let callback = move || {
            if let Some(s) = sw.upgrade() {
                let mut app = app.borrow_mut();
                let id = if recommended {
                    if s.get_processing() != 2
                        && crate::models::gpu_recommended(
                            crate::config::Processing::Auto,
                            app.gpu.device.as_ref(),
                            crate::models::cuda_runtime_exists(&app.config),
                        )
                    {
                        "turbo".to_string()
                    } else {
                        "small-id".to_string()
                    }
                } else {
                    model_ids()
                        .get(s.get_asr_model().max(0) as usize)
                        .cloned()
                        .unwrap_or_default()
                };
                let id = if id == "recommended" {
                    if s.get_processing() != 2
                        && crate::models::gpu_recommended(
                            crate::config::Processing::Auto,
                            app.gpu.device.as_ref(),
                            crate::models::cuda_runtime_exists(&app.config),
                        )
                    {
                        "turbo".into()
                    } else {
                        "small-id".into()
                    }
                } else {
                    id
                };
                let config = app.config.clone();
                if let Ok(model) = crate::models::get(&id)
                    && crate::models::installed(&config, model).is_some()
                {
                    app.downloaded = Some(id);
                    s.set_can_use_downloaded(true);
                    s.set_download_status("This model is already installed. Use downloaded model, then save settings to select it.".into());
                    return;
                }
                match app.downloads.start(&config, &id) {
                    Ok(()) => {
                        app.downloaded = None;
                        s.set_downloading(true);
                        s.set_can_use_downloaded(false);
                        s.set_download_status("Connecting…".into());
                    }
                    Err(e) => s.set_download_status(e.to_string().into()),
                }
            }
        };
        if recommended {
            settings.on_download_recommended(callback);
        } else {
            settings.on_download_model(callback);
        }
    }
    {
        let app = app.clone();
        settings.on_cancel_download(move || app.borrow().downloads.cancel());
    }
    {
        let app = app.clone();
        let sw = settings.as_weak();
        settings.on_use_downloaded(move || {
            if let Some(s) = sw.upgrade()
                && let Some(id) = app.borrow().downloaded.as_ref()
            {
                if let Some(index) = model_ids().iter().position(|m| m == id) {
                    s.set_asr_model(index as i32);
                    model_details(&s, &app.borrow());
                }
            }
        });
    }
    let download_timer = Timer::default();
    {
        let app = app.clone();
        let sw = settings.as_weak();
        download_timer.start(TimerMode::Repeated,Duration::from_millis(100),move|| {
            let Some(s)=sw.upgrade() else{return;};let mut app=app.borrow_mut();
            if let Ok(gpu)=app.gpu_rx.try_recv(){app.gpu=gpu;model_list(&s,&app);model_details(&s,&app);}
            for event in app.downloads.events.try_iter().collect::<Vec<_>>() {
                match event {
                    crate::models::DownloadEvent::Progress{id,bytes,total} => s.set_download_status(format!("Downloading {}: {:.0}% ({:.0}/{:.0} MiB)",id,100.0*bytes as f64/total as f64,bytes as f64/1048576.0,total as f64/1048576.0).into()),
                    crate::models::DownloadEvent::Complete(id)=>{app.downloads.finish();app.downloaded=Some(id);s.set_downloading(false);s.set_can_use_downloaded(true);s.set_download_status("Download verified. Use downloaded model, then save settings to apply it to the next recording.".into());model_list(&s,&app);},
                    crate::models::DownloadEvent::Failed{id,error}=>{app.downloads.finish();s.set_downloading(false);s.set_download_status(format!("{id}: {error}. Select Download to retry.").into());},
                }
            }
            let status=app.pipeline.as_ref().map(|p|p.live_state()).map(|v|v.inference).unwrap_or_default();
            s.set_inference_status(if status.is_empty(){"Inference starts with the next recording".into()}else{format!("Active: {status}").into()});
        });
    }
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
        if let Some(window) = &app.speaker_window {
            theme(window, &app.config);
        }
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
    #[test]
    fn advanced_toml_cannot_replace_model_controls() {
        let config = super::advanced_config("indonesian_model = 'accidental-invalid-value'\nindonesian_processing = 'invalid'\nwhisper_model = 'my-custom.bin'").unwrap();
        assert_eq!(
            config.indonesian_model,
            crate::config::ModelSelection::Recommended
        );
        assert_eq!(
            config.indonesian_processing,
            crate::config::Processing::Auto
        );
        assert_eq!(
            config.whisper_model,
            std::path::PathBuf::from("my-custom.bin")
        );
    }

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
            downloads: crate::models::Downloads::default(),
            downloaded: None,
            gpu: crate::gpu::Discovery::default(),
            gpu_rx: crossbeam_channel::bounded(1).1,
            config: Config::default(),
            pipeline: None,
            keys: None,
            library: records,
            selected: None,
            speaker_window: None,
            editor: None,
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
    fn edit_action_remains_visible_with_compact_library() {
        let mut r = completed();
        r.title = "A long recording title that should shrink before the action buttons".into();
        r.segments.push(TranscriptSegment {
            recording_id: r.id.clone(),
            sequence: 0,
            start_ms: 0,
            end_ms: 1000,
            text: "Editable.".into(),
            clock_time: None,
        });
        let id = r.id.clone();
        let (ui, mut app) = fixture(vec![r]);
        select(&ui, &mut app, &id, true).unwrap();
        ui.window().set_size(slint::LogicalSize::new(800., 600.));
        ui.set_compact(true);
        ui.set_sidebar_open(true);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(500));
        let action = ElementHandle::find_by_accessible_label(&ui, "Edit transcript")
            .next()
            .unwrap();
        assert!(
            action.absolute_position().x + action.size().width <= 800.,
            "Edit action exceeds window: {:?} {:?}",
            action.absolute_position(),
            action.size()
        );
        assert!(action.absolute_position().y + action.size().height <= 600.);
    }
    #[test]
    fn transcript_editor_saves_search_export_and_reload_and_confirms_discard() {
        let (ui, app) = fixture(vec![]);
        let app = Rc::new(RefCell::new(app));
        let root = std::env::temp_dir().join(format!("edit-ui-{}", completed().id));
        app.borrow_mut().config.base_dir = root.clone();
        for saved in [false, true] {
            let mut r = completed();
            r.autosave = saved;
            r.exported = true;
            r.segments.push(TranscriptSegment {
                recording_id: r.id.clone(),
                sequence: 0,
                start_ms: 0,
                end_ms: 1000,
                text: "Original words.".into(),
                clock_time: None,
            });
            if saved {
                let mut store =
                    recording::RecordingStore::new(app.borrow().config.transcripts_dir());
                store.begin(&mut r).unwrap();
                store
                    .append(&PipelineEvent::Segment(r.segments[0].clone()))
                    .unwrap();
                store
                    .append(&PipelineEvent::State {
                        id: r.id.clone(),
                        status: r.status.clone(),
                        duration_ms: 1000,
                    })
                    .unwrap();
            }
            let id = r.id.clone();
            app.borrow_mut().library = vec![r];
            select(&ui, &mut app.borrow_mut(), &id, true).unwrap();
            assert!(ui.get_can_edit());
            open_editor(&ui, &app).unwrap();
            let window = app.borrow().editor.as_ref().unwrap().window.clone_strong();
            ElementHandle::find_by_accessible_label(&window, "Transcript passage [00:00:00]")
                .next()
                .unwrap()
                .set_accessible_value("Edited café 😊.");
            assert!(window.get_dirty());
            assert_eq!(
                window.get_passages().row_data(0).unwrap().text.as_str(),
                "Edited café 😊."
            );
            window.invoke_cancel();
            assert!(window.get_discard_prompt());
            window.set_discard_prompt(false);
            ui.set_query("café".into());
            window.invoke_save();
            assert!(window.get_error().is_empty(), "{}", window.get_error());
            assert!(!window.get_dirty());
            assert_eq!(ui.get_document().as_str(), "Edited café 😊.");
            assert_eq!(ui.get_matches().as_str(), "1 / 1");
            let full = app.borrow().selected.clone().unwrap();
            assert!(!full.exported);
            assert!(full.markdown().contains("Edited café 😊."));
            assert_eq!(
                recording::load_recording(&full).unwrap().text(false),
                "Edited café 😊."
            );
            open_editor(&ui, &app).unwrap();
            window.get_passages().set_row_data(
                0,
                EditorRow {
                    label: "[00:00:00]".into(),
                    text: "Discarded.".into(),
                },
            );
            window.set_dirty(true);
            begin_quit(&ui, &mut app.borrow_mut(), true);
            assert!(!app.borrow().quitting);
            assert!(window.get_error().contains("before quitting"));
            window.invoke_discard();
            assert!(!window.get_dirty());
            assert_eq!(ui.get_document().as_str(), "Edited café 😊.");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn speaker_controls_update_selection_find_and_survive_saved_reload() {
        use crate::speakers::{Speaker, SpeakerTurn};
        let (ui, app) = fixture(vec![]);
        let app = Rc::new(RefCell::new(app));
        let root = std::env::temp_dir().join(format!("speaker-ui-{}", completed().id));
        app.borrow_mut().config.base_dir = root.clone();
        app.borrow_mut().config.viewer_theme = ViewerTheme::Dark;
        for saved in [false, true] {
            let mut r = completed();
            r.autosave = saved;
            r.detect_speakers = true;
            r.speaker_finished = true;
            r.speakers = ["1", "2"]
                .iter()
                .map(|id| Speaker {
                    id: id.to_string(),
                    name: format!("Speaker {id}"),
                    embedding: vec![1., 0.],
                    observations: 1,
                    merged_into: None,
                })
                .collect();
            r.segments.push(TranscriptSegment {
                recording_id: r.id.clone(),
                sequence: 0,
                start_ms: 0,
                end_ms: 1000,
                text: "Hello reader".into(),
                clock_time: None,
            });
            r.speaker_turns.push(SpeakerTurn {
                start_ms: 0,
                end_ms: 1000,
                speaker_id: Some("1".into()),
            });
            if saved {
                let mut store =
                    recording::RecordingStore::new(app.borrow().config.transcripts_dir());
                store.begin(&mut r).unwrap();
                store
                    .append(&PipelineEvent::Segment(r.segments[0].clone()))
                    .unwrap();
            }
            let id = r.id.clone();
            app.borrow_mut().library = vec![r.clone()];
            select(&ui, &mut app.borrow_mut(), &id, true).unwrap();
            open_speakers(&ui, &app).unwrap();
            let window = app.borrow().speaker_window.as_ref().unwrap().clone_strong();
            assert!(window.global::<Theme>().get_dark());
            ui.set_query("Sarah".into());
            let old = ui.get_document().to_string();
            let at = old.find("Hello").unwrap();
            ui.invoke_preserve_selection(at as i32, (at + 5) as i32);
            window.invoke_rename_speaker("1".into(), "Sarah".into());
            assert!(window.get_error().is_empty(), "{}", window.get_error());
            assert_eq!(ui.get_document().as_str(), "Sarah: Hello reader");
            assert_eq!(ui.get_matches().as_str(), "1 / 1");
            assert_eq!(ui.get_selection_anchor(), 7);
            assert_eq!(ui.get_selection_cursor(), 12);
            window.invoke_reassign_turn(0, 2);
            assert_eq!(ui.get_document().as_str(), "Speaker 2: Hello reader");
            window.invoke_merge_speaker("2".into(), 0);
            assert_eq!(ui.get_document().as_str(), "Sarah: Hello reader");
            assert_eq!(window.get_speakers().row_count(), 1);
            let full = app.borrow().selected.as_ref().unwrap().clone();
            let restored = recording::load_recording(&full).unwrap();
            assert_eq!(restored.text(false), "Sarah: Hello reader");
            assert!(
                restored
                    .speakers
                    .iter()
                    .any(|s| s.id == "2" && s.merged_into.as_deref() == Some("1"))
            );
            assert_eq!(restored.speaker_overrides.len(), 1);
            forget_recording(&ui, &mut app.borrow_mut(), &id);
            assert!(!window.get_editing_enabled());
        }
        std::fs::remove_dir_all(root).unwrap();
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
            assert!(
                element.absolute_position().y + element.size().height <= 600.,
                "{label} is below the window"
            );
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

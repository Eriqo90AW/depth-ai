use super::*;
use crate::{
    config::Language,
    engine::{AsrResult, Word},
    logging::Logger,
    recording::{RecordingStore, TranscriptSegment, load_recording},
};
use std::{fs::OpenOptions, io::Write, sync::Arc};
fn profile(id: &str, name: &str, embedding: Vec<f32>) -> Speaker {
    Speaker {
        id: id.into(),
        name: name.into(),
        embedding,
        observations: 1,
        merged_into: None,
    }
}
fn fixture() -> Recording {
    let mut r = Recording::new(Language::En, false);
    r.detect_speakers = true;
    r.speaker_finished = true;
    r.status = RecordingStatus::Completed;
    r.speakers = vec![
        profile("speaker-1", "Speaker 1", vec![1., 0.]),
        profile("speaker-2", "Speaker 2", vec![0., 1.]),
    ];
    r.segments.push(TranscriptSegment {
        recording_id: r.id.clone(),
        sequence: 0,
        start_ms: 0,
        end_ms: 2000,
        text: "Hello! Yes, let's begin.".into(),
        clock_time: None,
    });
    let result = AsrResult {
        text: r.segments[0].text.clone(),
        words: vec![
            Word {
                word: "Hello!".into(),
                start: 0.,
                end: 0.8,
                ..Default::default()
            },
            Word {
                word: " Yes,".into(),
                start: 1.,
                end: 1.2,
                ..Default::default()
            },
            Word {
                word: " let's".into(),
                start: 1.2,
                end: 1.5,
                ..Default::default()
            },
            Word {
                word: " begin.".into(),
                start: 1.5,
                end: 2.,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    r.word_timings.insert(0, word_timings(&result, 0, 2000));
    r.speaker_turns = vec![
        SpeakerTurn {
            start_ms: 0,
            end_ms: 1000,
            speaker_id: Some("speaker-1".into()),
        },
        SpeakerTurn {
            start_ms: 1000,
            end_ms: 2000,
            speaker_id: Some("speaker-2".into()),
        },
    ];
    r.speaker_analyzed_ms = 2000;
    r
}
#[test]
fn mixed_chunk_splits_without_losing_text_or_punctuation() {
    let r = fixture();
    assert_eq!(
        r.text(false),
        "Speaker 1: Hello!\n\nSpeaker 2: Yes, let's begin."
    );
    assert_eq!(
        r.text(true),
        "[00:00:00] Speaker 1: Hello!\n\n[00:00:01] Speaker 2: Yes, let's begin."
    );
    assert!(r.markdown().contains(&r.text(false)));
}
#[test]
fn rename_updates_every_turn_and_export_without_editing_source() {
    let mut r = fixture();
    let original = r.segments[0].text.clone();
    let mut repeated = r.segments[0].clone();
    repeated.sequence = 1;
    repeated.start_ms = 3000;
    repeated.end_ms = 4000;
    repeated.text = "Hello again.".into();
    r.segments.push(repeated);
    r.speaker_turns.push(SpeakerTurn {
        start_ms: 3000,
        end_ms: 4000,
        speaker_id: Some("speaker-1".into()),
    });
    correct(
        &mut r,
        Change::Renamed {
            speaker_id: "speaker-1".into(),
            name: "Sarah".into(),
        },
        Path::new("unused"),
    )
    .unwrap();
    assert_eq!(r.text(false).matches("Sarah:").count(), 2);
    assert!(r.text(true).contains("Sarah:"));
    assert!(r.markdown().contains("Sarah:"));
    assert_eq!(r.segments[0].text, original);
}
#[test]
fn manual_assignment_survives_new_automatic_windows_and_merges() {
    let mut r = fixture();
    correct(
        &mut r,
        Change::Reassigned(Assignment {
            sequence: 0,
            word_start: 0,
            word_end: 1,
            speaker_id: Some("speaker-2".into()),
        }),
        Path::new("unused"),
    )
    .unwrap();
    apply(
        &mut r,
        &Change::Window {
            start_ms: 0,
            end_ms: 2000,
            turns: vec![SpeakerTurn {
                start_ms: 0,
                end_ms: 2000,
                speaker_id: Some("speaker-1".into()),
            }],
        },
    );
    assert!(r.text(false).starts_with("Speaker 2: Hello!"));
    correct(
        &mut r,
        Change::Merged {
            from: "speaker-2".into(),
            into: "speaker-1".into(),
        },
        Path::new("unused"),
    )
    .unwrap();
    assert_eq!(active_speakers(&r).len(), 1);
    assert!(!r.text(false).contains("Speaker 2"));
    apply(
        &mut r,
        &Change::Profile(profile("speaker-2", "Bad automatic name", vec![0., 1.])),
    );
    assert_eq!(active_speakers(&r).len(), 1);
}
#[test]
fn overlaps_and_missing_word_timings_stay_unknown() {
    let mut r = fixture();
    r.speaker_turns.push(SpeakerTurn {
        start_ms: 0,
        end_ms: 500,
        speaker_id: Some("speaker-2".into()),
    });
    assert!(r.text(false).starts_with("Unknown speaker: Hello!"));
    r.word_timings.clear();
    assert_eq!(r.text(false), "Unknown speaker: Hello! Yes, let's begin.");
}
#[test]
fn pending_labels_become_unknown_at_completion() {
    let mut r = fixture();
    r.speaker_turns.clear();
    r.speaker_finished = false;
    r.speaker_analyzed_ms = 0;
    assert!(r.text(false).contains("Identifying speaker"));
    apply(&mut r, &Change::Finished);
    assert!(r.text(false).contains("Unknown speaker"));
    assert!(!r.text(false).contains("Identifying"));
}
#[test]
fn manual_unknown_is_preserved() {
    let mut r = fixture();
    correct(
        &mut r,
        Change::Reassigned(Assignment {
            sequence: 0,
            word_start: 0,
            word_end: 1,
            speaker_id: None,
        }),
        Path::new("unused"),
    )
    .unwrap();
    assert!(r.text(false).starts_with("Unknown speaker: Hello!"));
}
#[test]
fn unicode_word_alignment_and_subword_tokens_preserve_bytes() {
    let a = AsrResult {
        text: " café, kepada Budi.".into(),
        words: vec![
            Word {
                word: " café".into(),
                start: 0.,
                end: 0.5,
                ..Default::default()
            },
            Word {
                word: ",".into(),
                start: 0.5,
                end: 0.6,
                ..Default::default()
            },
            Word {
                word: " kepa".into(),
                start: 0.6,
                end: 0.8,
                ..Default::default()
            },
            Word {
                word: "da".into(),
                start: 0.8,
                end: 1.,
                ..Default::default()
            },
            Word {
                word: " Budi.".into(),
                start: 1.,
                end: 1.5,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let w = word_timings(&a, 3000, 5000);
    assert_eq!(w.len(), 3);
    let t = a.text.trim();
    assert_eq!(&t[w[0].start_byte..w[0].end_byte], "café,");
    assert_eq!(&t[w[1].start_byte..w[1].end_byte], "kepada");
    assert_eq!(w[0].start_ms, 3000);
}
#[test]
fn invalid_times_and_embeddings_are_rejected() {
    let a = AsrResult {
        text: "Hi".into(),
        words: vec![Word {
            word: "Hi".into(),
            start: f32::NAN,
            end: 1.,
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(word_timings(&a, 0, 2000).is_empty());
    assert!(match_profile(&mut vec![], &[f32::NAN], true).is_none());
    assert_eq!(cosine(&[0.], &[0.]), -1.0);
}
#[test]
fn similar_profiles_and_weak_evidence_stay_unknown() {
    let mut p = vec![
        profile("a", "A", vec![1., 0.]),
        profile("b", "B", vec![1., 0.01]),
    ];
    assert!(match_profile(&mut p, &[1., 0.], true).is_none());
    assert_eq!(p.len(), 2);
    let mut p = vec![profile("a", "A", vec![1., 0.])];
    assert!(match_profile(&mut p, &[0.5, 0.866], true).is_none());
    assert_eq!(p.len(), 1);
    assert_eq!(match_profile(&mut p, &[1., 0.], true).as_deref(), Some("a"));
}
#[test]
fn selection_maps_to_same_source_text_after_labels_change() {
    let old = fixture();
    let mut new = old.clone();
    apply(
        &mut new,
        &Change::Renamed {
            speaker_id: "speaker-1".into(),
            name: "A much longer speaker name".into(),
        },
    );
    for timed in [false, true] {
        let old_text = old.text(timed);
        let new_text = new.text(timed);
        let start = old_text.find("Yes,").unwrap();
        let end = start + 4;
        let mapped_start = map_position(&old, &new, timed, start);
        let mapped_end = map_position(&old, &new, timed, end);
        assert_eq!(&new_text[mapped_start..mapped_end], "Yes,");
    }
}
#[test]
fn overlapping_windows_replace_ranges_without_erasing_neighbors() {
    let mut r = fixture();
    r.speaker_turns = vec![SpeakerTurn {
        start_ms: 0,
        end_ms: 10000,
        speaker_id: Some("speaker-1".into()),
    }];
    apply(
        &mut r,
        &Change::Window {
            start_ms: 5000,
            end_ms: 15000,
            turns: vec![SpeakerTurn {
                start_ms: 5000,
                end_ms: 15000,
                speaker_id: Some("speaker-2".into()),
            }],
        },
    );
    assert_eq!(r.speaker_turns.len(), 2);
    assert_eq!(r.speaker_turns[0].end_ms, 5000);
    assert_eq!(r.speaker_turns[1].end_ms, 15000);
}
#[test]
fn corrections_replay_after_restart_and_damaged_tail() {
    let dir = std::env::temp_dir().join(format!(
        "speaker-recovery-{}",
        Recording::new(Language::En, false).id
    ));
    let mut r = fixture();
    r.autosave = true;
    let mut store = RecordingStore::new(dir.clone());
    store.begin(&mut r).unwrap();
    let s = r.segments[0].clone();
    store.append(&PipelineEvent::Segment(s)).unwrap();
    correct(
        &mut r,
        Change::Renamed {
            speaker_id: "speaker-1".into(),
            name: "Sarah".into(),
        },
        &dir,
    )
    .unwrap();
    correct(
        &mut r,
        Change::Reassigned(Assignment {
            sequence: 0,
            word_start: 1,
            word_end: 4,
            speaker_id: Some("speaker-1".into()),
        }),
        &dir,
    )
    .unwrap();
    correct(
        &mut r,
        Change::Merged {
            from: "speaker-2".into(),
            into: "speaker-1".into(),
        },
        &dir,
    )
    .unwrap();
    let loaded = load_recording(&r.metadata()).unwrap();
    assert!(loaded.text(false).contains("Sarah:"));
    assert_eq!(loaded.speaker_overrides.len(), 1);
    assert_eq!(active_speakers(&loaded).len(), 1);
    let mut file = OpenOptions::new()
        .append(true)
        .open(r.source.as_ref().unwrap())
        .unwrap();
    file.write_all(b"{broken").unwrap();
    drop(file);
    let mut recovered = load_recording(&r.metadata()).unwrap();
    assert_eq!(recovered.status, RecordingStatus::Incomplete);
    assert!(recovered.speaker_finished);
    assert_eq!(recovered.speakers[0].name, "Sarah");
    drop(store);
    correct(
        &mut recovered,
        Change::Renamed {
            speaker_id: "speaker-1".into(),
            name: "Sarah corrected".into(),
        },
        &dir,
    )
    .unwrap();
    let durable = load_recording(&recovered.metadata()).unwrap();
    assert_eq!(durable.speakers[0].name, "Sarah corrected");
    assert_eq!(durable.speaker_overrides.len(), 1);
    let backup = recovered
        .source
        .as_ref()
        .unwrap()
        .with_extension("recovery-backup");
    assert!(backup.exists());
    assert!(std::fs::read(&backup).unwrap().ends_with(b"{broken"));
    assert_eq!(crate::recording::library(&dir).unwrap().len(), 1);
    crate::recording::delete(&durable, &dir).unwrap();
    assert!(!backup.exists());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn unsaved_corrections_create_no_files_and_recording_edits_are_rejected() {
    let dir = std::env::temp_dir().join(format!(
        "speaker-unsaved-{}",
        Recording::new(Language::En, false).id
    ));
    let mut r = fixture();
    correct(
        &mut r,
        Change::Renamed {
            speaker_id: "speaker-1".into(),
            name: "Sarah".into(),
        },
        &dir,
    )
    .unwrap();
    assert!(!dir.exists());
    r.status = RecordingStatus::Recording;
    assert!(
        correct(
            &mut r,
            Change::Merged {
                from: "speaker-2".into(),
                into: "speaker-1".into()
            },
            &dir
        )
        .is_err()
    );
}
#[test]
fn continued_recordings_keep_names_profiles_and_manual_assignments() {
    let dir = std::env::temp_dir().join(format!(
        "speaker-continue-{}",
        Recording::new(Language::En, false).id
    ));
    let mut r = fixture();
    r.autosave = true;
    r.duration_ms = 2000;
    let mut store = RecordingStore::new(dir.clone());
    store.begin(&mut r).unwrap();
    store
        .append(&PipelineEvent::Segment(r.segments[0].clone()))
        .unwrap();
    correct(
        &mut r,
        Change::Renamed {
            speaker_id: "speaker-1".into(),
            name: "Sarah".into(),
        },
        &dir,
    )
    .unwrap();
    correct(
        &mut r,
        Change::Reassigned(Assignment {
            sequence: 0,
            word_start: 1,
            word_end: 4,
            speaker_id: Some("speaker-1".into()),
        }),
        &dir,
    )
    .unwrap();
    let mut continued = load_recording(&r.metadata()).unwrap();
    let mut next = RecordingStore::new(dir.clone());
    next.resume(&mut continued).unwrap();
    assert!(!continued.speaker_finished);
    assert_eq!(continued.speakers[0].name, "Sarah");
    assert_eq!(continued.speaker_overrides.len(), 1);
    assert_eq!(
        match_profile(&mut continued.speakers, &[1., 0.], true).as_deref(),
        Some("speaker-1")
    );
    drop(next);
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn legacy_json_without_speaker_fields_keeps_original_rendering() {
    let mut r = fixture();
    r.detect_speakers = false;
    let mut json = serde_json::to_value(&r).unwrap();
    for key in [
        "detect_speakers",
        "speakers",
        "word_timings",
        "speaker_turns",
        "speaker_overrides",
        "speaker_analyzed_ms",
        "speaker_finished",
        "speaker_notice",
    ] {
        json.as_object_mut().unwrap().remove(key);
    }
    let mut old: Recording = serde_json::from_value(json).unwrap();
    old.segments = r.segments.clone();
    assert!(!old.detect_speakers);
    assert_eq!(old.text(false), "Hello! Yes, let's begin.");
}
#[test]
fn stale_recording_events_do_not_change_another_journal() {
    let dir = std::env::temp_dir().join(format!(
        "speaker-stale-{}",
        Recording::new(Language::En, false).id
    ));
    let mut r = fixture();
    r.autosave = true;
    let mut store = RecordingStore::new(dir.clone());
    store.begin(&mut r).unwrap();
    store
        .append(&PipelineEvent::Speaker {
            id: "different-recording".into(),
            change: Change::Renamed {
                speaker_id: "speaker-1".into(),
                name: "Wrong".into(),
            },
        })
        .unwrap();
    let loaded = load_recording(&r).unwrap();
    assert_eq!(loaded.speakers[0].name, "Speaker 1");
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn audio_windows_are_bounded_overlap_and_keep_stop_tail() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut input = worker::WindowInput::new(tx);
    input.push(&vec![0.1; 16000 * 12], 0);
    assert_eq!(input.finish(), 0);
    let windows: Vec<_> = rx.iter().collect();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].start_ms, 0);
    assert_eq!(windows[0].samples.len(), 160000);
    assert_eq!(windows[1].start_ms, 5000);
    assert_eq!(windows[1].samples.len(), 112000);
}
#[test]
fn queue_pressure_never_blocks_capture_and_stop_preserves_tail() {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let mut input = worker::WindowInput::new(tx);
    input.push(&vec![0.1; 16000 * 25], 0);
    assert!(input.skipped > 0);
    let first = rx.recv().unwrap();
    assert_eq!(first.start_ms, 0);
    let skipped = input.finish();
    assert!(skipped > 0);
    let tail = rx.recv().unwrap();
    assert_eq!(tail.start_ms, 20000);
    assert_eq!(tail.samples.len(), 80000);
}
struct MockDiarizer {
    calls: usize,
    fail_once: bool,
}
impl worker::Diarizer for MockDiarizer {
    fn analyze(&mut self, _: &[f32]) -> anyhow::Result<worker::Detection> {
        self.calls += 1;
        if self.fail_once && self.calls == 1 {
            anyhow::bail!("injected failure");
        }
        let c = if self.calls % 2 == 1 { 9 } else { 3 };
        Ok(worker::Detection {
            turns: vec![worker::LocalTurn {
                start_ms: 0,
                end_ms: 5000,
                cluster: c,
            }],
            embeddings: std::collections::BTreeMap::from([(c, vec![1., 0.])]),
        })
    }
}
#[test]
fn backend_cluster_numbers_never_become_global_ids() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let (out, updates) = crossbeam_channel::unbounded();
    tx.send(worker::AudioWindow {
        start_ms: 0,
        samples: vec![0.; 160000],
    })
    .unwrap();
    tx.send(worker::AudioWindow {
        start_ms: 5000,
        samples: vec![0.; 160000],
    })
    .unwrap();
    drop(tx);
    worker::run(
        Box::new(MockDiarizer {
            calls: 0,
            fail_once: false,
        }),
        vec![],
        0,
        rx,
        out,
        Arc::new(Logger::disabled()),
    );
    let ids: Vec<_> = updates
        .iter()
        .filter_map(|u| match u {
            worker::Update::Change(Change::Window { turns, .. }) => {
                Some(turns[0].speaker_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ids,
        vec![Some("speaker-1".into()), Some("speaker-1".into())]
    );
}
#[test]
fn speaker_failure_is_nonfatal_and_worker_finishes() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let (out, updates) = crossbeam_channel::unbounded();
    for at in [0, 5000] {
        tx.send(worker::AudioWindow {
            start_ms: at,
            samples: vec![0.; 160000],
        })
        .unwrap();
    }
    drop(tx);
    worker::run(
        Box::new(MockDiarizer {
            calls: 0,
            fail_once: true,
        }),
        vec![],
        0,
        rx,
        out,
        Arc::new(Logger::disabled()),
    );
    let results: Vec<_> = updates.iter().collect();
    assert!(
        results
            .iter()
            .any(|u| matches!(u, worker::Update::Notice(_)))
    );
    assert!(
        results
            .iter()
            .any(|u| matches!(u, worker::Update::Change(Change::Window { .. })))
    );
    assert!(matches!(
        results.last(),
        Some(worker::Update::Change(Change::Finished))
    ));
}
#[test]
fn missing_models_leave_a_finished_unknown_transcript() {
    let config = crate::config::Config {
        speaker_segmentation_model: "does-not-exist-speakers.onnx".into(),
        ..Default::default()
    };
    let mut session = worker::start(config, vec![], 0, Arc::new(Logger::disabled()));
    drop(session.input.take());
    let results: Vec<_> = session.updates.iter().collect();
    session.thread.join().unwrap();
    assert!(
        results
            .iter()
            .any(|u| matches!(u, worker::Update::Notice(_)))
    );
    assert!(matches!(
        results.last(),
        Some(worker::Update::Change(Change::Finished))
    ));
}

#[test]
fn merged_voice_exemplars_reuse_the_chosen_identity_on_continuation() {
    let mut r = fixture();
    apply(
        &mut r,
        &Change::Merged {
            from: "speaker-2".into(),
            into: "speaker-1".into(),
        },
    );
    assert_eq!(
        match_profile(&mut r.speakers, &[0., 1.], true).as_deref(),
        Some("speaker-1")
    );
    assert_eq!(
        match_profile(&mut r.speakers, &[1., 0.], true).as_deref(),
        Some("speaker-1")
    );
    assert_eq!(r.speakers.len(), 2);
}
#[test]
fn indexed_intervals_preserve_overlap_across_long_history() {
    let mut r = fixture();
    r.speaker_turns.reverse();
    for n in 1..20000 {
        r.speaker_turns.push(SpeakerTurn {
            start_ms: n * 2000,
            end_ms: n * 2000 + 1000,
            speaker_id: Some("speaker-1".into()),
        });
    }
    assert_eq!(
        r.text(false),
        "Speaker 1: Hello!\n\nSpeaker 2: Yes, let's begin."
    );
    let index = TurnIndex::new(&r);
    assert_eq!(index.overlapping(300, 800).len(), 1);
    assert_eq!(index.overlapping(2100, 2300).len(), 1);
}

#[test]
fn missing_lexical_timings_do_not_borrow_a_neighboring_speaker() {
    let mut r = fixture();
    let result = AsrResult {
        text: r.segments[0].text.clone(),
        words: vec![Word {
            word: "Hello!".into(),
            start: 0.,
            end: 0.8,
            ..Default::default()
        }],
        ..Default::default()
    };
    r.word_timings.insert(0, word_timings(&result, 0, 2000));
    assert_eq!(
        r.text(false),
        "Speaker 1: Hello!\n\nUnknown speaker: Yes, let's begin."
    );
}

#[test]
fn missing_subword_timings_keep_the_whole_word_unknown() {
    let mut r = fixture();
    r.segments[0].text = "Hello incredible".into();
    let result = AsrResult {
        text: r.segments[0].text.clone(),
        words: vec![
            Word {
                word: "Hello".into(),
                start: 0.,
                end: 0.4,
                ..Default::default()
            },
            Word {
                word: " incre".into(),
                start: 0.4,
                end: 0.8,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    r.word_timings.insert(0, word_timings(&result, 0, 2000));
    assert_eq!(
        r.text(false),
        "Speaker 1: Hello\n\nUnknown speaker: incredible"
    );
}
struct ScriptedDiarizer(std::collections::VecDeque<worker::Detection>);
impl worker::Diarizer for ScriptedDiarizer {
    fn analyze(&mut self, _: &[f32]) -> anyhow::Result<worker::Detection> {
        Ok(self.0.pop_front().unwrap())
    }
}
#[test]
fn several_speakers_returning_voices_and_unembedded_short_replies() {
    let first = worker::Detection {
        turns: vec![
            worker::LocalTurn {
                start_ms: 0,
                end_ms: 3000,
                cluster: 90,
            },
            worker::LocalTurn {
                start_ms: 3000,
                end_ms: 6000,
                cluster: 4,
            },
            worker::LocalTurn {
                start_ms: 6000,
                end_ms: 9000,
                cluster: 12,
            },
        ],
        embeddings: std::collections::BTreeMap::from([
            (90, vec![1., 0., 0.]),
            (4, vec![0., 1., 0.]),
            (12, vec![0., 0., 1.]),
        ]),
    };
    let second = worker::Detection {
        turns: vec![
            worker::LocalTurn {
                start_ms: 0,
                end_ms: 3000,
                cluster: 2,
            },
            worker::LocalTurn {
                start_ms: 3000,
                end_ms: 3500,
                cluster: 88,
            },
        ],
        embeddings: std::collections::BTreeMap::from([(2, vec![1., 0., 0.])]),
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let (out, updates) = crossbeam_channel::unbounded();
    for at in [0, 10000] {
        tx.send(worker::AudioWindow {
            start_ms: at,
            samples: vec![0.; 160000],
        })
        .unwrap();
    }
    drop(tx);
    worker::run(
        Box::new(ScriptedDiarizer([first, second].into())),
        vec![],
        0,
        rx,
        out,
        Arc::new(Logger::disabled()),
    );
    let windows: Vec<_> = updates
        .iter()
        .filter_map(|u| match u {
            worker::Update::Change(Change::Window { turns, .. }) => {
                Some(turns.into_iter().map(|t| t.speaker_id).collect::<Vec<_>>())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        windows[0],
        vec![
            Some("speaker-1".into()),
            Some("speaker-2".into()),
            Some("speaker-3".into())
        ]
    );
    assert_eq!(windows[1], vec![Some("speaker-1".into()), None]);
}

#[test]
fn stop_refines_uncertain_intervals_clipped_by_later_windows() {
    let first = worker::Detection {
        turns: vec![worker::LocalTurn {
            start_ms: 0,
            end_ms: 10000,
            cluster: 1,
        }],
        embeddings: std::collections::BTreeMap::from([(1, vec![0.5, 0.866])]),
    };
    let second = worker::Detection {
        turns: vec![worker::LocalTurn {
            start_ms: 0,
            end_ms: 10000,
            cluster: 5,
        }],
        embeddings: std::collections::BTreeMap::from([(5, vec![0., 1.])]),
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let (out, updates) = crossbeam_channel::unbounded();
    for at in [0, 5000] {
        tx.send(worker::AudioWindow {
            start_ms: at,
            samples: vec![0.; 160000],
        })
        .unwrap();
    }
    drop(tx);
    let initial = vec![profile("speaker-1", "A", vec![1., 0.])];
    worker::run(
        Box::new(ScriptedDiarizer([first, second].into())),
        initial.clone(),
        0,
        rx,
        out,
        Arc::new(Logger::disabled()),
    );
    let mut r = fixture();
    r.speakers = initial;
    r.speaker_turns.clear();
    for update in updates {
        if let worker::Update::Change(change) = update {
            apply(&mut r, &change);
        }
    }
    assert!(r.speaker_finished);
    assert!(
        r.speaker_turns
            .iter()
            .any(|t| t.start_ms == 0 && t.end_ms == 5000)
    );
    assert!(
        r.speaker_turns
            .iter()
            .all(|t| t.speaker_id.as_deref() == Some("speaker-2"))
    );
}

#[test]
fn speaker_edits_require_finished_analysis_even_with_completed_asr() {
    let mut r = fixture();
    r.speaker_finished = false;
    assert!(
        correct(
            &mut r,
            Change::Renamed {
                speaker_id: "speaker-1".into(),
                name: "Sarah".into()
            },
            Path::new("unused")
        )
        .is_err()
    );
}

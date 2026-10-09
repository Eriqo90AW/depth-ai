//! User corrections to finished transcripts, retaining passage timing and attribution.
use crate::{
    recording::{self, PipelineEvent, Recording, TranscriptSegment},
    speakers::{self, Assignment, WordTiming},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Passage {
    pub sequence: u64,
    pub start: usize,
    pub end: usize,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: Option<String>,
    pub label: String,
    pub text: String,
}
#[derive(Debug, Clone)]
pub struct Request {
    pub basis: String,
    pub texts: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditedSegment {
    pub segment: TranscriptSegment,
    pub words: Vec<WordTiming>,
    pub assignments: Vec<Assignment>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub segments: Vec<EditedSegment>,
    pub preview: String,
}
pub fn available(r: &Recording) -> bool {
    r.can_delete() && (!r.detect_speakers || r.speaker_finished) && !r.segments.is_empty()
}
/// Include source text and speaker decisions to reject drafts made before continuation or corrections.
pub fn basis(r: &Recording) -> String {
    let bytes = serde_json::to_vec(&(
        &r.segments,
        &r.word_timings,
        &r.speaker_turns,
        &r.speaker_overrides,
        &r.speakers,
    ))
    .expect("serializable recording");
    format!("{:x}", Sha256::digest(bytes))
}
pub fn passages(r: &Recording) -> Vec<Passage> {
    if r.detect_speakers {
        speakers::display_turns(r)
            .into_iter()
            .map(|t| {
                let segment = r
                    .segments
                    .iter()
                    .find(|s| s.sequence == t.sequence)
                    .expect("turn source");
                let end_ms = r
                    .word_timings
                    .get(&t.sequence)
                    .and_then(|w| w.get(t.word_end.saturating_sub(1)))
                    .map(|w| w.end_ms)
                    .unwrap_or(segment.end_ms);
                Passage {
                    sequence: t.sequence,
                    start: t.source_start,
                    end: t.source_end,
                    start_ms: t.start_ms,
                    end_ms,
                    speaker_id: t.speaker_id,
                    label: format!("[{}] {}", recording::timestamp(t.start_ms), t.label),
                    text: t.text,
                }
            })
            .collect()
    } else {
        r.segments
            .iter()
            .map(|s| Passage {
                sequence: s.sequence,
                start: 0,
                end: s.text.len(),
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                speaker_id: None,
                label: format!(
                    "[{}]",
                    s.clock_time
                        .clone()
                        .unwrap_or_else(|| recording::timestamp(s.start_ms))
                ),
                text: s.text.clone(),
            })
            .collect()
    }
}
pub fn apply(r: &mut Recording, correction: &Correction) {
    for edited in &correction.segments {
        if let Some(segment) = r
            .segments
            .iter_mut()
            .find(|s| s.sequence == edited.segment.sequence)
        {
            *segment = edited.segment.clone();
        }
        r.word_timings
            .insert(edited.segment.sequence, edited.words.clone());
        r.speaker_overrides
            .retain(|a| a.sequence != edited.segment.sequence);
        r.speaker_overrides.extend(edited.assignments.clone());
    }
    r.preview = correction.preview.clone();
    r.exported = false;
}
pub fn correct(r: &mut Recording, request: Request, dir: &Path) -> io::Result<()> {
    if !available(r) {
        return Err(io::Error::other(
            "Wait for transcription and speaker detection to finish before editing.",
        ));
    }
    if request.basis != basis(r) {
        return Err(io::Error::other(
            "This recording changed while you were editing. Copy your edits, then reopen the editor.",
        ));
    }
    let passages = passages(r);
    if request.texts.len() != passages.len() {
        return Err(io::Error::other(
            "The transcript passages changed. Reopen the editor.",
        ));
    }
    let mut changes: BTreeMap<u64, Vec<(&Passage, &String)>> = BTreeMap::new();
    for (passage, text) in passages.iter().zip(&request.texts) {
        changes
            .entry(passage.sequence)
            .or_default()
            .push((passage, text));
    }
    let mut correction = Correction {
        segments: vec![],
        preview: String::new(),
    };
    for (sequence, parts) in changes {
        if parts.iter().all(|(p, text)| p.text == **text) {
            continue;
        }
        let mut segment = r
            .segments
            .iter()
            .find(|s| s.sequence == sequence)
            .expect("passage source")
            .clone();
        let source = segment.text.clone();
        let mut text = String::new();
        let mut words = vec![];
        let mut assignments = vec![];
        let mut cursor = 0;
        for (passage, replacement) in parts {
            text.push_str(&source[cursor..passage.start]);
            let start_byte = text.len();
            text.push_str(replacement);
            let word_start = words.len();
            if replacement == &passage.text {
                // Keep original word alignment in untouched passages, shifting only source bytes.
                words.extend(
                    r.word_timings
                        .get(&sequence)
                        .into_iter()
                        .flatten()
                        .filter(|w| {
                            w.start_byte >= passage.start
                                && w.end_byte <= passage.end
                                && w.start_byte <= w.end_byte
                        })
                        .map(|w| WordTiming {
                            start_byte: start_byte + w.start_byte - passage.start,
                            end_byte: start_byte + w.end_byte - passage.start,
                            start_ms: w.start_ms,
                            end_ms: w.end_ms,
                        }),
                );
            }
            if words.len() == word_start {
                // Manual text has no ASR word alignment. Retain its original passage anchor.
                words.push(WordTiming {
                    start_byte,
                    end_byte: text.len(),
                    start_ms: passage.start_ms,
                    end_ms: passage.end_ms,
                });
            }
            assignments.push(Assignment {
                sequence,
                word_start,
                word_end: words.len(),
                speaker_id: passage.speaker_id.clone(),
            });
            cursor = passage.end;
        }
        text.push_str(&source[cursor..]);
        segment.text = text;
        correction.segments.push(EditedSegment {
            segment,
            words,
            assignments,
        });
    }
    if correction.segments.is_empty() {
        return Ok(());
    }
    let mut next = r.clone();
    apply(&mut next, &correction);
    correction.preview = next
        .segments
        .iter()
        .flat_map(|s| s.text.split_whitespace())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(110)
        .collect();
    next.preview = correction.preview.clone();
    if r.legacy_session.is_some() {
        // Each imported session owns its corrections; never overwrite the shared legacy document.
        let path = legacy_path(r)?;
        std::fs::create_dir_all(path.parent().expect("edits directory"))?;
        let all = Correction {
            segments: next
                .segments
                .iter()
                .map(|s| EditedSegment {
                    segment: s.clone(),
                    words: vec![],
                    assignments: vec![],
                })
                .collect(),
            preview: next.preview.clone(),
        };
        recording::atomic_write(&path, &serde_json::to_vec(&all)?)?;
        *r = next;
        return Ok(());
    }
    recording::save_correction(
        r,
        next,
        PipelineEvent::TranscriptEdited {
            id: r.id.clone(),
            correction,
        },
        dir,
    )
}
fn legacy_path(r: &Recording) -> io::Result<PathBuf> {
    let source = r
        .source
        .as_ref()
        .ok_or_else(|| io::Error::other("Legacy source is missing"))?;
    let mut components = Path::new(&r.id).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(io::Error::other("Invalid recording identity"));
    }
    Ok(source
        .parent()
        .ok_or_else(|| io::Error::other("Legacy folder is missing"))?
        .join(".events")
        .join(format!("{}.edits.json", r.id)))
}
pub(crate) fn load_legacy_edits(r: &mut Recording) -> io::Result<()> {
    match std::fs::read(legacy_path(r)?) {
        Ok(bytes) => {
            let correction = serde_json::from_slice::<Correction>(&bytes)?;
            apply(r, &correction);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Language,
        recording::{RecordingStatus, RecordingStore},
        speakers::{Change, Speaker, SpeakerTurn},
    };
    use std::fs;
    fn sample() -> Recording {
        let mut r = Recording::new(Language::Id, false);
        r.status = RecordingStatus::Completed;
        r.duration_ms = 4000;
        r.segments = vec![TranscriptSegment {
            recording_id: r.id.clone(),
            sequence: 7,
            start_ms: 0,
            end_ms: 4000,
            text: "Selamat pagi. Terima kasih.".into(),
            clock_time: None,
        }];
        r.exported = true;
        r
    }
    fn request(r: &Recording, texts: &[&str]) -> Request {
        Request {
            basis: basis(r),
            texts: texts.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn save(r: &mut Recording, dir: &Path) {
        r.autosave = true;
        let mut store = RecordingStore::new(dir.to_path_buf());
        store.begin(r).unwrap();
        for segment in &r.segments {
            store
                .append(&PipelineEvent::Segment(segment.clone()))
                .unwrap();
        }
        if r.detect_speakers {
            for speaker in &r.speakers {
                store
                    .append(&PipelineEvent::Speaker {
                        id: r.id.clone(),
                        change: Change::Profile(speaker.clone()),
                    })
                    .unwrap();
            }
            for (sequence, words) in &r.word_timings {
                store
                    .append(&PipelineEvent::Speaker {
                        id: r.id.clone(),
                        change: Change::Timings {
                            sequence: *sequence,
                            words: words.clone(),
                        },
                    })
                    .unwrap();
            }
            store
                .append(&PipelineEvent::Speaker {
                    id: r.id.clone(),
                    change: Change::Window {
                        start_ms: 0,
                        end_ms: r.duration_ms,
                        turns: r.speaker_turns.clone(),
                    },
                })
                .unwrap();
        }
        store
            .append(&PipelineEvent::State {
                id: r.id.clone(),
                status: r.status.clone(),
                duration_ms: r.duration_ms,
            })
            .unwrap();
        store.render(r, true).unwrap();
    }
    #[test]
    fn saved_unicode_corrections_survive_reload_export_preview_and_continuation() {
        let mut r = sample();
        let dir = std::env::temp_dir().join(&r.id);
        save(&mut r, &dir);
        let edit = request(&r, &["Selamat siang — café 😊. Terima kasih!"]);
        correct(&mut r, edit, &dir).unwrap();
        assert!(!r.exported);
        let meta = recording::library(&dir).unwrap().remove(0);
        assert!(meta.preview.contains("café 😊"));
        let mut loaded = recording::load_recording(&meta).unwrap();
        assert_eq!(loaded.text(false), "Selamat siang — café 😊. Terima kasih!");
        assert_eq!(
            loaded.text(true),
            "[00:00:00] Selamat siang — café 😊. Terima kasih!"
        );
        assert!(
            fs::read_to_string(dir.join(format!("{}.md", r.id)))
                .unwrap()
                .contains("café 😊")
        );
        assert_eq!(loaded.segments[0].end_ms, 4000);
        let mut store = RecordingStore::new(dir.clone());
        store.resume(&mut loaded).unwrap();
        let segment = TranscriptSegment {
            recording_id: r.id.clone(),
            sequence: 8,
            start_ms: 4000,
            end_ms: 5000,
            text: "Lanjut.".into(),
            clock_time: None,
        };
        store.append(&PipelineEvent::Segment(segment)).unwrap();
        store
            .append(&PipelineEvent::State {
                id: r.id.clone(),
                status: RecordingStatus::Completed,
                duration_ms: 5000,
            })
            .unwrap();
        drop(store);
        assert_eq!(
            recording::load_recording(&meta).unwrap().text(false),
            "Selamat siang — café 😊. Terima kasih! Lanjut."
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn speaker_passages_retain_labels_and_times_after_length_changes_and_deletion() {
        let mut r = sample();
        r.detect_speakers = true;
        r.speaker_finished = true;
        r.speakers = ["1", "2"]
            .iter()
            .map(|id| Speaker {
                id: (*id).into(),
                name: format!("Speaker {id}"),
                embedding: vec![1., 0.],
                observations: 1,
                merged_into: None,
            })
            .collect();
        r.word_timings.insert(
            7,
            vec![
                WordTiming {
                    start_byte: 0,
                    end_byte: 13,
                    start_ms: 0,
                    end_ms: 1900,
                },
                WordTiming {
                    start_byte: 14,
                    end_byte: 27,
                    start_ms: 2000,
                    end_ms: 4000,
                },
            ],
        );
        r.word_timings.get_mut(&7).unwrap()[1].end_byte = r.segments[0].text.len();
        r.speaker_turns = vec![
            SpeakerTurn {
                start_ms: 0,
                end_ms: 2000,
                speaker_id: Some("1".into()),
            },
            SpeakerTurn {
                start_ms: 2000,
                end_ms: 4000,
                speaker_id: Some("2".into()),
            },
        ];
        let dir = std::env::temp_dir().join(&r.id);
        save(&mut r, &dir);
        assert_eq!(passages(&r).len(), 2);
        let edit = request(
            &r,
            &["Halo José 😊, selamat datang.", "Terima kasih banyak!"],
        );
        correct(&mut r, edit, &dir).unwrap();
        assert_eq!(
            r.text(true),
            "[00:00:00] Speaker 1: Halo José 😊, selamat datang.\n\n[00:00:02] Speaker 2: Terima kasih banyak!"
        );
        let mut loaded = recording::load_recording(&r).unwrap();
        assert_eq!(loaded.text(true), r.text(true));
        let edit = request(&loaded, &["", "Jawaban berubah."]);
        correct(&mut loaded, edit, &dir).unwrap();
        assert!(
            loaded
                .text(true)
                .contains("[00:00:02] Speaker 2: Jawaban berubah.")
        );
        assert_eq!(passages(&loaded).len(), 2);
        let edit = request(&loaded, &["Restored first passage.", "Jawaban berubah."]);
        correct(&mut loaded, edit, &dir).unwrap();
        assert!(
            loaded
                .text(true)
                .contains("Speaker 1: Restored first passage.")
        );
        assert!(loaded.segments[0].text.contains("passage. Jawaban"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn untouched_speaker_passage_keeps_individual_word_times() {
        let mut r = sample();
        r.detect_speakers = true;
        r.speaker_finished = true;
        r.speakers = ["1", "2"]
            .iter()
            .map(|id| Speaker {
                id: (*id).into(),
                name: format!("Speaker {id}"),
                embedding: vec![1., 0.],
                observations: 1,
                merged_into: None,
            })
            .collect();
        r.word_timings.insert(
            7,
            vec![
                WordTiming {
                    start_byte: 0,
                    end_byte: 7,
                    start_ms: 0,
                    end_ms: 800,
                },
                WordTiming {
                    start_byte: 8,
                    end_byte: 13,
                    start_ms: 800,
                    end_ms: 1900,
                },
                WordTiming {
                    start_byte: 14,
                    end_byte: 20,
                    start_ms: 2000,
                    end_ms: 2800,
                },
                WordTiming {
                    start_byte: 21,
                    end_byte: 27,
                    start_ms: 2800,
                    end_ms: 4000,
                },
            ],
        );
        r.speaker_turns = vec![
            SpeakerTurn {
                start_ms: 0,
                end_ms: 2000,
                speaker_id: Some("1".into()),
            },
            SpeakerTurn {
                start_ms: 2000,
                end_ms: 4000,
                speaker_id: Some("2".into()),
            },
        ];
        let edit = request(&r, &["Halo semuanya!", "Terima kasih."]);
        correct(&mut r, edit, Path::new("unused")).unwrap();
        let words = &r.word_timings[&7];
        assert_eq!(words.len(), 3);
        assert_eq!((words[1].start_ms, words[1].end_ms), (2000, 2800));
        assert_eq!((words[2].start_ms, words[2].end_ms), (2800, 4000));
        assert_eq!(
            &r.segments[0].text[words[1].start_byte..words[1].end_byte],
            "Terima"
        );
        assert_eq!(
            r.text(false),
            "Speaker 1: Halo semuanya!\n\nSpeaker 2: Terima kasih."
        );
    }
    #[test]
    fn stale_live_and_unfinished_edits_are_rejected_without_writes() {
        let mut r = sample();
        let edit = request(&r, &["Correction"]);
        let dir = std::env::temp_dir().join(&r.id);
        r.segments[0].text = "Changed elsewhere".into();
        assert!(
            correct(&mut r, edit, &dir)
                .unwrap_err()
                .to_string()
                .contains("changed while")
        );
        for status in [RecordingStatus::Recording, RecordingStatus::Processing] {
            r.status = status;
            let edit = request(&r, &["Correction"]);
            assert!(correct(&mut r, edit, &dir).is_err());
        }
        r.status = RecordingStatus::Completed;
        r.detect_speakers = true;
        let edit = request(&r, &["Correction"]);
        assert!(correct(&mut r, edit, &dir).is_err());
        assert_eq!(r.segments[0].text, "Changed elsewhere");
        assert!(!dir.exists());
    }
    #[test]
    fn unsaved_and_noop_edits_do_not_create_files_and_empty_text_survives() {
        let mut r = sample();
        let dir = std::env::temp_dir().join(&r.id);
        let edit = request(&r, &[&r.segments[0].text]);
        correct(&mut r, edit, &dir).unwrap();
        assert!(r.exported);
        let edit = request(&r, &[""]);
        correct(&mut r, edit, &dir).unwrap();
        assert_eq!(r.text(false), "");
        assert!(!r.exported);
        assert!(!dir.exists());
        let edit = request(&r, &["Kembali."]);
        correct(&mut r, edit, &dir).unwrap();
        assert_eq!(r.text(false), "Kembali.");
    }
    #[test]
    fn legacy_edits_are_session_specific_and_leave_shared_source_unchanged() {
        let dir = std::env::temp_dir().join(sample().id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript-2025-06-01.md");
        let original = "# Transcript\n\n## Session 1 — 23:58 — English\n\n**23:58:40**  Original words.\n\n## Session 2 — 23:59 — Indonesian\n\n**23:59:10**  Selamat malam.\n";
        fs::write(&path, original).unwrap();
        let entries = recording::library(&dir).unwrap();
        let mut r = recording::load_recording(&entries[0]).unwrap();
        let edit = request(&r, &["Selamat malam semuanya."]);
        correct(&mut r, edit, &dir).unwrap();
        let reloaded = recording::load_recording(&r).unwrap();
        assert_eq!(reloaded.text(true), "[23:59:10] Selamat malam semuanya.");
        assert_eq!(
            recording::load_recording(&entries[1]).unwrap().text(false),
            "Original words."
        );
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert_eq!(
            recording::library(&dir).unwrap()[0].preview,
            "Selamat malam semuanya."
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn damaged_tail_is_preserved_and_correction_is_replayable() {
        use std::io::Write;
        let mut r = sample();
        let dir = std::env::temp_dir().join(&r.id);
        save(&mut r, &dir);
        let path = r.source.as_ref().unwrap().clone();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{broken")
            .unwrap();
        r = recording::load_recording(&r).unwrap();
        let edit = request(&r, &["Recovered correction."]);
        correct(&mut r, edit, &dir).unwrap();
        assert_eq!(
            recording::load_recording(&r).unwrap().text(false),
            "Recovered correction."
        );
        assert!(
            fs::read_to_string(path.with_extension("recovery-backup"))
                .unwrap()
                .ends_with("{broken")
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

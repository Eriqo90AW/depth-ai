//! Recording-local speaker identities, timed rendering, and correction events.
use crate::{
    engine::AsrResult,
    recording::{PipelineEvent, Recording, RecordingStatus, timestamp},
};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(feature = "speakers")]
pub mod backend;
pub mod worker;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Speaker {
    pub id: String,
    pub name: String,
    pub embedding: Vec<f32>,
    pub observations: u32,
    #[serde(default)]
    pub merged_into: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WordTiming {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_ms: u64,
    pub end_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeakerTurn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Assignment {
    pub sequence: u64,
    pub word_start: usize,
    pub word_end: usize,
    pub speaker_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Change {
    Profile(Speaker),
    Timings {
        sequence: u64,
        words: Vec<WordTiming>,
    },
    Window {
        start_ms: u64,
        end_ms: u64,
        turns: Vec<SpeakerTurn>,
    },
    Refined(Vec<SpeakerTurn>),
    Finished,
    Resumed,
    Notice(String),
    Renamed {
        speaker_id: String,
        name: String,
    },
    Merged {
        from: String,
        into: String,
    },
    Reassigned(Assignment),
}

pub fn apply(r: &mut Recording, change: &Change) {
    match change {
        Change::Profile(profile) => {
            if let Some(s) = r.speakers.iter_mut().find(|s| s.id == profile.id) {
                // Names and aliases are user-owned. Automatic updates only change voice data.
                s.embedding.clone_from(&profile.embedding);
                s.observations = profile.observations;
            } else {
                r.speakers.push(profile.clone());
            }
        }
        Change::Timings { sequence, words } => {
            r.word_timings.insert(*sequence, words.clone());
        }
        Change::Window {
            start_ms,
            end_ms,
            turns,
        } => {
            // Replace only this window. Clip older turns crossing its edges.
            let mut kept = vec![];
            for t in &r.speaker_turns {
                if t.end_ms <= *start_ms || t.start_ms >= *end_ms {
                    kept.push(t.clone());
                } else {
                    if t.start_ms < *start_ms {
                        let mut v = t.clone();
                        v.end_ms = *start_ms;
                        kept.push(v);
                    }
                    if t.end_ms > *end_ms {
                        let mut v = t.clone();
                        v.start_ms = *end_ms;
                        kept.push(v);
                    }
                }
            }
            kept.extend(turns.iter().filter(|t| t.end_ms > t.start_ms).cloned());
            kept.sort_by_key(|t| (t.start_ms, t.end_ms));
            r.speaker_turns = kept;
            r.speaker_analyzed_ms = r.speaker_analyzed_ms.max(*end_ms);
        }
        Change::Refined(turns) => {
            for resolved in turns {
                for t in &mut r.speaker_turns {
                    if t.speaker_id.is_none()
                        && t.start_ms == resolved.start_ms
                        && t.end_ms == resolved.end_ms
                    {
                        t.speaker_id.clone_from(&resolved.speaker_id);
                    }
                }
            }
        }
        Change::Finished => r.speaker_finished = true,
        Change::Resumed => {
            r.speaker_finished = false;
            r.speaker_notice.clear();
        }
        Change::Notice(message) => r.speaker_notice.clone_from(message),
        Change::Renamed { speaker_id, name } => {
            if let Some(s) = r.speakers.iter_mut().find(|s| s.id == *speaker_id) {
                s.name.clone_from(name);
            }
        }
        Change::Merged { from, into } => {
            if let Some(source) = r.speakers.iter().find(|s| s.id == *from).cloned() {
                if let Some(target) = r.speakers.iter_mut().find(|s| s.id == *into) {
                    if target.embedding.len() == source.embedding.len()
                        && !target.embedding.is_empty()
                    {
                        for (a, b) in target.embedding.iter_mut().zip(&source.embedding) {
                            *a = (*a + b) / 2.0;
                        }
                        normalize(&mut target.embedding);
                        target.observations =
                            target.observations.saturating_add(source.observations);
                    }
                }
                if let Some(s) = r.speakers.iter_mut().find(|s| s.id == *from) {
                    s.merged_into = Some(into.clone());
                }
            }
        }
        Change::Reassigned(assignment) => r.speaker_overrides.push(assignment.clone()),
    }
    r.exported = false;
}

pub fn canonical_id<'a>(r: &'a Recording, id: &'a str) -> &'a str {
    let mut current = id;
    for _ in 0..r.speakers.len() {
        if let Some(next) = r
            .speakers
            .iter()
            .find(|s| s.id == current)
            .and_then(|s| s.merged_into.as_deref())
        {
            current = next;
        } else {
            break;
        }
    }
    current
}
pub fn active_speakers(r: &Recording) -> Vec<&Speaker> {
    r.speakers
        .iter()
        .filter(|s| s.merged_into.is_none())
        .collect()
}

/// Convert engine timestamps to byte spans in the *original* text, preserving punctuation.
pub fn word_timings(result: &AsrResult, start_ms: u64, end_ms: u64) -> Vec<WordTiming> {
    let text = result.text.trim();
    let mut cursor = 0;
    let mut words: Vec<WordTiming> = vec![];
    for w in &result.words {
        let word = w.word.trim();
        if word.is_empty()
            || !w.start.is_finite()
            || !w.end.is_finite()
            || w.start < 0.0
            || w.end <= w.start
        {
            continue;
        }
        if let Some(at) = text[cursor..].find(word) {
            let a = cursor + at;
            let b = a + word.len();
            let start = start_ms
                .saturating_add((w.start * 1000.0) as u64)
                .min(end_ms);
            let end = start_ms.saturating_add((w.end * 1000.0) as u64).min(end_ms);
            if start < end {
                if let Some(previous) = words.last_mut().filter(|p| {
                    p.end_byte == a && !text[p.end_byte..b].starts_with(char::is_whitespace)
                }) {
                    previous.end_byte = b;
                    previous.end_ms = previous.end_ms.max(end);
                } else {
                    words.push(WordTiming {
                        start_byte: a,
                        end_byte: b,
                        start_ms: start,
                        end_ms: end,
                    });
                }
            }
            cursor = b;
        }
    }
    // Keep lexical words intact, even when an engine omits a subword timestamp.
    // Untimed letters must not inherit a neighboring voice or split a word in the UI.
    if words.is_empty() {
        return words;
    }
    let mut complete = vec![];
    let mut cursor = 0;
    let mut previous_end = start_ms;
    for part in text.split_whitespace() {
        let a = cursor + text[cursor..].find(part).unwrap();
        let b = a + part.len();
        cursor = b;
        let evidence: Vec<_> = words
            .iter()
            .filter(|w| w.start_byte < b && w.end_byte > a)
            .collect();
        let covered = part.char_indices().all(|(i, c)| {
            !c.is_alphanumeric()
                || evidence
                    .iter()
                    .any(|w| w.start_byte <= a + i && w.end_byte >= a + i + c.len_utf8())
        });
        let start = evidence
            .iter()
            .map(|w| w.start_ms)
            .min()
            .unwrap_or(previous_end);
        let end = if covered {
            evidence.iter().map(|w| w.end_ms).max().unwrap_or(start)
        } else {
            start
        };
        complete.push(WordTiming {
            start_byte: a,
            end_byte: b,
            start_ms: start,
            end_ms: end,
        });
        previous_end = end;
    }
    complete
}

#[derive(Debug, Clone)]
pub struct DisplayTurn {
    pub sequence: u64,
    pub word_start: usize,
    pub word_end: usize,
    pub start_ms: u64,
    pub text: String,
    pub speaker_id: Option<String>,
    pub label: String,
    pub source_start: usize,
    pub source_end: usize,
}
// Index intervals once per render, avoiding a full history scan for every word.
struct TurnIndex<'a> {
    turns: Vec<&'a SpeakerTurn>,
    prefix_end: Vec<u64>,
}
impl<'a> TurnIndex<'a> {
    fn new(r: &'a Recording) -> Self {
        let mut turns: Vec<_> = r.speaker_turns.iter().collect();
        turns.sort_by_key(|t| t.start_ms);
        let mut end = 0;
        let prefix_end = turns
            .iter()
            .map(|t| {
                end = end.max(t.end_ms);
                end
            })
            .collect();
        Self { turns, prefix_end }
    }
    fn overlapping(&self, start: u64, end: u64) -> Vec<&'a SpeakerTurn> {
        let last = self.turns.partition_point(|t| t.start_ms < end);
        let first = self.prefix_end[..last].partition_point(|v| *v <= start);
        self.turns[first..last]
            .iter()
            .copied()
            .filter(|t| t.end_ms > start)
            .collect()
    }
}
fn attribution(
    r: &Recording,
    index: &TurnIndex,
    sequence: u64,
    word: usize,
    start: u64,
    end: u64,
) -> (Option<String>, String) {
    if let Some(a) = r
        .speaker_overrides
        .iter()
        .rev()
        .find(|a| a.sequence == sequence && word >= a.word_start && word < a.word_end)
    {
        return label(r, a.speaker_id.as_deref(), false);
    }
    if end <= start {
        return label(r, None, !r.speaker_finished && end > r.speaker_analyzed_ms);
    }
    let overlapping = index.overlapping(start, end);
    // Multiple simultaneous identities, or weak coverage, must remain unknown.
    let ids: std::collections::BTreeSet<_> = overlapping
        .iter()
        .filter_map(|t| t.speaker_id.as_deref())
        .map(|id| canonical_id(r, id))
        .collect();
    if ids.len() == 1 && !overlapping.iter().any(|t| t.speaker_id.is_none()) {
        let mut covered = 0;
        let mut covered_end = start;
        for t in &overlapping {
            let a = t.start_ms.max(start).max(covered_end);
            let b = t.end_ms.min(end);
            covered += b.saturating_sub(a);
            covered_end = covered_end.max(b);
        }
        let conflict = overlapping.iter().enumerate().any(|(i, a)| {
            overlapping.iter().skip(i + 1).any(|b| {
                a.start_ms.max(b.start_ms) < a.end_ms.min(b.end_ms)
                    && canonical_id(r, a.speaker_id.as_deref().unwrap())
                        != canonical_id(r, b.speaker_id.as_deref().unwrap())
            })
        });
        if !conflict && covered >= end.saturating_sub(start) * 7 / 10 {
            return label(r, ids.first().copied(), false);
        }
    }
    label(r, None, !r.speaker_finished && end > r.speaker_analyzed_ms)
}
fn label(r: &Recording, id: Option<&str>, pending: bool) -> (Option<String>, String) {
    if let Some(id) = id {
        let id = canonical_id(r, id);
        if let Some(s) = r.speakers.iter().find(|s| s.id == id) {
            return (Some(id.into()), s.name.clone());
        }
    }
    (
        None,
        if pending {
            "Identifying speaker"
        } else {
            "Unknown speaker"
        }
        .into(),
    )
}
pub fn display_turns(r: &Recording) -> Vec<DisplayTurn> {
    let mut turns: Vec<DisplayTurn> = vec![];
    let index = TurnIndex::new(r);
    for s in &r.segments {
        let valid: Vec<_> = r
            .word_timings
            .get(&s.sequence)
            .into_iter()
            .flatten()
            .filter(|w| {
                w.start_byte <= w.end_byte
                    && w.end_byte <= s.text.len()
                    && s.text.is_char_boundary(w.start_byte)
                    && s.text.is_char_boundary(w.end_byte)
            })
            .collect();
        if valid.is_empty() {
            let (id, label) = attribution(r, &index, s.sequence, 0, s.start_ms, s.end_ms);
            turns.push(DisplayTurn {
                sequence: s.sequence,
                word_start: 0,
                word_end: 1,
                start_ms: s.start_ms,
                text: s.text.clone(),
                speaker_id: id,
                label,
                source_start: 0,
                source_end: s.text.len(),
            });
            continue;
        }
        let mut begin = 0;
        for (i, w) in valid.iter().enumerate() {
            let end = valid
                .get(i + 1)
                .map(|n| n.start_byte)
                .unwrap_or(s.text.len());
            if end < begin {
                continue;
            }
            let (id, label) = attribution(r, &index, s.sequence, i, w.start_ms, w.end_ms);
            let source_start = begin;
            let text = &s.text[begin..end];
            begin = end;
            if let Some(t) = turns
                .last_mut()
                .filter(|t| t.sequence == s.sequence && t.speaker_id == id && t.label == label)
            {
                t.text.push_str(text);
                t.word_end = i + 1;
                t.source_end = end;
            } else {
                turns.push(DisplayTurn {
                    sequence: s.sequence,
                    word_start: i,
                    word_end: i + 1,
                    start_ms: w.start_ms,
                    text: text.into(),
                    speaker_id: id,
                    label,
                    source_start,
                    source_end: end,
                });
            }
        }
    }
    for t in &mut turns {
        // Keep empty passages at the beginning of their separator, so restoring text retains spaces.
        if !t.text.trim().is_empty() {
            t.source_start += t.text.len() - t.text.trim_start().len();
        }
        t.text = t.text.trim().into();
        t.source_end = t.source_start + t.text.len();
    }
    turns
}
pub fn render(r: &Recording, timed: bool) -> String {
    display_turns(r)
        .iter()
        .map(|t| {
            if timed {
                format!("[{}] {}: {}", timestamp(t.start_ms), t.label, t.text)
            } else {
                format!("{}: {}", t.label, t.text)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Map selections through label changes using the stable transcript source byte offsets.
pub fn map_position(old: &Recording, new: &Recording, timed: bool, position: usize) -> usize {
    let prefix = |t: &DisplayTurn| {
        if timed {
            format!("[{}] {}: ", timestamp(t.start_ms), t.label).len()
        } else {
            t.label.len() + 2
        }
    };
    let mut offset = 0;
    let mut location = None;
    for t in display_turns(old) {
        let body = offset + prefix(&t);
        if position <= body + t.text.len() {
            location = Some((
                t.sequence,
                t.source_start + position.saturating_sub(body).min(t.text.len()),
            ));
            break;
        }
        offset = body + t.text.len() + 2;
    }
    let Some((sequence, source)) = location else {
        return render(new, timed).len();
    };
    let mut offset = 0;
    for t in display_turns(new) {
        let body = offset + prefix(&t);
        if t.sequence == sequence && source >= t.source_start && source <= t.source_end {
            return body + source - t.source_start;
        }
        offset = body + t.text.len() + 2;
    }
    position.min(render(new, timed).len())
}

/// Corrections are allowed only after both workers have finished. Journal first, then memory.
pub fn correct(r: &mut Recording, change: Change, dir: &Path) -> std::io::Result<()> {
    if !r.detect_speakers
        || !r.speaker_finished
        || !matches!(
            r.status,
            RecordingStatus::Completed | RecordingStatus::Incomplete
        )
    {
        return Err(std::io::Error::other(
            "Wait for speaker detection to finish before editing speakers.",
        ));
    }
    let exists = |id: &str| active_speakers(r).iter().any(|s| s.id == id);
    match &change {
        Change::Renamed { speaker_id, name } => {
            if !exists(speaker_id)
                || name.trim().is_empty()
                || name.chars().count() > 80
                || name.contains(['\r', '\n'])
            {
                return Err(std::io::Error::other(
                    "Enter a speaker name of 1-80 characters on one line.",
                ));
            }
        }
        Change::Merged { from, into } => {
            if from == into || !exists(from) || !exists(into) {
                return Err(std::io::Error::other(
                    "Choose two different speakers to merge.",
                ));
            }
        }
        Change::Reassigned(a) => {
            let count = r
                .word_timings
                .get(&a.sequence)
                .map(|w| w.len().max(1))
                .unwrap_or(1);
            if !r.segments.iter().any(|s| s.sequence == a.sequence)
                || a.word_start >= a.word_end
                || a.word_end > count
                || a.speaker_id.as_deref().is_some_and(|id| !exists(id))
            {
                return Err(std::io::Error::other(
                    "The selected turn or speaker is no longer available.",
                ));
            }
        }
        _ => return Err(std::io::Error::other("Invalid speaker correction")),
    }
    let mut next = r.clone();
    apply(&mut next, &change);
    crate::recording::save_correction(
        r,
        next,
        PipelineEvent::Speaker {
            id: r.id.clone(),
            change,
        },
        dir,
    )
}

pub fn normalize(v: &mut [f32]) -> bool {
    let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
    if !n.is_finite() || n < 1e-8 {
        return false;
    }
    for a in v {
        *a /= n;
    }
    true
}
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return -1.0;
    }
    let denom =
        (a.iter().map(|v| v * v).sum::<f32>() * b.iter().map(|v| v * v).sum::<f32>()).sqrt();
    if !denom.is_finite() || denom < 1e-8 {
        return -1.0;
    }
    a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>() / denom
}

/// Conservative recording-local matching. A gray zone avoids creating duplicate identities.
pub fn match_profile(
    profiles: &mut Vec<Speaker>,
    embedding: &[f32],
    create: bool,
) -> Option<String> {
    // Keep merged voice exemplars useful on continuation, comparing distinct identities.
    let mut identities = std::collections::BTreeMap::<String, (usize, f32)>::new();
    for (i, s) in profiles.iter().enumerate() {
        let mut id = s.id.as_str();
        for _ in 0..profiles.len() {
            if let Some(next) = profiles
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.merged_into.as_deref())
            {
                id = next;
            } else {
                break;
            }
        }
        let score = cosine(&s.embedding, embedding);
        let best = identities.entry(id.to_string()).or_insert((i, score));
        if score > best.1 {
            *best = (i, score);
        }
    }
    let mut ranked: Vec<_> = identities
        .into_iter()
        .map(|(id, (i, score))| (i, score, id))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    if let Some((i, score, id)) = ranked.first() {
        let (i, score) = (*i, *score);
        let next = ranked.get(1).map(|p| p.1).unwrap_or(-1.0);
        if score >= 0.6 && score - next >= 0.08 {
            let s = &mut profiles[i];
            if create {
                let n = s.observations.min(50) as f32;
                for (a, b) in s.embedding.iter_mut().zip(embedding) {
                    *a = (*a * n + b) / (n + 1.0);
                }
                normalize(&mut s.embedding);
                s.observations = s.observations.saturating_add(1);
            }
            return Some(id.clone());
        }
        if !create || score > 0.4 {
            return None;
        }
    } else if !create {
        return None;
    }
    let mut embedding = embedding.to_vec();
    if !normalize(&mut embedding) {
        return None;
    }
    let n = profiles.len() + 1;
    let id = format!("speaker-{n}");
    profiles.push(Speaker {
        id: id.clone(),
        name: format!("Speaker {n}"),
        embedding,
        observations: 1,
        merged_into: None,
    });
    Some(id)
}

#[cfg(test)]
mod tests;
